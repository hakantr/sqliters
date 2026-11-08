//! SQL-to-VDBE bytecode compiler (`select.c`, `where.c`, `expr.c`, `insert.c`) and
//! `EXPLAIN` / `EXPLAIN QUERY PLAN` planner.

use sqliters_syntax::{
    BinaryOp, BindParam, CurrentTimeKind, DeleteStmt, Expr, FromClause, FunctionArgs, IndexHint,
    InsertSource, InsertStmt, LikeOp, LimitClause, OrderByTerm, ResultColumn, SelectCore,
    SelectStmt, Stmt, TableOrSubquery, UnaryOp, UpdateStmt,
};
use sqliters_types::{
    Collation, ErrorCode, KeySortOrder, Result, SqliteError, TypeAffinity, Value,
};
use sqliters_vdbe::{
    AggregateKind, Instruction, Opcode, P4, VdbeProgram, affinity_to_p5, sqlite_compare_affinity,
    validate_function_call,
};

use crate::schema::{IndexSchema, Schema, TableSchema};

/// Chosen B-Tree access strategy for a single table (`where.c`).
#[derive(Debug, Clone)]
pub enum AccessPlan {
    FullScan,
    RowidSeek {
        key_expr: Expr,
    },
    IndexSeek {
        index: IndexSchema,
        prefix_exprs: Vec<Expr>,
    },
}

impl AccessPlan {
    pub fn explain_detail(&self, display_name: &str) -> String {
        match self {
            Self::FullScan => format!("SCAN {display_name}"),
            Self::RowidSeek { .. } => {
                format!("SEARCH {display_name} USING INTEGER PRIMARY KEY (rowid=?)")
            }
            Self::IndexSeek {
                index,
                prefix_exprs,
            } => {
                let cols: Vec<String> = index
                    .columns
                    .iter()
                    .take(prefix_exprs.len())
                    .map(|c| format!("{}=?", c.name))
                    .collect();
                format!(
                    "SEARCH {display_name} USING INDEX {} ({})",
                    index.name,
                    cols.join(" AND ")
                )
            }
        }
    }
}

/// Inspects a `WHERE` clause (`selection`) for equality predicates matching `rowid` / `INTEGER PRIMARY KEY`
/// or a leading prefix of any secondary index on `tbl`.
pub fn choose_access_plan(
    tbl: &TableSchema,
    alias: Option<&str>,
    selection: Option<&Expr>,
    schema: &Schema,
) -> AccessPlan {
    let Some(where_expr) = selection else {
        return AccessPlan::FullScan;
    };

    let mut conjuncts = Vec::new();
    collect_and_conjuncts(where_expr, &mut conjuncts);

    // 1. Check for `rowid = <const_or_param>` or `ipk_col = <const_or_param>` (O(log N) primary B+Tree seek!)
    for conj in &conjuncts {
        if let Some((col_name, rhs)) = extract_table_col_eq(conj, tbl, alias) {
            let is_rowid = tbl.is_implicit_rowid_name(&col_name)
                || tbl
                    .rowid_alias_col
                    .is_some_and(|ipk| tbl.columns[ipk].name.eq_ignore_ascii_case(&col_name));
            if is_rowid {
                return AccessPlan::RowidSeek { key_expr: rhs };
            }
        }
    }

    // 2. Check secondary indexes (and primary Index B-Tree for `WITHOUT ROWID` tables),
    //    preferring UNIQUE indexes and longer matched key prefixes!
    let mut indexes = schema.indexes_for_table(&tbl.name);
    if let Some(pk_idx) = tbl.without_rowid_pk_as_index() {
        indexes.insert(0, pk_idx);
    }
    let mut best: Option<(usize, bool, IndexSchema, Vec<Expr>)> = None;

    for idx in indexes {
        if idx.where_clause.is_some() {
            // Partial indexes require proving the partial predicate; keep safe full scan or post-filter
            continue;
        }
        let mut prefix_exprs = Vec::new();
        for ic in &idx.columns {
            let Some(ci) = ic.col_idx else {
                break;
            };
            // Only use equality seek if index column collation matches default/column collation
            let tbl_col_coll = tbl.columns[ci].collation;
            if ic.sort_order.collation != tbl_col_coll {
                break;
            }
            let matched = conjuncts.iter().find_map(|conj| {
                let (cname, rhs) = extract_table_col_eq(conj, tbl, alias)?;
                if cname.eq_ignore_ascii_case(&ic.name) {
                    let idx_aff = tbl.columns[ci].affinity;
                    let rhs_aff = compiler_expr_affinity(&rhs, Some(tbl), alias);
                    let comp_aff = sqlite_compare_affinity(Some(idx_aff), rhs_aff);
                    if index_affinity_ok(comp_aff, idx_aff) {
                        Some(rhs)
                    } else {
                        None
                    }
                } else {
                    None
                }
            });
            if let Some(rhs_expr) = matched {
                prefix_exprs.push(rhs_expr);
            } else {
                break;
            }
        }

        if !prefix_exprs.is_empty() {
            let len = prefix_exprs.len();
            let is_uniq = idx.unique && len == idx.columns.len();
            let better = match &best {
                None => true,
                Some((best_len, best_uniq, _, _)) => {
                    (is_uniq && !*best_uniq) || (is_uniq == *best_uniq && len > *best_len)
                }
            };
            if better {
                best = Some((len, is_uniq, idx, prefix_exprs));
            }
        }
    }

    if let Some((_, _, index, prefix_exprs)) = best {
        return AccessPlan::IndexSeek {
            index,
            prefix_exprs,
        };
    }

    AccessPlan::FullScan
}

fn collect_and_conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = expr
    {
        collect_and_conjuncts(left, out);
        collect_and_conjuncts(right, out);
    } else {
        out.push(expr);
    }
}

fn extract_table_col_eq(
    expr: &Expr,
    tbl: &TableSchema,
    alias: Option<&str>,
) -> Option<(String, Expr)> {
    let Expr::Binary {
        left,
        op: BinaryOp::Eq,
        right,
    } = expr
    else {
        return None;
    };

    if let Some(cname) = match_single_table_col(left, tbl, alias)
        && expr_is_table_independent(right)
    {
        return Some((cname, (**right).clone()));
    }
    if let Some(cname) = match_single_table_col(right, tbl, alias)
        && expr_is_table_independent(left)
    {
        return Some((cname, (**left).clone()));
    }
    None
}

fn match_single_table_col(expr: &Expr, tbl: &TableSchema, alias: Option<&str>) -> Option<String> {
    let Expr::Column { table, column } = expr else {
        return None;
    };
    if let Some(qual) = table {
        let expect = alias.unwrap_or(&tbl.name);
        if !qual.eq_ignore_ascii_case(expect) {
            return None;
        }
    }
    if tbl.find_column(column).is_some() || tbl.is_implicit_rowid_name(column) {
        Some(column.clone())
    } else {
        None
    }
}

pub fn expr_is_table_independent(expr: &Expr) -> bool {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_) => true,
        Expr::Column { .. } | Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            false
        }
        Expr::Unary { expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::IsNull { expr, .. }
        | Expr::Cast { expr, .. } => expr_is_table_independent(expr),
        Expr::Binary { left, right, .. } => {
            expr_is_table_independent(left) && expr_is_table_independent(right)
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expr_is_table_independent(expr)
                && expr_is_table_independent(low)
                && expr_is_table_independent(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_is_table_independent(expr) && list.iter().all(expr_is_table_independent)
        }
        Expr::Like {
            expr,
            pattern,
            escape,
            ..
        } => {
            expr_is_table_independent(expr)
                && expr_is_table_independent(pattern)
                && escape.as_deref().is_none_or(expr_is_table_independent)
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_none_or(expr_is_table_independent)
                && branches
                    .iter()
                    .all(|(w, t)| expr_is_table_independent(w) && expr_is_table_independent(t))
                && else_expr.as_deref().is_none_or(expr_is_table_independent)
        }
        Expr::RowValue(elems) => elems.iter().all(expr_is_table_independent),
        Expr::FunctionCall {
            name, args, filter, ..
        } => {
            if expr_contains_aggregate(expr) || filter.is_some() {
                return false;
            }
            let _ = name;
            match args {
                FunctionArgs::Star => false,
                FunctionArgs::Exprs(es) => es.iter().all(expr_is_table_independent),
            }
        }
    }
}

pub fn expr_contains_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall { name, args, .. } => {
            let is_agg = match args {
                FunctionArgs::Star => AggregateKind::from_name(name, true).is_some(),
                FunctionArgs::Exprs(es) => {
                    let upper = name.to_ascii_uppercase();
                    if (upper == "MIN" || upper == "MAX") && es.len() != 1 {
                        false
                    } else if upper == "COUNT" && es.is_empty() {
                        true
                    } else {
                        AggregateKind::from_name(name, false).is_some()
                    }
                }
            };
            if is_agg {
                return true;
            }
            match args {
                FunctionArgs::Star => false,
                FunctionArgs::Exprs(es) => es.iter().any(expr_contains_aggregate),
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::IsNull { expr, .. }
        | Expr::Cast { expr, .. } => expr_contains_aggregate(expr),
        Expr::Binary { left, right, .. } => {
            expr_contains_aggregate(left) || expr_contains_aggregate(right)
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expr_contains_aggregate(expr)
                || expr_contains_aggregate(low)
                || expr_contains_aggregate(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_contains_aggregate(expr) || list.iter().any(expr_contains_aggregate)
        }
        Expr::Like {
            expr,
            pattern,
            escape,
            ..
        } => {
            expr_contains_aggregate(expr)
                || expr_contains_aggregate(pattern)
                || escape.as_deref().is_some_and(expr_contains_aggregate)
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_some_and(expr_contains_aggregate)
                || branches
                    .iter()
                    .any(|(w, t)| expr_contains_aggregate(w) || expr_contains_aggregate(t))
                || else_expr.as_deref().is_some_and(expr_contains_aggregate)
        }
        Expr::RowValue(elems) => elems.iter().any(expr_contains_aggregate),
        _ => false,
    }
}

pub fn expr_contains_subquery(expr: &Expr) -> bool {
    match expr {
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => true,
        Expr::RowValue(_) => true, // Force relational engine evaluation for row-value tuples
        Expr::Unary { expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::IsNull { expr, .. }
        | Expr::Cast { expr, .. } => expr_contains_subquery(expr),
        Expr::Binary { left, right, .. } => {
            expr_contains_subquery(left) || expr_contains_subquery(right)
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expr_contains_subquery(expr)
                || expr_contains_subquery(low)
                || expr_contains_subquery(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_contains_subquery(expr) || list.iter().any(expr_contains_subquery)
        }
        Expr::Like {
            expr,
            pattern,
            escape,
            ..
        } => {
            expr_contains_subquery(expr)
                || expr_contains_subquery(pattern)
                || escape.as_deref().is_some_and(expr_contains_subquery)
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_some_and(expr_contains_subquery)
                || branches
                    .iter()
                    .any(|(w, t)| expr_contains_subquery(w) || expr_contains_subquery(t))
                || else_expr.as_deref().is_some_and(expr_contains_subquery)
        }
        Expr::FunctionCall { args, filter, .. } => {
            filter.as_deref().is_some_and(expr_contains_subquery)
                || match args {
                    FunctionArgs::Star => false,
                    FunctionArgs::Exprs(es) => es.iter().any(expr_contains_subquery),
                }
        }
        _ => false,
    }
}

pub fn expr_column_label(expr: &Expr) -> String {
    match expr {
        Expr::Column {
            table: None,
            column,
        } => column.clone(),
        Expr::Column {
            table: Some(t),
            column,
        } => {
            let _ = t;
            column.clone()
        }
        Expr::Null => "NULL".to_owned(),
        Expr::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
        Expr::Integer(i) => i.to_string(),
        Expr::Float(f) => f.to_string(),
        Expr::String(s) => format!("'{s}'"),
        Expr::Blob(_) => "x''".to_owned(),
        Expr::CurrentTimeKw(CurrentTimeKind::Date) => "CURRENT_DATE".to_owned(),
        Expr::CurrentTimeKw(CurrentTimeKind::Time) => "CURRENT_TIME".to_owned(),
        Expr::CurrentTimeKw(CurrentTimeKind::Timestamp) => "CURRENT_TIMESTAMP".to_owned(),
        Expr::Bind(BindParam::Anonymous(_)) => "?".to_owned(),
        Expr::Bind(BindParam::Numbered(n)) => format!("?{n}"),
        Expr::Bind(BindParam::Named(s, _)) => s.clone(),
        Expr::FunctionCall { name, args, .. } => match args {
            FunctionArgs::Star => format!("{name}(*)"),
            FunctionArgs::Exprs(es) => {
                let inner: Vec<String> = es.iter().map(expr_column_label).collect();
                format!("{name}({})", inner.join(","))
            }
        },
        Expr::Cast { expr, type_name } => {
            format!("CAST({} AS {type_name})", expr_column_label(expr))
        }
        Expr::Unary { op, expr } => {
            let sym = match op {
                UnaryOp::Plus => "+",
                UnaryOp::Negate => "-",
                UnaryOp::BitNot => "~",
                UnaryOp::Not => "NOT ",
            };
            format!("{sym}{}", expr_column_label(expr))
        }
        Expr::Binary { left, op, right } => {
            let sym = match op {
                BinaryOp::Concat => "||",
                BinaryOp::Multiply => "*",
                BinaryOp::Divide => "/",
                BinaryOp::Remainder => "%",
                BinaryOp::Add => "+",
                BinaryOp::Subtract => "-",
                BinaryOp::ShiftLeft => "<<",
                BinaryOp::ShiftRight => ">>",
                BinaryOp::BitAnd => "&",
                BinaryOp::BitOr => "|",
                BinaryOp::Lt => "<",
                BinaryOp::Le => "<=",
                BinaryOp::Gt => ">",
                BinaryOp::Ge => ">=",
                BinaryOp::Eq => "=",
                BinaryOp::Ne => "!=",
                BinaryOp::Is => "IS",
                BinaryOp::IsNot => "IS NOT",
                BinaryOp::And => "AND",
                BinaryOp::Or => "OR",
            };
            format!(
                "{} {sym} {}",
                expr_column_label(left),
                expr_column_label(right)
            )
        }
        _ => "?column?".to_owned(),
    }
}

// ============================================================================
// Single-Table & Tableless SELECT -> VDBE Compiler
// ============================================================================

struct TableBinding<'a> {
    tbl: &'a TableSchema,
    alias: Option<&'a str>,
    cursor: i32,
}

/// Attempts to compile `stmt` into a pure [`VdbeProgram`] for direct step-by-step execution
/// on [`sqliters_vdbe::VdbeVm`]. Returns `Ok(None)` if the query uses multi-table joins, CTEs,
/// compound set operations, aggregates/`GROUP BY`, `DISTINCT`, or subqueries (handled by `engine`).
pub fn try_compile_select(stmt: &SelectStmt, schema: &Schema) -> Result<Option<VdbeProgram>> {
    if !stmt.ctes.is_empty() || !stmt.body.compounds.is_empty() {
        return Ok(None);
    }
    let SelectCore::Select {
        distinct,
        projection,
        from,
        selection,
        group_by,
        having,
    } = &stmt.body.head
    else {
        return Ok(None);
    };

    if *distinct || !group_by.is_empty() || having.is_some() {
        return Ok(None);
    }

    // Check for aggregates or subqueries in projection / selection / order_by / limit
    for rc in projection {
        if let ResultColumn::Expr { expr, .. } = rc
            && (expr_contains_aggregate(expr) || expr_contains_subquery(expr))
        {
            return Ok(None);
        }
    }
    if let Some(w) = selection
        && (expr_contains_aggregate(w) || expr_contains_subquery(w))
    {
        return Ok(None);
    }
    for ob in &stmt.order_by {
        if expr_contains_aggregate(&ob.expr) || expr_contains_subquery(&ob.expr) {
            return Ok(None);
        }
    }
    if let Some(lim) = &stmt.limit
        && (expr_contains_subquery(&lim.limit)
            || lim.offset.as_ref().is_some_and(expr_contains_subquery))
    {
        return Ok(None);
    }

    match from {
        None => {
            if !stmt.order_by.is_empty() {
                // Trivial 1-row tableless query; ORDER BY is a no-op if valid
            }
            let prog =
                compile_tableless_select(projection, selection.as_ref(), stmt.limit.as_ref())?;
            Ok(Some(prog))
        }
        Some(FromClause { head, joins }) if joins.is_empty() => {
            let TableOrSubquery::Table {
                name,
                alias,
                indexed_by,
            } = head
            else {
                return Ok(None);
            };
            if matches!(indexed_by, Some(IndexHint::IndexedBy(_))) {
                return Ok(None);
            }
            let Some(tbl) = schema.find_table(name) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {name}"),
                ));
            };
            if tbl.columns.iter().any(|c| c.generated.is_some()) {
                return Ok(None);
            }
            compile_single_table_select(
                SingleTableSource {
                    tbl: &tbl,
                    alias: alias.as_deref(),
                    indexed_by: indexed_by.as_ref(),
                },
                projection,
                selection.as_ref(),
                &stmt.order_by,
                stmt.limit.as_ref(),
                schema,
            )
        }
        Some(_) => Ok(None),
    }
}

fn compile_tableless_select(
    projection: &[ResultColumn],
    selection: Option<&Expr>,
    limit: Option<&LimitClause>,
) -> Result<VdbeProgram> {
    let mut prog = VdbeProgram::new();
    let init_addr = prog.emit(Instruction::new(Opcode::Init, 0, 0, 0));

    let mut proj_exprs = Vec::with_capacity(projection.len());
    let mut col_names = Vec::with_capacity(projection.len());
    for rc in projection {
        match rc {
            ResultColumn::Star | ResultColumn::TableStar(_) => {
                return Err(SqliteError::new(ErrorCode::Error, "no tables specified"));
            }
            ResultColumn::Expr { expr, alias } => {
                let name = alias.clone().unwrap_or_else(|| expr_column_label(expr));
                col_names.push(name);
                proj_exprs.push(expr.clone());
            }
        }
    }
    prog.column_names = col_names;

    let mut jumps_to_halt = Vec::new();

    if let Some(lim) = limit {
        let lim_reg = prog.alloc_register();
        compile_expr(&mut prog, &lim.limit, None, lim_reg)?;
        let zero_reg = prog.alloc_register();
        prog.emit(Instruction::new(Opcode::Integer, 0, zero_reg, 0));
        let le_reg = prog.alloc_register();
        prog.emit(Instruction::new(Opcode::Le, lim_reg, zero_reg, le_reg));
        jumps_to_halt.push(prog.emit(Instruction::new(Opcode::JumpIfTrue, le_reg, 0, 0)));

        if let Some(off_expr) = &lim.offset {
            let off_reg = prog.alloc_register();
            compile_expr(&mut prog, off_expr, None, off_reg)?;
            let gt_reg = prog.alloc_register();
            prog.emit(Instruction::new(Opcode::Gt, off_reg, zero_reg, gt_reg));
            jumps_to_halt.push(prog.emit(Instruction::new(Opcode::JumpIfTrue, gt_reg, 0, 0)));
        }
    }

    if let Some(w) = selection {
        let cond_reg = prog.alloc_register();
        compile_expr(&mut prog, w, None, cond_reg)?;
        jumps_to_halt.push(prog.emit(Instruction::new(Opcode::JumpIfFalse, cond_reg, 0, 1)));
    }

    let out_base = prog.alloc_registers(proj_exprs.len());
    for (i, e) in proj_exprs.iter().enumerate() {
        compile_expr(&mut prog, e, None, out_base + i as i32)?;
    }
    prog.emit(Instruction::new(
        Opcode::ResultRow,
        out_base,
        proj_exprs.len() as i32,
        0,
    ));

    let halt_addr = prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    for j in jumps_to_halt {
        prog.patch_p2(j, halt_addr);
    }
    prog.patch_p2(init_addr, 1);
    Ok(prog)
}

struct SingleTableSource<'a> {
    tbl: &'a TableSchema,
    alias: Option<&'a str>,
    indexed_by: Option<&'a IndexHint>,
}

fn compile_single_table_select(
    source: SingleTableSource<'_>,
    projection: &[ResultColumn],
    selection: Option<&Expr>,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    schema: &Schema,
) -> Result<Option<VdbeProgram>> {
    let SingleTableSource {
        tbl,
        alias,
        indexed_by,
    } = source;
    let mut prog = VdbeProgram::new();
    let init_addr = prog.emit(Instruction::new(Opcode::Init, 0, 0, 0));

    // Expand projection (`*`, `t.*`, expressions)
    let table_ref_name = alias.unwrap_or(&tbl.name);
    let mut expanded: Vec<(Expr, String, Option<String>)> = Vec::new();
    for rc in projection {
        match rc {
            ResultColumn::Star => {
                for c in &tbl.columns {
                    expanded.push((
                        Expr::Column {
                            table: None,
                            column: c.name.clone(),
                        },
                        c.name.clone(),
                        None,
                    ));
                }
            }
            ResultColumn::TableStar(tname) => {
                if !tname.eq_ignore_ascii_case(table_ref_name) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such table: {tname}"),
                    ));
                }
                for c in &tbl.columns {
                    expanded.push((
                        Expr::Column {
                            table: Some(tname.clone()),
                            column: c.name.clone(),
                        },
                        c.name.clone(),
                        None,
                    ));
                }
            }
            ResultColumn::Expr { expr, alias: al } => {
                let label = al.clone().unwrap_or_else(|| expr_column_label(expr));
                expanded.push((expr.clone(), label, al.clone()));
            }
        }
    }
    prog.column_names = expanded.iter().map(|(_, name, _)| name.clone()).collect();

    // Begin read transaction & open table cursor
    let tbl_cursor = prog.alloc_cursor();
    let binding = TableBinding {
        tbl,
        alias,
        cursor: tbl_cursor,
    };

    let body_start = prog.current_addr();
    prog.emit(Instruction::new(Opcode::Transaction, 0, 0, 0));
    if tbl.without_rowid {
        prog.emit(
            Instruction::new(Opcode::OpenReadIndex, tbl_cursor, tbl.root_pgno as i32, 0)
                .with_p4(P4::KeyInfo(tbl.without_rowid_pk_orders()))
                .with_comment(tbl.name.clone()),
        );
    } else {
        prog.emit(
            Instruction::new(
                Opcode::OpenRead,
                tbl_cursor,
                tbl.root_pgno as i32,
                tbl.columns.len() as i32,
            )
            .with_comment(tbl.name.clone()),
        );
    }

    // Optional LIMIT / OFFSET registers initialized before loop
    let limit_regs = if let Some(lim) = limit {
        let rem_reg = prog.alloc_register();
        compile_expr(&mut prog, &lim.limit, None, rem_reg)?;
        let off_reg = if let Some(off_e) = &lim.offset {
            let r = prog.alloc_register();
            compile_expr(&mut prog, off_e, None, r)?;
            Some(r)
        } else {
            None
        };
        let one_reg = prog.alloc_register();
        prog.emit(Instruction::new(Opcode::Integer, 1, one_reg, 0));
        let zero_reg = prog.alloc_register();
        prog.emit(Instruction::new(Opcode::Integer, 0, zero_reg, 0));
        Some((rem_reg, off_reg, one_reg, zero_reg))
    } else {
        None
    };

    let plan = match indexed_by {
        Some(IndexHint::NotIndexed) => AccessPlan::FullScan,
        None => choose_access_plan(tbl, alias, selection, schema),
        Some(IndexHint::IndexedBy(_)) => return Ok(None),
    };

    // Optional Sorter setup for ORDER BY (elided when a natural ascending rowid scan or
    // `WITHOUT ROWID` primary B-Tree scan already satisfies `ORDER BY`!)
    let sorter_setup = if !order_by.is_empty() {
        let mut sort_sources = Vec::with_capacity(order_by.len());
        let mut sort_orders = Vec::with_capacity(order_by.len());
        let mut resolved_exprs = Vec::with_capacity(order_by.len());
        for ob in order_by {
            let Some((src, coll_ref_expr)) = resolve_order_by_source(&ob.expr, &expanded, tbl)
            else {
                return Ok(None);
            };
            let coll = if let Expr::Collate { collation, .. } = &ob.expr {
                Collation::from_name(collation).unwrap_or_default()
            } else {
                infer_expr_collation(&coll_ref_expr, tbl)
            };
            let desc = ob.desc;
            let nulls_last = ob.nulls_last.unwrap_or(desc);
            resolved_exprs.push(coll_ref_expr);
            sort_sources.push(src);
            sort_orders.push(KeySortOrder {
                desc,
                nulls_last,
                collation: coll,
            });
        }

        let natural_rowid_order = order_by.len() == 1
            && !order_by[0].desc
            && matches!(plan, AccessPlan::FullScan | AccessPlan::RowidSeek { .. })
            && resolved_exprs
                .first()
                .is_some_and(|e| is_table_rowid_column_expr(e, tbl, alias));

        let natural_without_rowid_order = tbl.without_rowid
            && matches!(plan, AccessPlan::FullScan)
            && order_by.len() <= tbl.without_rowid_pk_cols.len()
            && resolved_exprs
                .iter()
                .zip(sort_orders.iter())
                .zip(tbl.without_rowid_pk_cols.iter())
                .all(|((re, so), pk_ic)| {
                    *so == pk_ic.sort_order
                        && pk_ic
                            .col_idx
                            .is_some_and(|pk_ci| is_exact_table_column_expr(re, tbl, alias, pk_ci))
                });

        if natural_rowid_order || natural_without_rowid_order {
            None
        } else {
            let s_cursor = prog.alloc_cursor();
            prog.emit(
                Instruction::new(
                    Opcode::SorterOpen,
                    s_cursor,
                    (sort_sources.len() + expanded.len()) as i32,
                    0,
                )
                .with_p4(P4::KeyInfo(sort_orders)),
            );
            Some((s_cursor, sort_sources))
        }
    } else {
        None
    };

    let mut jumps_after_scan = Vec::new();
    let mut jumps_next_row = Vec::new();
    let mut idx_cursor_opt = None;
    let mut seek_advance_cursor = None;

    // Early check: if LIMIT == 0 and no sorter needed, jump straight past scan
    if sorter_setup.is_none()
        && let Some((rem_reg, _, _, zero_reg)) = limit_regs
    {
        let cmp_r = prog.alloc_register();
        prog.emit(Instruction::new(Opcode::Eq, rem_reg, zero_reg, cmp_r));
        jumps_after_scan.push(prog.emit(Instruction::new(Opcode::JumpIfTrue, cmp_r, 0, 0)));
    }

    let loop_top = match &plan {
        AccessPlan::FullScan => {
            let rewind = prog.emit(Instruction::new(Opcode::Rewind, tbl_cursor, 0, 0));
            jumps_after_scan.push(rewind);
            prog.current_addr()
        }
        AccessPlan::RowidSeek { key_expr } => {
            let key_reg = prog.alloc_register();
            compile_expr(&mut prog, key_expr, None, key_reg)?;
            let seek = prog.emit(Instruction::new(Opcode::SeekRowid, tbl_cursor, 0, key_reg));
            jumps_after_scan.push(seek);
            prog.current_addr()
        }
        AccessPlan::IndexSeek {
            index,
            prefix_exprs,
        } => {
            let is_primary_without_rowid =
                tbl.without_rowid && index.origin == "pk" && index.root_pgno == tbl.root_pgno;

            if is_primary_without_rowid {
                let key_base = prog.alloc_registers(prefix_exprs.len());
                for (i, ke) in prefix_exprs.iter().enumerate() {
                    let r = key_base + i as i32;
                    compile_expr(&mut prog, ke, None, r)?;
                    jumps_after_scan.push(prog.emit(Instruction::new(Opcode::JumpIfNull, r, 0, 0)));
                    let col_aff = index.columns[i]
                        .col_idx
                        .map_or(TypeAffinity::Blob, |ci| tbl.columns[ci].affinity);
                    prog.emit(
                        Instruction::new(Opcode::Affinity, r, 1, 0)
                            .with_p4(P4::Affinity(vec![col_aff])),
                    );
                }
                let seek_ge = prog.emit(
                    Instruction::new(Opcode::SeekIndexEqual, tbl_cursor, 0, key_base)
                        .with_p5(prefix_exprs.len() as u16),
                );
                jumps_after_scan.push(seek_ge);
                seek_advance_cursor = Some(tbl_cursor);
                prog.current_addr()
            } else {
                let mut pk_slots_in_idx = Vec::new();
                if tbl.without_rowid {
                    for pk_col in &tbl.without_rowid_pk_cols {
                        let Some(slot) = index.without_rowid_pk_slot_in_index(pk_col) else {
                            return Ok(None);
                        };
                        pk_slots_in_idx.push(slot);
                    }
                }

                let icur = prog.alloc_cursor();
                idx_cursor_opt = Some(icur);
                seek_advance_cursor = Some(icur);
                prog.emit(
                    Instruction::new(Opcode::OpenReadIndex, icur, index.root_pgno as i32, 0)
                        .with_p4(P4::KeyInfo(index.key_sort_orders()))
                        .with_comment(index.name.clone()),
                );
                let key_base = prog.alloc_registers(prefix_exprs.len());
                for (i, ke) in prefix_exprs.iter().enumerate() {
                    let r = key_base + i as i32;
                    compile_expr(&mut prog, ke, None, r)?;
                    jumps_after_scan.push(prog.emit(Instruction::new(Opcode::JumpIfNull, r, 0, 0)));
                    let col_aff = index.columns[i]
                        .col_idx
                        .map_or(TypeAffinity::Blob, |ci| tbl.columns[ci].affinity);
                    prog.emit(
                        Instruction::new(Opcode::Affinity, r, 1, 0)
                            .with_p4(P4::Affinity(vec![col_aff])),
                    );
                }
                let seek_ge = prog.emit(
                    Instruction::new(Opcode::SeekIndexEqual, icur, 0, key_base)
                        .with_p5(prefix_exprs.len() as u16),
                );
                jumps_after_scan.push(seek_ge);

                let top = prog.current_addr();
                if tbl.without_rowid {
                    let pk_base = prog.alloc_registers(pk_slots_in_idx.len());
                    for (k, &slot) in pk_slots_in_idx.iter().enumerate() {
                        prog.emit(Instruction::new(
                            Opcode::Column,
                            icur,
                            slot as i32,
                            pk_base + k as i32,
                        ));
                    }
                    let seek_tbl = prog.emit(
                        Instruction::new(Opcode::SeekIndexEqual, tbl_cursor, 0, pk_base)
                            .with_p5(pk_slots_in_idx.len() as u16),
                    );
                    jumps_next_row.push(seek_tbl);
                } else {
                    let rid_reg = prog.alloc_register();
                    prog.emit(Instruction::new(Opcode::Rowid, icur, rid_reg, 0));
                    let seek_tbl =
                        prog.emit(Instruction::new(Opcode::SeekRowid, tbl_cursor, 0, rid_reg));
                    jumps_next_row.push(seek_tbl);
                }
                top
            }
        }
    };

    // Evaluate full WHERE filter (also handles residual conjuncts after index/rowid seek!)
    if let Some(w) = selection {
        let cond_reg = prog.alloc_register();
        compile_expr(&mut prog, w, Some(&binding), cond_reg)?;
        jumps_next_row.push(prog.emit(Instruction::new(Opcode::JumpIfFalse, cond_reg, 0, 1)));
    }

    if let Some((s_cursor, ref sort_sources)) = sorter_setup {
        // Compute `proj_cols` first so `ORDER BY` terms matching result columns reuse them once!
        let total_cols = sort_sources.len() + expanded.len();
        let row_base = prog.alloc_registers(total_cols);
        for (j, (pe, _, _)) in expanded.iter().enumerate() {
            compile_expr(
                &mut prog,
                pe,
                Some(&binding),
                row_base + (sort_sources.len() + j) as i32,
            )?;
        }
        for (i, src) in sort_sources.iter().enumerate() {
            match src {
                OrderBySource::ProjCol(pidx) => {
                    prog.emit(Instruction::new(
                        Opcode::Copy,
                        row_base + (sort_sources.len() + *pidx) as i32,
                        row_base + i as i32,
                        0,
                    ));
                }
                OrderBySource::Expr(se) => {
                    compile_expr(&mut prog, se, Some(&binding), row_base + i as i32)?;
                }
            }
        }
        prog.emit(Instruction::new(
            Opcode::SorterInsert,
            s_cursor,
            row_base,
            total_cols as i32,
        ));
    } else {
        // Streaming OFFSET / LIMIT check before ResultRow
        if let Some((rem_reg, off_reg_opt, one_reg, zero_reg)) = limit_regs {
            if let Some(off_reg) = off_reg_opt {
                let gt_r = prog.alloc_register();
                prog.emit(Instruction::new(Opcode::Gt, off_reg, zero_reg, gt_r));
                let skip_dec = prog.emit(Instruction::new(Opcode::JumpIfFalse, gt_r, 0, 1));
                prog.emit(Instruction::new(
                    Opcode::Subtract,
                    off_reg,
                    one_reg,
                    off_reg,
                ));
                jumps_next_row.push(prog.emit(goto_placeholder()));
                let after_off = prog.current_addr();
                prog.patch_p2(skip_dec, after_off);
            }
            // If rem_reg < 0, LIMIT is negative (= unlimited in SQLite!)
            let lt_r = prog.alloc_register();
            prog.emit(Instruction::new(Opcode::Lt, rem_reg, zero_reg, lt_r));
            let skip_lim = prog.emit(Instruction::new(Opcode::JumpIfTrue, lt_r, 0, 0));
            let le_r = prog.alloc_register();
            prog.emit(Instruction::new(Opcode::Le, rem_reg, zero_reg, le_r));
            jumps_after_scan.push(prog.emit(Instruction::new(Opcode::JumpIfTrue, le_r, 0, 0)));
            prog.emit(Instruction::new(
                Opcode::Subtract,
                rem_reg,
                one_reg,
                rem_reg,
            ));
            let after_lim = prog.current_addr();
            prog.patch_p2(skip_lim, after_lim);
        }

        let out_base = prog.alloc_registers(expanded.len());
        for (j, (pe, _, _)) in expanded.iter().enumerate() {
            compile_expr(&mut prog, pe, Some(&binding), out_base + j as i32)?;
        }
        prog.emit(Instruction::new(
            Opcode::ResultRow,
            out_base,
            expanded.len() as i32,
            0,
        ));
    }

    // Advance loop (`Next` / `IndexMatchNext` / single-shot `RowidSeek`)
    let next_addr = prog.current_addr();
    for j in jumps_next_row {
        prog.patch_p2(j, next_addr);
    }

    match &plan {
        AccessPlan::FullScan => {
            prog.emit(Instruction::new(
                Opcode::Next,
                tbl_cursor,
                loop_top as i32,
                0,
            ));
        }
        AccessPlan::RowidSeek { .. } => {
            // Single point lookup: falls straight through!
        }
        AccessPlan::IndexSeek { .. } => {
            let adv_cur = seek_advance_cursor.expect("index seek cursor allocated");
            prog.emit(Instruction::new(
                Opcode::IndexMatchNext,
                adv_cur,
                loop_top as i32,
                0,
            ));
        }
    }

    let after_scan = prog.current_addr();
    for j in jumps_after_scan {
        prog.patch_p2(j, after_scan);
    }

    if let Some(icur) = idx_cursor_opt {
        prog.emit(Instruction::new(Opcode::Close, icur, 0, 0));
    }
    prog.emit(Instruction::new(Opcode::Close, tbl_cursor, 0, 0));

    // Drain Sorter if ORDER BY was present
    if let Some((s_cursor, sort_exprs)) = sorter_setup {
        let mut jumps_done = Vec::new();
        let sort_ins = prog.emit(Instruction::new(Opcode::SorterSort, s_cursor, 0, 0));
        jumps_done.push(sort_ins);

        let drain_top = prog.current_addr();
        let mut jumps_sorter_next = Vec::new();

        if let Some((rem_reg, off_reg_opt, one_reg, zero_reg)) = limit_regs {
            if let Some(off_reg) = off_reg_opt {
                let gt_r = prog.alloc_register();
                prog.emit(Instruction::new(Opcode::Gt, off_reg, zero_reg, gt_r));
                let skip_dec = prog.emit(Instruction::new(Opcode::JumpIfFalse, gt_r, 0, 1));
                prog.emit(Instruction::new(
                    Opcode::Subtract,
                    off_reg,
                    one_reg,
                    off_reg,
                ));
                jumps_sorter_next.push(prog.emit(goto_placeholder()));
                let after_off = prog.current_addr();
                prog.patch_p2(skip_dec, after_off);
            }
            let lt_r = prog.alloc_register();
            prog.emit(Instruction::new(Opcode::Lt, rem_reg, zero_reg, lt_r));
            let skip_lim = prog.emit(Instruction::new(Opcode::JumpIfTrue, lt_r, 0, 0));
            let le_r = prog.alloc_register();
            prog.emit(Instruction::new(Opcode::Le, rem_reg, zero_reg, le_r));
            jumps_done.push(prog.emit(Instruction::new(Opcode::JumpIfTrue, le_r, 0, 0)));
            prog.emit(Instruction::new(
                Opcode::Subtract,
                rem_reg,
                one_reg,
                rem_reg,
            ));
            let after_lim = prog.current_addr();
            prog.patch_p2(skip_lim, after_lim);
        }

        let out_base = prog.alloc_registers(expanded.len());
        let key_cnt = sort_exprs.len();
        for j in 0..expanded.len() {
            prog.emit(Instruction::new(
                Opcode::SorterColumn,
                s_cursor,
                (key_cnt + j) as i32,
                out_base + j as i32,
            ));
        }
        prog.emit(Instruction::new(
            Opcode::ResultRow,
            out_base,
            expanded.len() as i32,
            0,
        ));

        let s_next_addr = prog.current_addr();
        for j in jumps_sorter_next {
            prog.patch_p2(j, s_next_addr);
        }
        prog.emit(Instruction::new(
            Opcode::SorterNext,
            s_cursor,
            drain_top as i32,
            0,
        ));

        let done_addr = prog.current_addr();
        for j in jumps_done {
            prog.patch_p2(j, done_addr);
        }
        prog.emit(Instruction::new(Opcode::Close, s_cursor, 0, 0));
    }

    prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    prog.patch_p2(init_addr, body_start);
    Ok(Some(prog))
}

fn goto_placeholder() -> Instruction {
    Instruction::new(Opcode::Goto, 0, 0, 0)
}

enum OrderBySource {
    ProjCol(usize),
    Expr(Expr),
}

fn is_table_rowid_column_expr(expr: &Expr, tbl: &TableSchema, alias: Option<&str>) -> bool {
    if tbl.without_rowid {
        return false;
    }
    let Expr::Column { table, column } = expr else {
        return false;
    };
    if let Some(t) = table {
        let expected = alias.unwrap_or(&tbl.name);
        if !t.eq_ignore_ascii_case(expected) {
            return false;
        }
    }
    tbl.is_implicit_rowid_name(column)
        || tbl
            .rowid_alias_col
            .is_some_and(|ipk| tbl.columns[ipk].name.eq_ignore_ascii_case(column))
}

fn is_exact_table_column_expr(
    expr: &Expr,
    tbl: &TableSchema,
    alias: Option<&str>,
    target_col_idx: usize,
) -> bool {
    let Expr::Column { table, column } = expr else {
        return false;
    };
    if let Some(t) = table {
        let expected = alias.unwrap_or(&tbl.name);
        if !t.eq_ignore_ascii_case(expected) {
            return false;
        }
    }
    tbl.columns
        .get(target_col_idx)
        .is_some_and(|c| c.name.eq_ignore_ascii_case(column))
}

fn resolve_order_by_source(
    ob_expr: &Expr,
    expanded: &[(Expr, String, Option<String>)],
    tbl: &TableSchema,
) -> Option<(OrderBySource, Expr)> {
    let mut inner = ob_expr;
    while let Expr::Collate { expr, .. } = inner {
        inner = expr;
    }
    if let Expr::Integer(ord) = inner
        && *ord >= 1
        && (*ord as usize) <= expanded.len()
    {
        let idx = (*ord as usize) - 1;
        return Some((OrderBySource::ProjCol(idx), expanded[idx].0.clone()));
    }
    if let Expr::Column {
        table: None,
        column,
    } = inner
    {
        let in_tbl = tbl.find_column(column).is_some() || tbl.is_implicit_rowid_name(column);
        if let Some((idx, (proj_expr, _, al))) =
            expanded.iter().enumerate().find(|(_, (_, label, alias))| {
                alias
                    .as_ref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(column))
                    || label.eq_ignore_ascii_case(column)
            })
            && (al.is_some() || !in_tbl)
        {
            return Some((OrderBySource::ProjCol(idx), proj_expr.clone()));
        }
    }
    if let Some((idx, (proj_expr, _, _))) = expanded
        .iter()
        .enumerate()
        .find(|(_, (pe, _, _))| pe == inner)
    {
        return Some((OrderBySource::ProjCol(idx), proj_expr.clone()));
    }
    if expr_uses_select_alias(ob_expr, expanded, tbl) {
        return None;
    }
    Some((OrderBySource::Expr(ob_expr.clone()), ob_expr.clone()))
}

fn expr_uses_select_alias(
    expr: &Expr,
    expanded: &[(Expr, String, Option<String>)],
    tbl: &TableSchema,
) -> bool {
    match expr {
        Expr::Column {
            table: None,
            column,
        } => {
            let in_tbl = tbl.find_column(column).is_some() || tbl.is_implicit_rowid_name(column);
            !in_tbl
                && expanded.iter().any(|(_, label, alias)| {
                    alias
                        .as_ref()
                        .is_some_and(|a| a.eq_ignore_ascii_case(column))
                        || label.eq_ignore_ascii_case(column)
                })
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. } => expr_uses_select_alias(inner, expanded, tbl),
        Expr::Binary { left, right, .. } => {
            expr_uses_select_alias(left, expanded, tbl)
                || expr_uses_select_alias(right, expanded, tbl)
        }
        Expr::Between {
            expr: v, low, high, ..
        } => {
            expr_uses_select_alias(v, expanded, tbl)
                || expr_uses_select_alias(low, expanded, tbl)
                || expr_uses_select_alias(high, expanded, tbl)
        }
        Expr::InList { expr: v, list, .. } => {
            expr_uses_select_alias(v, expanded, tbl)
                || list
                    .iter()
                    .any(|e| expr_uses_select_alias(e, expanded, tbl))
        }
        Expr::Like {
            expr: v,
            pattern,
            escape,
            ..
        } => {
            expr_uses_select_alias(v, expanded, tbl)
                || expr_uses_select_alias(pattern, expanded, tbl)
                || escape
                    .as_ref()
                    .is_some_and(|e| expr_uses_select_alias(e, expanded, tbl))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_ref()
                .is_some_and(|e| expr_uses_select_alias(e, expanded, tbl))
                || branches.iter().any(|(w, t)| {
                    expr_uses_select_alias(w, expanded, tbl)
                        || expr_uses_select_alias(t, expanded, tbl)
                })
                || else_expr
                    .as_ref()
                    .is_some_and(|e| expr_uses_select_alias(e, expanded, tbl))
        }
        Expr::FunctionCall { args, filter, .. } => {
            let in_args = match args {
                FunctionArgs::Star => false,
                FunctionArgs::Exprs(es) => {
                    es.iter().any(|e| expr_uses_select_alias(e, expanded, tbl))
                }
            };
            in_args
                || filter
                    .as_ref()
                    .is_some_and(|e| expr_uses_select_alias(e, expanded, tbl))
        }
        Expr::RowValue(elems) => elems
            .iter()
            .any(|e| expr_uses_select_alias(e, expanded, tbl)),
        _ => false,
    }
}

fn infer_expr_collation(expr: &Expr, tbl: &TableSchema) -> Collation {
    match expr {
        Expr::Collate { collation, .. } => Collation::from_name(collation).unwrap_or_default(),
        Expr::Column { column, .. } => tbl
            .find_column(column)
            .map_or(Collation::Binary, |(_, c)| c.collation),
        _ => Collation::Binary,
    }
}

// ============================================================================
// Expression -> VDBE Register Compiler (`sqlite3ExprCode`)
// ============================================================================

fn compile_expr(
    prog: &mut VdbeProgram,
    expr: &Expr,
    binding: Option<&TableBinding<'_>>,
    dest: i32,
) -> Result<()> {
    match expr {
        Expr::Null => {
            prog.emit(Instruction::new(Opcode::Null, 0, dest, 0));
        }
        Expr::Bool(b) => {
            prog.emit(Instruction::new(Opcode::Integer, i32::from(*b), dest, 0));
        }
        Expr::Integer(i) => {
            if let Ok(small) = i32::try_from(*i) {
                prog.emit(Instruction::new(Opcode::Integer, small, dest, 0));
            } else {
                prog.emit(Instruction::new(Opcode::Int64, 0, dest, 0).with_p4(P4::Int64(*i)));
            }
        }
        Expr::Float(f) => {
            prog.emit(Instruction::new(Opcode::Real, 0, dest, 0).with_p4(P4::Real(*f)));
        }
        Expr::String(s) => {
            prog.emit(Instruction::new(Opcode::String8, 0, dest, 0).with_p4(P4::Str(s.clone())));
        }
        Expr::Blob(b) => {
            prog.emit(Instruction::new(Opcode::Blob, 0, dest, 0).with_p4(P4::Blob(b.clone())));
        }
        Expr::CurrentTimeKw(kind) => {
            let fn_name = match kind {
                CurrentTimeKind::Date => "CURRENT_DATE",
                CurrentTimeKind::Time => "CURRENT_TIME",
                CurrentTimeKind::Timestamp => "CURRENT_TIMESTAMP",
            };
            prog.emit(
                Instruction::new(Opcode::Function, 0, 0, dest).with_p4(P4::Func {
                    name: fn_name.to_owned(),
                    argc: 0,
                }),
            );
        }
        Expr::Bind(bp) => {
            let idx = match bp {
                BindParam::Anonymous(i) | BindParam::Numbered(i) | BindParam::Named(_, i) => *i,
            };
            prog.emit(Instruction::new(Opcode::Variable, idx as i32, dest, 0));
        }
        Expr::Column { table, column } => {
            let Some(b) = binding else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: {column}"),
                ));
            };
            if let Some(qual) = table {
                let expected = b.alias.unwrap_or(&b.tbl.name);
                if !qual.eq_ignore_ascii_case(expected) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such column: {qual}.{column}"),
                    ));
                }
            }
            if let Some((col_idx, col_schema)) = b.tbl.find_column(column) {
                if b.tbl.rowid_alias_col == Some(col_idx) {
                    // C SQLite `insert.c:1361`: INTEGER PRIMARY KEY slot in record is NULL; read from B+Tree rowid!
                    prog.emit(
                        Instruction::new(Opcode::Rowid, b.cursor, dest, 0)
                            .with_comment(col_schema.name.clone()),
                    );
                } else {
                    let phys_slot = b.tbl.physical_record_slot(col_idx);
                    prog.emit(
                        Instruction::new(Opcode::Column, b.cursor, phys_slot as i32, dest)
                            .with_p4(col_schema.default_p4_for_table(b.tbl.strict))
                            .with_comment(col_schema.name.clone()),
                    );
                }
            } else if b.tbl.is_implicit_rowid_name(column) {
                prog.emit(Instruction::new(Opcode::Rowid, b.cursor, dest, 0));
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: {column}"),
                ));
            }
        }
        Expr::Collate { expr: inner, .. } => {
            compile_expr(prog, inner, binding, dest)?;
        }
        Expr::Unary { op, expr: inner } => match op {
            UnaryOp::Plus => {
                // Unary `+` is a no-op expression wrapper in SQLite (`+col` strips affinity)
                compile_expr(prog, inner, binding, dest)?;
            }
            UnaryOp::Negate => {
                let tmp = prog.alloc_register();
                compile_expr(prog, inner, binding, tmp)?;
                prog.emit(Instruction::new(Opcode::Negate, tmp, dest, 0));
            }
            UnaryOp::BitNot => {
                let tmp = prog.alloc_register();
                compile_expr(prog, inner, binding, tmp)?;
                prog.emit(Instruction::new(Opcode::BitNot, tmp, dest, 0));
            }
            UnaryOp::Not => {
                let tmp = prog.alloc_register();
                compile_expr(prog, inner, binding, tmp)?;
                prog.emit(Instruction::new(Opcode::Not, tmp, dest, 0));
            }
        },
        Expr::Binary { left, op, right } => {
            let lhs = prog.alloc_register();
            let rhs = prog.alloc_register();
            compile_expr(prog, left, binding, lhs)?;
            compile_expr(prog, right, binding, rhs)?;
            let vdbe_op = match op {
                BinaryOp::Concat => Opcode::Concat,
                BinaryOp::Multiply => Opcode::Multiply,
                BinaryOp::Divide => Opcode::Divide,
                BinaryOp::Remainder => Opcode::Remainder,
                BinaryOp::Add => Opcode::Add,
                BinaryOp::Subtract => Opcode::Subtract,
                BinaryOp::ShiftLeft => Opcode::ShiftLeft,
                BinaryOp::ShiftRight => Opcode::ShiftRight,
                BinaryOp::BitAnd => Opcode::BitAnd,
                BinaryOp::BitOr => Opcode::BitOr,
                BinaryOp::Lt => Opcode::Lt,
                BinaryOp::Le => Opcode::Le,
                BinaryOp::Gt => Opcode::Gt,
                BinaryOp::Ge => Opcode::Ge,
                BinaryOp::Eq => Opcode::Eq,
                BinaryOp::Ne => Opcode::Ne,
                BinaryOp::Is => Opcode::Is,
                BinaryOp::IsNot => Opcode::IsNot,
                BinaryOp::And => Opcode::And,
                BinaryOp::Or => Opcode::Or,
            };
            let mut ins = Instruction::new(vdbe_op, lhs, rhs, dest);
            if matches!(
                vdbe_op,
                Opcode::Eq
                    | Opcode::Ne
                    | Opcode::Lt
                    | Opcode::Le
                    | Opcode::Gt
                    | Opcode::Ge
                    | Opcode::Is
                    | Opcode::IsNot
            ) {
                let coll = resolve_binary_collation(left, right, binding);
                let l_aff = compiler_expr_affinity(
                    left,
                    binding.map(|b| b.tbl),
                    binding.and_then(|b| b.alias),
                );
                let r_aff = compiler_expr_affinity(
                    right,
                    binding.map(|b| b.tbl),
                    binding.and_then(|b| b.alias),
                );
                let comp_aff = sqlite_compare_affinity(l_aff, r_aff);
                ins = ins
                    .with_p4(P4::Collation(coll))
                    .with_p5(u16::from(affinity_to_p5(comp_aff)));
            }
            prog.emit(ins);
        }
        Expr::IsNull {
            expr: inner,
            negated,
        } => {
            let tmp = prog.alloc_register();
            compile_expr(prog, inner, binding, tmp)?;
            let op = if *negated {
                Opcode::NotNull
            } else {
                Opcode::IsNull
            };
            prog.emit(Instruction::new(op, tmp, 0, dest));
        }
        Expr::Cast {
            expr: inner,
            type_name,
        } => {
            let tmp = prog.alloc_register();
            compile_expr(prog, inner, binding, tmp)?;
            prog.emit(
                Instruction::new(Opcode::Cast, tmp, dest, 0).with_p4(P4::Str(type_name.clone())),
            );
        }
        Expr::Between {
            expr: val,
            low,
            high,
            negated,
        } => {
            let ge_expr = Expr::Binary {
                left: val.clone(),
                op: BinaryOp::Ge,
                right: low.clone(),
            };
            let le_expr = Expr::Binary {
                left: val.clone(),
                op: BinaryOp::Le,
                right: high.clone(),
            };
            let combined = Expr::Binary {
                left: Box::new(ge_expr),
                op: BinaryOp::And,
                right: Box::new(le_expr),
            };
            if *negated {
                compile_expr(
                    prog,
                    &Expr::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(combined),
                    },
                    binding,
                    dest,
                )?;
            } else {
                compile_expr(prog, &combined, binding, dest)?;
            }
        }
        Expr::InList {
            expr: lhs_e,
            list,
            negated,
        } => {
            if list.is_empty() {
                // `x IN ()` is always FALSE (0); `x NOT IN ()` is always TRUE (1) in SQLite!
                prog.emit(Instruction::new(
                    Opcode::Integer,
                    i32::from(*negated),
                    dest,
                    0,
                ));
            } else {
                let mut disj: Option<Expr> = None;
                for item in list {
                    let eq = Expr::Binary {
                        left: lhs_e.clone(),
                        op: BinaryOp::Eq,
                        right: Box::new(item.clone()),
                    };
                    disj = Some(match disj {
                        None => eq,
                        Some(acc) => Expr::Binary {
                            left: Box::new(acc),
                            op: BinaryOp::Or,
                            right: Box::new(eq),
                        },
                    });
                }
                let final_e = if *negated {
                    Expr::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(disj.unwrap()),
                    }
                } else {
                    disj.unwrap()
                };
                compile_expr(prog, &final_e, binding, dest)?;
            }
        }
        Expr::Like {
            expr: txt_e,
            pattern,
            escape,
            op,
            negated,
        } => {
            let fname = match op {
                LikeOp::Like => "LIKE",
                LikeOp::Glob => "GLOB",
            };
            let argc = if escape.is_some() { 3 } else { 2 };
            let arg_base = prog.alloc_registers(argc);
            // Note: SQLite's LIKE/GLOB function takes `(pattern, string [, escape])`!
            compile_expr(prog, pattern, binding, arg_base)?;
            compile_expr(prog, txt_e, binding, arg_base + 1)?;
            if let Some(esc) = escape {
                compile_expr(prog, esc, binding, arg_base + 2)?;
            }
            let res_reg = if *negated {
                prog.alloc_register()
            } else {
                dest
            };
            prog.emit(
                Instruction::new(Opcode::Function, arg_base, 0, res_reg).with_p4(P4::Func {
                    name: fname.to_owned(),
                    argc,
                }),
            );
            if *negated {
                prog.emit(Instruction::new(Opcode::Not, res_reg, dest, 0));
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            let op_reg = if let Some(op_e) = operand {
                let r = prog.alloc_register();
                compile_expr(prog, op_e, binding, r)?;
                Some(r)
            } else {
                None
            };

            let mut end_jumps = Vec::new();
            for (when_e, then_e) in branches {
                let cond_reg = prog.alloc_register();
                if let Some(base_r) = op_reg {
                    let w_reg = prog.alloc_register();
                    compile_expr(prog, when_e, binding, w_reg)?;
                    let op_e = operand.as_ref().unwrap();
                    let coll = resolve_binary_collation(op_e, when_e, binding);
                    let l_aff = compiler_expr_affinity(
                        op_e,
                        binding.map(|b| b.tbl),
                        binding.and_then(|b| b.alias),
                    );
                    let r_aff = compiler_expr_affinity(
                        when_e,
                        binding.map(|b| b.tbl),
                        binding.and_then(|b| b.alias),
                    );
                    let comp_aff = sqlite_compare_affinity(l_aff, r_aff);
                    prog.emit(
                        Instruction::new(Opcode::Eq, base_r, w_reg, cond_reg)
                            .with_p4(P4::Collation(coll))
                            .with_p5(u16::from(affinity_to_p5(comp_aff))),
                    );
                } else {
                    compile_expr(prog, when_e, binding, cond_reg)?;
                }
                let j_false = prog.emit(Instruction::new(Opcode::JumpIfFalse, cond_reg, 0, 1));
                compile_expr(prog, then_e, binding, dest)?;
                end_jumps.push(prog.emit(goto_placeholder()));
                let next_branch = prog.current_addr();
                prog.patch_p2(j_false, next_branch);
            }

            if let Some(el) = else_expr {
                compile_expr(prog, el, binding, dest)?;
            } else {
                prog.emit(Instruction::new(Opcode::Null, 0, dest, 0));
            }
            let end_addr = prog.current_addr();
            for j in end_jumps {
                prog.patch_p2(j, end_addr);
            }
        }
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => {
            let (star_args, expr_list) = match args {
                FunctionArgs::Star => (true, Vec::new()),
                FunctionArgs::Exprs(es) => (false, es.clone()),
            };
            validate_function_call(
                name,
                star_args,
                expr_list.len(),
                *distinct,
                filter.is_some(),
                false,
            )?;
            let arg_base = prog.alloc_registers(expr_list.len().max(1));
            for (i, a) in expr_list.iter().enumerate() {
                compile_expr(prog, a, binding, arg_base + i as i32)?;
            }
            prog.emit(
                Instruction::new(Opcode::Function, arg_base, 0, dest).with_p4(P4::Func {
                    name: name.clone(),
                    argc: expr_list.len(),
                }),
            );
        }
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } | Expr::RowValue(_) => {
            return Err(SqliteError::misuse(
                "subquery must be evaluated by relational engine",
            ));
        }
    }
    Ok(())
}

fn resolve_binary_collation(
    left: &Expr,
    right: &Expr,
    binding: Option<&TableBinding<'_>>,
) -> Collation {
    if let Expr::Collate { collation, .. } = left {
        return Collation::from_name(collation).unwrap_or_default();
    }
    if let Expr::Collate { collation, .. } = right {
        return Collation::from_name(collation).unwrap_or_default();
    }
    if let Some(b) = binding {
        if let Expr::Column { column, .. } = left
            && let Some((_, c)) = b.tbl.find_column(column)
        {
            return c.collation;
        }
        if let Expr::Column { column, .. } = right
            && let Some((_, c)) = b.tbl.find_column(column)
        {
            return c.collation;
        }
    }
    Collation::Binary
}

pub const fn index_affinity_ok(comp_aff: TypeAffinity, idx_aff: TypeAffinity) -> bool {
    match comp_aff {
        TypeAffinity::Blob => true,
        TypeAffinity::Text => matches!(idx_aff, TypeAffinity::Text),
        TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real => matches!(
            idx_aff,
            TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real
        ),
    }
}

fn compiler_expr_affinity(
    expr: &Expr,
    tbl: Option<&TableSchema>,
    alias: Option<&str>,
) -> Option<TypeAffinity> {
    match expr {
        Expr::Collate { expr: inner, .. } => compiler_expr_affinity(inner, tbl, alias),
        Expr::Cast { type_name, .. } => Some(TypeAffinity::from_declared_type(type_name)),
        Expr::Column { table, column } => {
            let t = tbl?;
            if let Some(qual) = table {
                let expected = alias.unwrap_or(&t.name);
                if !qual.eq_ignore_ascii_case(expected) {
                    return None;
                }
            }
            if let Some((_, c)) = t.find_column(column) {
                Some(c.affinity)
            } else if t.is_implicit_rowid_name(column) {
                Some(TypeAffinity::Integer)
            } else {
                None
            }
        }
        _ => None,
    }
}

// ============================================================================
// `EXPLAIN` & `EXPLAIN QUERY PLAN` Disassembly Support
// ============================================================================

/// Generates a representative [`VdbeProgram`] for `EXPLAIN <stmt>`.
pub fn compile_for_explain(stmt: &Stmt, schema: &Schema) -> Result<VdbeProgram> {
    match stmt {
        Stmt::Select(sel) => {
            if let Some(prog) = try_compile_select(sel, schema)? {
                return Ok(prog);
            }
            let mut prog = VdbeProgram::new();
            prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Transaction, 0, 0, 0));
            prog.emit(
                Instruction::new(Opcode::OpenEphemeral, 0, 0, 0).with_comment("relational query"),
            );
            prog.emit(Instruction::new(Opcode::ResultRow, 1, 1, 0));
            prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
            Ok(prog)
        }
        Stmt::Insert(ins) => compile_insert_explain(ins, schema),
        Stmt::Update(upd) => compile_update_explain(upd, schema),
        Stmt::Delete(del) => compile_delete_explain(del, schema),
        _ => {
            let mut prog = VdbeProgram::new();
            prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Transaction, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
            Ok(prog)
        }
    }
}

fn compile_insert_explain(ins: &InsertStmt, schema: &Schema) -> Result<VdbeProgram> {
    let Some(tbl) = schema.find_table(&ins.table) else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", ins.table),
        ));
    };
    let mut prog = VdbeProgram::new();
    prog.read_only = false;
    prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    prog.emit(Instruction::new(Opcode::Transaction, 0, 1, 0));
    let tcur = prog.alloc_cursor();
    prog.emit(
        Instruction::new(
            Opcode::OpenWrite,
            tcur,
            tbl.root_pgno as i32,
            tbl.columns.len() as i32,
        )
        .with_comment(tbl.name.clone()),
    );
    let indexes = schema.indexes_for_table(&tbl.name);
    let mut icurs = Vec::new();
    for idx in &indexes {
        let ic = prog.alloc_cursor();
        prog.emit(
            Instruction::new(Opcode::OpenWriteIndex, ic, idx.root_pgno as i32, 0)
                .with_p4(P4::KeyInfo(idx.key_sort_orders()))
                .with_comment(idx.name.clone()),
        );
        icurs.push((ic, idx));
    }

    let rid_reg = prog.alloc_register();
    let col_base = prog.alloc_registers(tbl.columns.len());
    let rec_reg = prog.alloc_register();

    if let InsertSource::Values(rows) = &ins.source {
        for row in rows {
            prog.emit(Instruction::new(Opcode::NewRowid, tcur, rid_reg, 0));
            for (i, expr) in row.iter().take(tbl.columns.len()).enumerate() {
                if tbl.rowid_alias_col == Some(i) {
                    compile_expr(&mut prog, expr, None, rid_reg)?;
                    prog.emit(Instruction::new(Opcode::Null, 0, col_base + i as i32, 0));
                } else {
                    compile_expr(&mut prog, expr, None, col_base + i as i32)?;
                }
            }
            let affs: Vec<_> = tbl.columns.iter().map(|c| c.affinity).collect();
            prog.emit(
                Instruction::new(Opcode::Affinity, col_base, tbl.columns.len() as i32, 0)
                    .with_p4(P4::Affinity(affs)),
            );
            prog.emit(Instruction::new(
                Opcode::MakeRecord,
                col_base,
                tbl.columns.len() as i32,
                rec_reg,
            ));
            for (ic, idx) in &icurs {
                let idx_rec = prog.alloc_register();
                prog.emit(Instruction::new(
                    Opcode::MakeRecord,
                    col_base,
                    (idx.columns.len() + 1) as i32,
                    idx_rec,
                ));
                prog.emit(
                    Instruction::new(Opcode::IdxInsert, *ic, idx_rec, 0)
                        .with_p5(u16::from(idx.unique)),
                );
            }
            prog.emit(Instruction::new(Opcode::Insert, tcur, rec_reg, rid_reg).with_p5(0x03));
        }
    } else {
        prog.emit(Instruction::new(Opcode::NewRowid, tcur, rid_reg, 0));
        prog.emit(Instruction::new(
            Opcode::MakeRecord,
            col_base,
            tbl.columns.len() as i32,
            rec_reg,
        ));
        prog.emit(Instruction::new(Opcode::Insert, tcur, rec_reg, rid_reg).with_p5(0x03));
    }

    for (ic, _) in icurs {
        prog.emit(Instruction::new(Opcode::Close, ic, 0, 0));
    }
    prog.emit(Instruction::new(Opcode::Close, tcur, 0, 0));
    prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(prog)
}

fn compile_update_explain(upd: &UpdateStmt, schema: &Schema) -> Result<VdbeProgram> {
    let Some(tbl) = schema.find_table(&upd.table) else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", upd.table),
        ));
    };
    let mut prog = VdbeProgram::new();
    prog.read_only = false;
    prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    prog.emit(Instruction::new(Opcode::Transaction, 0, 1, 0));
    let tcur = prog.alloc_cursor();
    prog.emit(
        Instruction::new(
            Opcode::OpenWrite,
            tcur,
            tbl.root_pgno as i32,
            tbl.columns.len() as i32,
        )
        .with_comment(tbl.name.clone()),
    );
    let rew = prog.emit(Instruction::new(Opcode::Rewind, tcur, 0, 0));
    let rid = prog.alloc_register();
    prog.emit(Instruction::new(Opcode::Rowid, tcur, rid, 0));
    let rec = prog.alloc_register();
    prog.emit(Instruction::new(
        Opcode::MakeRecord,
        rid,
        tbl.columns.len() as i32,
        rec,
    ));
    prog.emit(Instruction::new(Opcode::Insert, tcur, rec, rid).with_p5(0x02));
    prog.emit(Instruction::new(Opcode::Next, tcur, (rew + 1) as i32, 0));
    let end = prog.current_addr();
    prog.patch_p2(rew, end);
    prog.emit(Instruction::new(Opcode::Close, tcur, 0, 0));
    prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(prog)
}

fn compile_delete_explain(del: &DeleteStmt, schema: &Schema) -> Result<VdbeProgram> {
    let Some(tbl) = schema.find_table(&del.table) else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", del.table),
        ));
    };
    let mut prog = VdbeProgram::new();
    prog.read_only = false;
    prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    prog.emit(Instruction::new(Opcode::Transaction, 0, 1, 0));
    let tcur = prog.alloc_cursor();
    prog.emit(
        Instruction::new(
            Opcode::OpenWrite,
            tcur,
            tbl.root_pgno as i32,
            tbl.columns.len() as i32,
        )
        .with_comment(tbl.name.clone()),
    );
    let rew = prog.emit(Instruction::new(Opcode::Rewind, tcur, 0, 0));
    prog.emit(Instruction::new(Opcode::Delete, tcur, 0, 0).with_p5(0x02));
    prog.emit(Instruction::new(Opcode::Next, tcur, (rew + 1) as i32, 0));
    let end = prog.current_addr();
    prog.patch_p2(rew, end);
    prog.emit(Instruction::new(Opcode::Close, tcur, 0, 0));
    prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(prog)
}

/// Generates 4-column `EXPLAIN QUERY PLAN <stmt>` rows: `(id, parent, notused, detail)`.
pub fn explain_query_plan(stmt: &Stmt, schema: &Schema) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    let mut next_id = 2i64;
    let mut push_detail = |detail: String| {
        rows.push(vec![
            Value::Integer(next_id),
            Value::Integer(0),
            Value::Integer(0),
            Value::Text(detail),
        ]);
        next_id += 1;
    };

    match stmt {
        Stmt::Select(sel) => {
            if let SelectCore::Select {
                from: Some(fc),
                selection,
                ..
            } = &sel.body.head
            {
                if fc.joins.is_empty()
                    && let TableOrSubquery::Table {
                        name,
                        alias,
                        indexed_by,
                    } = &fc.head
                    && let Some(tbl) = schema.find_table(name)
                {
                    let plan = match indexed_by {
                        Some(IndexHint::NotIndexed) => AccessPlan::FullScan,
                        _ => choose_access_plan(&tbl, alias.as_deref(), selection.as_ref(), schema),
                    };
                    let disp = alias.as_deref().unwrap_or(&tbl.name);
                    push_detail(plan.explain_detail(disp));
                } else {
                    if let TableOrSubquery::Table { name, alias, .. } = &fc.head {
                        push_detail(format!("SCAN {}", alias.as_deref().unwrap_or(name)));
                    } else {
                        push_detail("SCAN SUBQUERY".to_owned());
                    }
                    for j in &fc.joins {
                        match &j.right {
                            TableOrSubquery::Table { name, alias, .. } => {
                                push_detail(format!("SCAN {}", alias.as_deref().unwrap_or(name)));
                            }
                            TableOrSubquery::Subquery { .. } => {
                                push_detail("SCAN SUBQUERY".to_owned());
                            }
                        }
                    }
                }
                if !sel.order_by.is_empty() {
                    push_detail("USE TEMP B-TREE FOR ORDER BY".to_owned());
                }
            } else {
                push_detail("SCAN CONSTANT ROW".to_owned());
            }
        }
        Stmt::Update(upd) => {
            if let Some(tbl) = schema.find_table(&upd.table) {
                let plan =
                    choose_access_plan(&tbl, upd.alias.as_deref(), upd.selection.as_ref(), schema);
                let disp = upd.alias.as_deref().unwrap_or(&tbl.name);
                push_detail(plan.explain_detail(disp));
            }
        }
        Stmt::Delete(del) => {
            if let Some(tbl) = schema.find_table(&del.table) {
                let plan =
                    choose_access_plan(&tbl, del.alias.as_deref(), del.selection.as_ref(), schema);
                let disp = del.alias.as_deref().unwrap_or(&tbl.name);
                push_detail(plan.explain_detail(disp));
            }
        }
        _ => {
            push_detail("EXECUTE STATEMENT".to_owned());
        }
    }

    Ok(rows)
}
