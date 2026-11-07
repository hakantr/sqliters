//! Relational SQL query engine, DML/DDL executor, constraint & index manager, `VACUUM`, and `PRAGMA`s.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use sqliters_btree::Btree;
use sqliters_pager::TransactionState;
use sqliters_syntax::{
    AlterTableAction, BinaryOp, BindParam, CompoundOp, ConflictAction, CreateIndexStmt,
    CreateTableStmt, CurrentTimeKind, DeleteStmt, Expr, FromClause, FunctionArgs, InsertSource,
    InsertStmt, JoinClause, JoinConstraint, JoinType, LikeOp, LimitClause, OrderByTerm,
    PragmaValue, ResultColumn, SelectBody, SelectCore, SelectStmt, Stmt, TableOrSubquery, UnaryOp,
    UpdateStmt,
};
use sqliters_types::{
    Collation, ErrorCode, KeySortOrder, Result, SqliteError, TypeAffinity, Value,
    decode_record, encode_record, extended,
};
use sqliters_vdbe::{
    AggregateAccumulator, AggregateKind, Opcode, VmRuntimeContext, eval_cast, eval_comparison,
    eval_scalar_function, parse_escape_char, sqlite_glob_match, sqlite_like_match,
};

use crate::compiler::{
    AccessPlan, choose_access_plan, compile_for_explain, explain_query_plan,
    expr_column_label, expr_contains_aggregate,
};
use crate::schema::{
    ColumnSchema, IndexSchema, Schema, TableSchema, build_column_schema_from_def,
    build_index_schema_from_ast, build_table_schema_from_ast, delete_schema_entry,
    insert_schema_entry, update_schema_entry,
};

/// Materialized result set (`column_names` + `rows`) produced by executing a statement.
#[derive(Debug, Clone, Default)]
pub struct QueryOutput {
    pub column_names: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Mutable database state passed into statement execution.
pub struct ExecContext<'a> {
    pub btree: &'a mut Btree,
    pub schema: &'a mut Schema,
    pub runtime: &'a mut VmRuntimeContext,
    pub explicit_tx: &'a mut bool,
    pub implicit_savepoint_tx: &'a mut bool,
    pub foreign_keys: &'a mut bool,
}

#[derive(Debug, Clone)]
struct ScopedColumn {
    table_alias: Option<String>,
    name: String,
    collation: Collation,
    hidden_rowid: bool,
    merged_using: bool,
}

#[derive(Debug, Clone, Default)]
struct EvalRow {
    cols: Vec<ScopedColumn>,
    vals: Vec<Value>,
}

impl EvalRow {
    fn lookup(&self, qual: Option<&str>, col_name: &str) -> Result<Option<(Value, Collation)>> {
        let mut matches = Vec::new();
        for (i, sc) in self.cols.iter().enumerate() {
            if sc.hidden_rowid {
                continue;
            }
            if let Some(q) = qual {
                let Some(ref ta) = sc.table_alias else {
                    continue;
                };
                if !ta.eq_ignore_ascii_case(q) {
                    continue;
                }
            }
            if sc.name.eq_ignore_ascii_case(col_name) {
                matches.push((self.vals[i].clone(), sc.collation, sc.merged_using));
                if qual.is_none() && sc.merged_using {
                    // Deduplicated USING / NATURAL column takes unambiguous precedence
                    return Ok(Some((self.vals[i].clone(), sc.collation)));
                }
            }
        }

        if matches.len() == 1 {
            return Ok(Some((matches[0].0.clone(), matches[0].1)));
        }
        if matches.len() > 1 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("ambiguous column name: {col_name}"),
            ));
        }

        // Check hidden implicit rowid (`rowid`, `_rowid_`, `oid`)
        if col_name.eq_ignore_ascii_case("rowid")
            || col_name.eq_ignore_ascii_case("_rowid_")
            || col_name.eq_ignore_ascii_case("oid")
        {
            for (i, sc) in self.cols.iter().enumerate() {
                if !sc.hidden_rowid {
                    continue;
                }
                if let Some(q) = qual {
                    let Some(ref ta) = sc.table_alias else {
                        continue;
                    };
                    if !ta.eq_ignore_ascii_case(q) {
                        continue;
                    }
                }
                matches.push((self.vals[i].clone(), Collation::Binary, false));
            }
            if matches.len() == 1 {
                return Ok(Some((matches[0].0.clone(), Collation::Binary)));
            }
        }

        Ok(None)
    }
}

#[derive(Clone, Default)]
struct CteStore {
    tables: Vec<(String, QueryOutput)>,
}

impl CteStore {
    fn find(&self, name: &str) -> Option<&QueryOutput> {
        self.tables
            .iter()
            .rev()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, q)| q)
    }
}

/// Top-level statement dispatcher for all SQL statements (`SELECT`, DML, DDL, `EXPLAIN`, `PRAGMA`, transactions).
pub fn execute_statement(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    params: &[Value],
) -> Result<QueryOutput> {
    // Reload schema if schema_cookie changed externally
    if ctx.btree.pager().page_count() > 0
        && ctx.btree.pager().header().schema_cookie != ctx.schema.schema_cookie
    {
        *ctx.schema = Schema::load(ctx.btree)?;
    }

    match stmt {
        Stmt::Explain { query_plan, stmt } => {
            if *query_plan {
                let rows = explain_query_plan(stmt, ctx.schema)?;
                Ok(QueryOutput {
                    column_names: vec![
                        "id".to_owned(),
                        "parent".to_owned(),
                        "notused".to_owned(),
                        "detail".to_owned(),
                    ],
                    rows,
                })
            } else {
                let prog = compile_for_explain(stmt, ctx.schema)?;
                Ok(QueryOutput {
                    column_names: vec![
                        "addr".to_owned(),
                        "opcode".to_owned(),
                        "p1".to_owned(),
                        "p2".to_owned(),
                        "p3".to_owned(),
                        "p4".to_owned(),
                        "p5".to_owned(),
                        "comment".to_owned(),
                    ],
                    rows: prog.explain_rows(),
                })
            }
        }
        Stmt::Begin { mode } => {
            if *ctx.explicit_tx {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "cannot start a transaction within a transaction",
                ));
            }
            match mode {
                sqliters_syntax::TransactionMode::Deferred => {}
                sqliters_syntax::TransactionMode::Immediate
                | sqliters_syntax::TransactionMode::Exclusive => {
                    ctx.btree.begin_write()?;
                }
            }
            *ctx.explicit_tx = true;
            *ctx.implicit_savepoint_tx = false;
            Ok(QueryOutput::default())
        }
        Stmt::Commit => {
            if !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "cannot commit - no transaction is active",
                ));
            }
            ctx.btree.commit()?;
            *ctx.explicit_tx = false;
            *ctx.implicit_savepoint_tx = false;
            Ok(QueryOutput::default())
        }
        Stmt::Rollback { savepoint } => {
            if let Some(sp_name) = savepoint {
                ctx.btree.pager_mut().rollback_to_savepoint(sp_name)?;
                *ctx.schema = Schema::load(ctx.btree)?;
                Ok(QueryOutput::default())
            } else {
                if !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "cannot rollback - no transaction is active",
                    ));
                }
                ctx.btree.rollback()?;
                *ctx.explicit_tx = false;
                *ctx.implicit_savepoint_tx = false;
                *ctx.schema = Schema::load(ctx.btree)?;
                Ok(QueryOutput::default())
            }
        }
        Stmt::Savepoint(name) => {
            let was_autocommit = !*ctx.explicit_tx && ctx.btree.pager().savepoint_depth() == 0;
            ctx.btree.pager_mut().savepoint(name)?;
            if was_autocommit {
                *ctx.implicit_savepoint_tx = true;
            }
            Ok(QueryOutput::default())
        }
        Stmt::Release(name) => {
            ctx.btree.pager_mut().release_savepoint(name)?;
            if *ctx.implicit_savepoint_tx && ctx.btree.pager().savepoint_depth() == 0 {
                ctx.btree.commit()?;
                *ctx.implicit_savepoint_tx = false;
            }
            Ok(QueryOutput::default())
        }
        Stmt::Select(sel) => run_read_autocommit(ctx, |ctx| {
            exec_select(ctx, sel, params, &[], &CteStore::default())
        }),
        Stmt::Insert(ins) => run_write_autocommit(ctx, |ctx| exec_insert(ctx, ins, params)),
        Stmt::Update(upd) => run_write_autocommit(ctx, |ctx| exec_update(ctx, upd, params)),
        Stmt::Delete(del) => run_write_autocommit(ctx, |ctx| exec_delete(ctx, del, params)),
        Stmt::CreateTable(ct) => run_write_autocommit(ctx, |ctx| {
            exec_create_table(ctx, ct)?;
            Ok(QueryOutput::default())
        }),
        Stmt::DropTable { if_exists, name } => run_write_autocommit(ctx, |ctx| {
            exec_drop_table(ctx, *if_exists, name)?;
            Ok(QueryOutput::default())
        }),
        Stmt::CreateIndex(ci) => run_write_autocommit(ctx, |ctx| {
            exec_create_index(ctx, ci)?;
            Ok(QueryOutput::default())
        }),
        Stmt::DropIndex { if_exists, name } => run_write_autocommit(ctx, |ctx| {
            exec_drop_index(ctx, *if_exists, name)?;
            Ok(QueryOutput::default())
        }),
        Stmt::AlterTable { table, action } => run_write_autocommit(ctx, |ctx| {
            exec_alter_table(ctx, table, action)?;
            Ok(QueryOutput::default())
        }),
        Stmt::Pragma {
            schema: _,
            name,
            value,
        } => exec_pragma(ctx, name, value.as_ref()),
        Stmt::Vacuum => exec_vacuum(ctx),
        Stmt::Analyze(_) => Ok(QueryOutput::default()),
    }
}

fn is_in_autocommit(ctx: &ExecContext<'_>) -> bool {
    !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx
}

fn run_read_autocommit<F>(ctx: &mut ExecContext<'_>, f: F) -> Result<QueryOutput>
where
    F: FnOnce(&mut ExecContext<'_>) -> Result<QueryOutput>,
{
    let started = ctx.btree.transaction_state() == TransactionState::Open;
    if started && ctx.btree.pager().page_count() > 0 {
        ctx.btree.begin_read()?;
    }
    let res = f(ctx);
    if started && is_in_autocommit(ctx) && ctx.btree.transaction_state() == TransactionState::Read {
        let _ = ctx.btree.commit();
    }
    res
}

fn run_write_autocommit<F>(ctx: &mut ExecContext<'_>, f: F) -> Result<QueryOutput>
where
    F: FnOnce(&mut ExecContext<'_>) -> Result<QueryOutput>,
{
    let autocommit = is_in_autocommit(ctx);
    // Use an internal statement savepoint so statement-level ABORT rolls back partial statement writes
    // while keeping an enclosing explicit transaction alive (exact SQLite statement journal behavior!).
    let sp_name = "__sqliters_stmt_sp";
    ctx.btree.begin_write()?;
    if !autocommit {
        ctx.btree.pager_mut().savepoint(sp_name)?;
    }

    match f(ctx) {
        Ok(out) => {
            if autocommit {
                ctx.btree.commit()?;
            } else {
                ctx.btree.pager_mut().release_savepoint(sp_name)?;
            }
            Ok(out)
        }
        Err(err) => {
            if autocommit || err.code == ErrorCode::Full {
                let _ = ctx.btree.rollback();
                *ctx.explicit_tx = false;
                *ctx.implicit_savepoint_tx = false;
            } else {
                let _ = ctx.btree.pager_mut().rollback_to_savepoint(sp_name);
                let _ = ctx.btree.pager_mut().release_savepoint(sp_name);
            }
            *ctx.schema = Schema::load(ctx.btree)?;
            Err(err)
        }
    }
}

// ============================================================================
// Relational SELECT Engine (Joins, CTEs, Aggregates, Compounds, Subqueries)
// ============================================================================

fn exec_select(
    ctx: &mut ExecContext<'_>,
    stmt: &SelectStmt,
    params: &[Value],
    outer_scopes: &[EvalRow],
    parent_ctes: &CteStore,
) -> Result<QueryOutput> {
    let mut ctes = parent_ctes.clone();
    for cte in &stmt.ctes {
        let mut cte_out = exec_select(ctx, &cte.query, params, outer_scopes, &ctes)?;
        if let Some(explicit_cols) = &cte.columns {
            if explicit_cols.len() != cte_out.column_names.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "table {} has {} values for {} columns",
                        cte.name,
                        cte_out.column_names.len(),
                        explicit_cols.len()
                    ),
                ));
            }
            cte_out.column_names.clone_from(explicit_cols);
        }
        ctes.tables.push((cte.name.clone(), cte_out));
    }

    if stmt.body.compounds.is_empty() {
        exec_single_select_core(
            ctx,
            &stmt.body.head,
            &stmt.order_by,
            stmt.limit.as_ref(),
            params,
            outer_scopes,
            &ctes,
        )
    } else {
        exec_compound_select(
            ctx,
            &stmt.body,
            &stmt.order_by,
            stmt.limit.as_ref(),
            params,
            outer_scopes,
            &ctes,
        )
    }
}

fn exec_compound_select(
    ctx: &mut ExecContext<'_>,
    body: &SelectBody,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<QueryOutput> {
    let head_out = exec_single_select_core(ctx, &body.head, &[], None, params, outer_scopes, ctes)?;
    let col_names = head_out.column_names;
    let mut acc_rows = head_out.rows;

    for (op, rhs_core) in &body.compounds {
        let rhs_out =
            exec_single_select_core(ctx, rhs_core, &[], None, params, outer_scopes, ctes)?;
        if rhs_out.column_names.len() != col_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "SELECTs to the left and right of UNION/INTERSECT/EXCEPT do not have the same number of result columns",
            ));
        }
        match op {
            CompoundOp::UnionAll => {
                acc_rows.extend(rhs_out.rows);
            }
            CompoundOp::Union => {
                acc_rows.extend(rhs_out.rows);
                acc_rows = dedup_rows_sorted_or_seen(acc_rows);
            }
            CompoundOp::Intersect => {
                let rhs_keys: HashSet<Vec<u8>> =
                    rhs_out.rows.iter().map(|r| encode_record(r)).collect();
                let mut seen = HashSet::new();
                acc_rows.retain(|r| {
                    let k = encode_record(r);
                    rhs_keys.contains(&k) && seen.insert(k)
                });
            }
            CompoundOp::Except => {
                let rhs_keys: HashSet<Vec<u8>> =
                    rhs_out.rows.iter().map(|r| encode_record(r)).collect();
                let mut seen = HashSet::new();
                acc_rows.retain(|r| {
                    let k = encode_record(r);
                    !rhs_keys.contains(&k) && seen.insert(k)
                });
            }
        }
    }

    if !order_by.is_empty() {
        sort_materialized_output(&mut acc_rows, &col_names, order_by)?;
    }
    let acc_rows = apply_limit_offset(ctx, acc_rows, limit, params, outer_scopes, ctes)?;
    Ok(QueryOutput {
        column_names: col_names,
        rows: acc_rows,
    })
}

fn exec_single_select_core(
    ctx: &mut ExecContext<'_>,
    core: &SelectCore,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<QueryOutput> {
    match core {
        SelectCore::Values(expr_rows) => {
            if expr_rows.is_empty() {
                return Ok(QueryOutput::default());
            }
            let width = expr_rows[0].len();
            let col_names: Vec<String> = (1..=width).map(|i| format!("column{i}")).collect();
            let mut rows = Vec::with_capacity(expr_rows.len());
            for erow in expr_rows {
                if erow.len() != width {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "all VALUES must have the same number of terms",
                    ));
                }
                let mut vals = Vec::with_capacity(width);
                for e in erow {
                    vals.push(eval_expr(ctx, e, params, outer_scopes, ctes, None)?);
                }
                rows.push(vals);
            }
            if !order_by.is_empty() {
                sort_materialized_output(&mut rows, &col_names, order_by)?;
            }
            let rows = apply_limit_offset(ctx, rows, limit, params, outer_scopes, ctes)?;
            Ok(QueryOutput {
                column_names: col_names,
                rows,
            })
        }
        SelectCore::Select {
            distinct,
            projection,
            from,
            selection,
            group_by,
            having,
        } => {
            // 1. Evaluate FROM & JOINs
            let source_rows = match from {
                None => vec![EvalRow::default()],
                Some(fc) => eval_from_clause(
                    ctx,
                    fc,
                    selection.as_ref(),
                    params,
                    outer_scopes,
                    ctes,
                )?,
            };

            // 2. Filter via WHERE
            let mut filtered = Vec::with_capacity(source_rows.len());
            if let Some(where_expr) = selection {
                for r in source_rows {
                    let scopes = push_scope(outer_scopes, &r);
                    let v = eval_expr(ctx, where_expr, params, &scopes, ctes, None)?;
                    if v.truthiness() == Some(true) {
                        filtered.push(r);
                    }
                }
            } else {
                filtered = source_rows;
            }

            // 3. Determine whether query is aggregated
            let is_agg = !group_by.is_empty()
                || having.is_some()
                || projection.iter().any(|rc| match rc {
                    ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
                    _ => false,
                })
                || order_by.iter().any(|ob| expr_contains_aggregate(&ob.expr));

            let sample_cols = filtered
                .first()
                .map(|r| r.cols.clone())
                .or_else(|| from.as_ref().and_then(|fc| schema_cols_preview(ctx, fc, ctes)))
                .unwrap_or_default();

            let (col_names, expanded_proj) = expand_projection(projection, &sample_cols)?;

            let mut result_rows_with_sort: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();

            if is_agg {
                let groups = build_groups(ctx, filtered, group_by, params, outer_scopes, ctes)?;
                for group_rows in groups {
                    let rep_row = group_rows.first().cloned().unwrap_or_default();
                    let scopes = push_scope(outer_scopes, &rep_row);
                    if let Some(hav) = having {
                        let hv = eval_expr(
                            ctx,
                            hav,
                            params,
                            &scopes,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?;
                        if hv.truthiness() != Some(true) {
                            continue;
                        }
                    }

                    let mut out_vals = Vec::with_capacity(expanded_proj.len());
                    for (pe, _, _) in &expanded_proj {
                        out_vals.push(eval_expr(
                            ctx,
                            pe,
                            params,
                            &scopes,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?);
                    }

                    let mut sort_vals = Vec::with_capacity(order_by.len());
                    for ob in order_by {
                        let sv = eval_order_term_in_scope(
                            ctx,
                            &ob.expr,
                            &out_vals,
                            &expanded_proj,
                            &scopes,
                            params,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?;
                        sort_vals.push(sv);
                    }
                    result_rows_with_sort.push((out_vals, sort_vals));
                }
            } else {
                for row in filtered {
                    let scopes = push_scope(outer_scopes, &row);
                    let mut out_vals = Vec::with_capacity(expanded_proj.len());
                    for (pe, _, _) in &expanded_proj {
                        out_vals.push(eval_expr(ctx, pe, params, &scopes, ctes, None)?);
                    }

                    let mut sort_vals = Vec::with_capacity(order_by.len());
                    for ob in order_by {
                        let sv = eval_order_term_in_scope(
                            ctx,
                            &ob.expr,
                            &out_vals,
                            &expanded_proj,
                            &scopes,
                            params,
                            ctes,
                            None,
                        )?;
                        sort_vals.push(sv);
                    }
                    result_rows_with_sort.push((out_vals, sort_vals));
                }
            }

            if *distinct {
                let mut seen = HashSet::new();
                result_rows_with_sort.retain(|(proj, _)| seen.insert(encode_record(proj)));
            }

            if !order_by.is_empty() {
                let specs: Vec<KeySortOrder> = order_by
                    .iter()
                    .map(|ob| {
                        let coll = match &ob.expr {
                            Expr::Collate { collation, .. } => {
                                Collation::from_name(collation).unwrap_or_default()
                            }
                            _ => Collation::Binary,
                        };
                        KeySortOrder {
                            desc: ob.desc,
                            nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                            collation: coll,
                        }
                    })
                    .collect();

                result_rows_with_sort
                    .sort_by(|(_, sa), (_, sb)| compare_value_slices(sa, sb, &specs));
            }

            let out_rows: Vec<Vec<Value>> =
                result_rows_with_sort.into_iter().map(|(p, _)| p).collect();
            let out_rows = apply_limit_offset(ctx, out_rows, limit, params, outer_scopes, ctes)?;

            Ok(QueryOutput {
                column_names: col_names,
                rows: out_rows,
            })
        }
    }
}

fn eval_from_clause(
    ctx: &mut ExecContext<'_>,
    fc: &FromClause,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    let single_table_hint = if fc.joins.is_empty() {
        where_hint
    } else {
        None
    };
    let mut left_rows =
        eval_table_or_subquery(ctx, &fc.head, single_table_hint, params, outer_scopes, ctes)?;

    for join in &fc.joins {
        let right_rows =
            eval_table_or_subquery(ctx, &join.right, None, params, outer_scopes, ctes)?;
        left_rows = apply_join(ctx, left_rows, right_rows, join, params, outer_scopes, ctes)?;
    }

    Ok(left_rows)
}

fn eval_table_or_subquery(
    ctx: &mut ExecContext<'_>,
    tos: &TableOrSubquery,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    match tos {
        TableOrSubquery::Table { name, alias } => {
            let eff_alias = alias.clone().unwrap_or_else(|| name.clone());
            if let Some(cte_out) = ctes.find(name) {
                let cols: Vec<ScopedColumn> = cte_out
                    .column_names
                    .iter()
                    .map(|cn| ScopedColumn {
                        table_alias: Some(eff_alias.clone()),
                        name: cn.clone(),
                        collation: Collation::Binary,
                        hidden_rowid: false,
                        merged_using: false,
                    })
                    .collect();
                return Ok(cte_out
                    .rows
                    .iter()
                    .map(|r| EvalRow {
                        cols: cols.clone(),
                        vals: r.clone(),
                    })
                    .collect());
            }

            let Some(tbl) = ctx.schema.find_table(name) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {name}"),
                ));
            };

            let raw_entries = read_table_with_plan(
                ctx,
                &tbl,
                alias.as_deref(),
                where_hint,
                params,
                outer_scopes,
                ctes,
            )?;

            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    table_alias: Some(eff_alias.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    hidden_rowid: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                cols.push(ScopedColumn {
                    table_alias: Some(eff_alias),
                    name: "rowid".to_owned(),
                    collation: Collation::Binary,
                    hidden_rowid: true,
                    merged_using: false,
                });
            }

            let mut out = Vec::with_capacity(raw_entries.len());
            for (rowid, payload) in raw_entries {
                let mut vals = decode_table_logical_row(ctx, &tbl, rowid, &payload, params)?;
                if !tbl.without_rowid {
                    vals.push(Value::Integer(rowid));
                }
                out.push(EvalRow {
                    cols: cols.clone(),
                    vals,
                });
            }
            Ok(out)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_out = exec_select(ctx, query, params, outer_scopes, ctes)?;
            let cols: Vec<ScopedColumn> = sub_out
                .column_names
                .iter()
                .map(|cn| ScopedColumn {
                    table_alias: alias.clone(),
                    name: cn.clone(),
                    collation: Collation::Binary,
                    hidden_rowid: false,
                    merged_using: false,
                })
                .collect();
            Ok(sub_out
                .rows
                .into_iter()
                .map(|vals| EvalRow {
                    cols: cols.clone(),
                    vals,
                })
                .collect())
        }
    }
}

fn read_table_with_plan(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    alias: Option<&str>,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<(i64, Vec<u8>)>> {
    if ctx.btree.pager().page_count() == 0 {
        return Ok(Vec::new());
    }
    let plan = choose_access_plan(tbl, alias, where_hint, ctx.schema);
    match plan {
        AccessPlan::FullScan => ctx.btree.scan_table(tbl.root_pgno),
        AccessPlan::RowidSeek { key_expr } => {
            let kv = eval_expr(ctx, &key_expr, params, outer_scopes, ctes, None)?;
            let target = match kv {
                Value::Integer(i) => Some(i),
                Value::Real(r) if (r as i64 as f64) == r => Some(r as i64),
                Value::Text(ref s) => sqliters_types::parse_numeric_text(s).and_then(|v| match v {
                    Value::Integer(i) => Some(i),
                    Value::Real(r) if (r as i64 as f64) == r => Some(r as i64),
                    _ => None,
                }),
                _ => None,
            };
            let Some(rid) = target else {
                return Ok(Vec::new());
            };
            if let Some(payload) = ctx.btree.seek_table_rowid(tbl.root_pgno, rid)? {
                Ok(vec![(rid, payload)])
            } else {
                Ok(Vec::new())
            }
        }
        AccessPlan::IndexSeek {
            index,
            prefix_exprs,
        } => {
            let mut prefix_vals = Vec::with_capacity(prefix_exprs.len());
            for (i, ke) in prefix_exprs.iter().enumerate() {
                let raw = eval_expr(ctx, ke, params, outer_scopes, ctes, None)?;
                let aff = tbl.columns[index.columns[i].col_idx].affinity;
                let coerced = aff.apply(raw);
                if coerced.is_null() {
                    // `col = NULL` is never true in SQL `WHERE`!
                    return Ok(Vec::new());
                }
                prefix_vals.push(coerced);
            }
            let orders = index.key_sort_orders();
            let idx_hits = ctx
                .btree
                .seek_index_equal(index.root_pgno, &prefix_vals, &orders)?;
            let mut rows = Vec::with_capacity(idx_hits.len());
            for hit in idx_hits {
                if let Some(Value::Integer(rid)) = hit.last()
                    && let Some(payload) = ctx.btree.seek_table_rowid(tbl.root_pgno, *rid)?
                {
                    rows.push((*rid, payload));
                }
            }
            Ok(rows)
        }
    }
}

fn apply_join(
    ctx: &mut ExecContext<'_>,
    left_rows: Vec<EvalRow>,
    right_rows: Vec<EvalRow>,
    join: &JoinClause,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    let left_sample_cols = left_rows.first().map(|r| r.cols.clone()).unwrap_or_default();
    let right_sample_cols = right_rows
        .first()
        .map(|r| r.cols.clone())
        .or_else(|| schema_cols_for_tos(ctx, &join.right, ctes))
        .unwrap_or_default();

    // Determine USING columns if NATURAL or USING(...)
    let using_cols: Vec<String> = if join.natural {
        let mut shared = Vec::new();
        for lc in &left_sample_cols {
            if lc.hidden_rowid {
                continue;
            }
            if right_sample_cols
                .iter()
                .any(|rc| !rc.hidden_rowid && rc.name.eq_ignore_ascii_case(&lc.name))
                && !shared
                    .iter()
                    .any(|s: &String| s.eq_ignore_ascii_case(&lc.name))
            {
                shared.push(lc.name.clone());
            }
        }
        shared
    } else if let Some(JoinConstraint::Using(cols)) = &join.constraint {
        cols.clone()
    } else {
        Vec::new()
    };

    let combine_rows = |l: Option<&EvalRow>, r: Option<&EvalRow>| -> EvalRow {
        let mut cols = Vec::new();
        let mut vals = Vec::new();

        let l_cols = l.map_or(left_sample_cols.as_slice(), |row| row.cols.as_slice());
        let r_cols = r.map_or(right_sample_cols.as_slice(), |row| row.cols.as_slice());

        for (i, lc) in l_cols.iter().enumerate() {
            let mut sc = lc.clone();
            let mut val = l.map_or(Value::Null, |row| row.vals[i].clone());
            if !sc.hidden_rowid
                && using_cols
                    .iter()
                    .any(|u| u.eq_ignore_ascii_case(&sc.name))
            {
                sc.merged_using = true;
                if val.is_null()
                    && let Some(r_row) = r
                    && let Some((rv, _)) = r_row.lookup(None, &sc.name).ok().flatten()
                {
                    val = rv;
                }
            }
            cols.push(sc);
            vals.push(val);
        }

        for (j, rc) in r_cols.iter().enumerate() {
            if !rc.hidden_rowid
                && using_cols
                    .iter()
                    .any(|u| u.eq_ignore_ascii_case(&rc.name))
            {
                // Omit duplicate USING / NATURAL column from unqualified expansion
                continue;
            }
            cols.push(rc.clone());
            vals.push(r.map_or(Value::Null, |row| row.vals[j].clone()));
        }

        EvalRow { cols, vals }
    };

    let mut out = Vec::new();
    let mut right_matched = vec![false; right_rows.len()];

    for l in &left_rows {
        let mut any_match = false;
        for (ri, r) in right_rows.iter().enumerate() {
            let matches = if !using_cols.is_empty() {
                let mut ok = true;
                for u in &using_cols {
                    let lv = l.lookup(None, u)?.map_or(Value::Null, |(v, _)| v);
                    let rv = r.lookup(None, u)?.map_or(Value::Null, |(v, _)| v);
                    if eval_comparison(Opcode::Eq, &lv, &rv, Collation::Binary).truthiness()
                        != Some(true)
                    {
                        ok = false;
                        break;
                    }
                }
                ok
            } else if let Some(JoinConstraint::On(on_expr)) = &join.constraint {
                let candidate = combine_rows(Some(l), Some(r));
                let scopes = push_scope(outer_scopes, &candidate);
                eval_expr(ctx, on_expr, params, &scopes, ctes, None)?.truthiness() == Some(true)
            } else {
                true
            };

            if matches {
                any_match = true;
                right_matched[ri] = true;
                out.push(combine_rows(Some(l), Some(r)));
            }
        }

        if !any_match && matches!(join.join_type, JoinType::Left | JoinType::Full) {
            out.push(combine_rows(Some(l), None));
        }
    }

    if matches!(join.join_type, JoinType::Right | JoinType::Full) {
        for (ri, r) in right_rows.iter().enumerate() {
            if !right_matched[ri] {
                out.push(combine_rows(None, Some(r)));
            }
        }
    }

    Ok(out)
}

fn schema_cols_preview(
    ctx: &ExecContext<'_>,
    fc: &FromClause,
    ctes: &CteStore,
) -> Option<Vec<ScopedColumn>> {
    let mut cols = schema_cols_for_tos(ctx, &fc.head, ctes)?;
    for j in &fc.joins {
        if let Some(rcols) = schema_cols_for_tos(ctx, &j.right, ctes) {
            cols.extend(rcols);
        }
    }
    Some(cols)
}

fn schema_cols_for_tos(
    ctx: &ExecContext<'_>,
    tos: &TableOrSubquery,
    ctes: &CteStore,
) -> Option<Vec<ScopedColumn>> {
    match tos {
        TableOrSubquery::Table { name, alias } => {
            let eff = alias.clone().unwrap_or_else(|| name.clone());
            if let Some(cte) = ctes.find(name) {
                return Some(
                    cte.column_names
                        .iter()
                        .map(|cn| ScopedColumn {
                            table_alias: Some(eff.clone()),
                            name: cn.clone(),
                            collation: Collation::Binary,
                            hidden_rowid: false,
                            merged_using: false,
                        })
                        .collect(),
                );
            }
            let tbl = ctx.schema.find_table(name)?;
            Some(
                tbl.columns
                    .iter()
                    .map(|c| ScopedColumn {
                        table_alias: Some(eff.clone()),
                        name: c.name.clone(),
                        collation: c.collation,
                        hidden_rowid: false,
                        merged_using: false,
                    })
                    .collect(),
            )
        }
        TableOrSubquery::Subquery { .. } => None,
    }
}

fn expand_projection(
    projection: &[ResultColumn],
    sample_cols: &[ScopedColumn],
) -> Result<(Vec<String>, Vec<(Expr, String, Option<String>)>)> {
    let mut names = Vec::new();
    let mut expanded = Vec::new();
    for rc in projection {
        match rc {
            ResultColumn::Star => {
                for sc in sample_cols {
                    if sc.hidden_rowid {
                        continue;
                    }
                    let e = Expr::Column {
                        table: if sc.merged_using {
                            None
                        } else {
                            sc.table_alias.clone()
                        },
                        column: sc.name.clone(),
                    };
                    names.push(sc.name.clone());
                    expanded.push((e, sc.name.clone(), None));
                }
            }
            ResultColumn::TableStar(tname) => {
                let mut found = false;
                for sc in sample_cols {
                    if sc.hidden_rowid {
                        continue;
                    }
                    if sc
                        .table_alias
                        .as_ref()
                        .is_some_and(|ta| ta.eq_ignore_ascii_case(tname))
                    {
                        found = true;
                        let e = Expr::Column {
                            table: Some(tname.clone()),
                            column: sc.name.clone(),
                        };
                        names.push(sc.name.clone());
                        expanded.push((e, sc.name.clone(), None));
                    }
                }
                if !found && !sample_cols.is_empty() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such table: {tname}"),
                    ));
                }
            }
            ResultColumn::Expr { expr, alias } => {
                let label = alias.clone().unwrap_or_else(|| expr_column_label(expr));
                names.push(label.clone());
                expanded.push((expr.clone(), label, alias.clone()));
            }
        }
    }
    Ok((names, expanded))
}

fn build_groups(
    ctx: &mut ExecContext<'_>,
    rows: Vec<EvalRow>,
    group_by: &[Expr],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<Vec<EvalRow>>> {
    if group_by.is_empty() {
        // Ungrouped aggregate (`SELECT COUNT(*) FROM t`): always yields 1 group even if `rows` is empty!
        return Ok(vec![rows]);
    }
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let mut groups: Vec<(Vec<Value>, Vec<Collation>, Vec<EvalRow>)> = Vec::new();
    for row in rows {
        let scopes = push_scope(outer_scopes, &row);
        let mut g_vals = Vec::with_capacity(group_by.len());
        let mut g_colls = Vec::with_capacity(group_by.len());
        for ge in group_by {
            let v = eval_expr(ctx, ge, params, &scopes, ctes, None)?;
            let coll = match ge {
                Expr::Collate { collation, .. } => {
                    Collation::from_name(collation).unwrap_or_default()
                }
                Expr::Column { table, column } => row
                    .lookup(table.as_deref(), column)?
                    .map_or(Collation::Binary, |(_, c)| c),
                _ => Collation::Binary,
            };
            g_vals.push(v);
            g_colls.push(coll);
        }

        let existing = groups.iter_mut().find(|(kv, colls, _)| {
            kv.iter()
                .zip(g_vals.iter())
                .zip(colls.iter())
                .all(|((a, b), &c)| match (a.is_null(), b.is_null()) {
                    (true, true) => true,
                    (true, false) | (false, true) => false,
                    (false, false) => a.cmp_sqlite(b, c) == Ordering::Equal,
                })
        });
        if let Some((_, _, bucket)) = existing {
            bucket.push(row);
        } else {
            groups.push((g_vals, g_colls, vec![row]));
        }
    }

    // Sort groups in SQLite's natural GROUP BY B-tree order!
    groups.sort_by(|(ka, ca, _), (kb, _, _)| {
        let specs: Vec<KeySortOrder> = ca
            .iter()
            .map(|&c| KeySortOrder {
                desc: false,
                nulls_last: false,
                collation: c,
            })
            .collect();
        compare_value_slices(ka, kb, &specs)
    });

    Ok(groups.into_iter().map(|(_, _, b)| b).collect())
}

#[allow(clippy::too_many_arguments)]
fn eval_order_term_in_scope(
    ctx: &mut ExecContext<'_>,
    ob_expr: &Expr,
    proj_vals: &[Value],
    expanded_proj: &[(Expr, String, Option<String>)],
    scopes: &[EvalRow],
    params: &[Value],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Value> {
    if let Expr::Integer(ord) = ob_expr
        && *ord >= 1
        && (*ord as usize) <= proj_vals.len()
    {
        return Ok(proj_vals[(*ord as usize) - 1].clone());
    }
    if let Expr::Column {
        table: None,
        column,
    } = ob_expr
        && let Some((idx, _)) = expanded_proj.iter().enumerate().find(|(_, (_, label, al))| {
            al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                || label.eq_ignore_ascii_case(column)
        })
    {
        // Prefer alias match if column is not directly in scope or is an explicit alias
        let in_row = scopes
            .last()
            .and_then(|r| r.lookup(None, column).ok().flatten())
            .is_some();
        if expanded_proj[idx].2.is_some() || !in_row {
            return Ok(proj_vals[idx].clone());
        }
    }
    eval_expr(ctx, ob_expr, params, scopes, ctes, agg_group)
}

fn sort_materialized_output(
    rows: &mut [Vec<Value>],
    col_names: &[String],
    order_by: &[OrderByTerm],
) -> Result<()> {
    let mut specs = Vec::with_capacity(order_by.len());
    for ob in order_by {
        let inner = match &ob.expr {
            Expr::Collate { expr, .. } => expr.as_ref(),
            other => other,
        };
        let col_idx = match inner {
            Expr::Integer(ord) if *ord >= 1 && (*ord as usize) <= col_names.len() => {
                (*ord as usize) - 1
            }
            Expr::Column { column, .. } => col_names
                .iter()
                .position(|cn| cn.eq_ignore_ascii_case(column))
                .ok_or_else(|| {
                    SqliteError::new(
                        ErrorCode::Error,
                        format!("1st ORDER BY term does not match any column in the result set: {column}"),
                    )
                })?,
            _ => 0,
        };
        let coll = match &ob.expr {
            Expr::Collate { collation, .. } => Collation::from_name(collation).unwrap_or_default(),
            _ => Collation::Binary,
        };
        specs.push((
            col_idx,
            KeySortOrder {
                desc: ob.desc,
                nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                collation: coll,
            },
        ));
    }

    rows.sort_by(|a, b| {
        for &(idx, spec) in &specs {
            let ord = compare_single_value(&a[idx], &b[idx], spec);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    Ok(())
}

fn apply_limit_offset(
    ctx: &mut ExecContext<'_>,
    rows: Vec<Vec<Value>>,
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<Vec<Value>>> {
    let Some(lim) = limit else {
        return Ok(rows);
    };
    let lim_val = eval_expr(ctx, &lim.limit, params, outer_scopes, ctes, None)?.to_integer_lossy();
    let off_val = match &lim.offset {
        Some(oe) => eval_expr(ctx, oe, params, outer_scopes, ctes, None)?
            .to_integer_lossy()
            .max(0) as usize,
        None => 0,
    };

    let iter = rows.into_iter().skip(off_val);
    if lim_val < 0 {
        Ok(iter.collect())
    } else {
        Ok(iter.take(lim_val as usize).collect())
    }
}

fn dedup_rows_sorted_or_seen(rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if seen.insert(encode_record(&r)) {
            out.push(r);
        }
    }
    out
}

fn compare_value_slices(a: &[Value], b: &[Value], specs: &[KeySortOrder]) -> Ordering {
    for (i, spec) in specs.iter().enumerate() {
        let ord = compare_single_value(&a[i], &b[i], *spec);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn compare_single_value(va: &Value, vb: &Value, spec: KeySortOrder) -> Ordering {
    match (va.is_null(), vb.is_null()) {
        (true, true) => Ordering::Equal,
        (true, false) => {
            if spec.nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, true) => {
            if spec.nulls_last {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (false, false) => {
            let raw = va.cmp_sqlite(vb, spec.collation);
            if spec.desc { raw.reverse() } else { raw }
        }
    }
}

fn push_scope(outer: &[EvalRow], inner: &EvalRow) -> Vec<EvalRow> {
    let mut v = Vec::with_capacity(outer.len() + 1);
    v.extend_from_slice(outer);
    v.push(inner.clone());
    v
}

// ============================================================================
// Expression Evaluator (`eval_expr`)
// ============================================================================

fn eval_expr(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Value> {
    match expr {
        Expr::Null => Ok(Value::Null),
        Expr::Bool(b) => Ok(Value::Integer(i64::from(*b))),
        Expr::Integer(i) => Ok(Value::Integer(*i)),
        Expr::Float(f) => Ok(Value::real(*f)),
        Expr::String(s) => Ok(Value::Text(s.clone())),
        Expr::Blob(b) => Ok(Value::Blob(b.clone())),
        Expr::CurrentTimeKw(kind) => Ok(Value::Text(
            match kind {
                CurrentTimeKind::Date => "2026-11-08",
                CurrentTimeKind::Time => "00:00:00",
                CurrentTimeKind::Timestamp => "2026-11-08 00:00:00",
            }
            .to_owned(),
        )),
        Expr::Bind(bp) => {
            let idx = match bp {
                BindParam::Anonymous(i) | BindParam::Numbered(i) | BindParam::Named(_, i) => *i,
            };
            Ok(params
                .get(idx.saturating_sub(1))
                .cloned()
                .unwrap_or(Value::Null))
        }
        Expr::Column { table, column } => {
            for scope in scopes.iter().rev() {
                if let Some((val, _)) = scope.lookup(table.as_deref(), column)? {
                    return Ok(val);
                }
            }
            // In SQLite, double-quoted string fallback is deprecated; return clear error
            let full = match table {
                Some(t) => format!("{t}.{column}"),
                None => column.clone(),
            };
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {full}"),
            ))
        }
        Expr::Collate { expr: inner, .. } => eval_expr(ctx, inner, params, scopes, ctes, agg_group),
        Expr::Unary { op, expr: inner } => {
            let v = eval_expr(ctx, inner, params, scopes, ctes, agg_group)?;
            Ok(match op {
                UnaryOp::Plus => v,
                UnaryOp::Negate => match v {
                    Value::Null => Value::Null,
                    Value::Integer(i) => i
                        .checked_neg()
                        .map_or_else(|| Value::real(-(i as f64)), Value::Integer),
                    other => Value::real(-other.to_real_lossy()),
                },
                UnaryOp::BitNot => {
                    if v.is_null() {
                        Value::Null
                    } else {
                        Value::Integer(!v.to_integer_lossy())
                    }
                }
                UnaryOp::Not => match v.truthiness() {
                    None => Value::Null,
                    Some(b) => Value::Integer(i64::from(!b)),
                },
            })
        }
        Expr::Binary { left, op, right } => {
            // Short-circuit AND / OR
            if *op == BinaryOp::And {
                let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
                if lv.truthiness() == Some(false) {
                    return Ok(Value::Integer(0));
                }
                let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;
                return Ok(match (lv.truthiness(), rv.truthiness()) {
                    (_, Some(false)) => Value::Integer(0),
                    (Some(true), Some(true)) => Value::Integer(1),
                    _ => Value::Null,
                });
            }
            if *op == BinaryOp::Or {
                let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
                if lv.truthiness() == Some(true) {
                    return Ok(Value::Integer(1));
                }
                let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;
                return Ok(match (lv.truthiness(), rv.truthiness()) {
                    (_, Some(true)) => Value::Integer(1),
                    (Some(false), Some(false)) => Value::Integer(0),
                    _ => Value::Null,
                });
            }

            let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
            let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;

            match op {
                BinaryOp::Concat => {
                    if lv.is_null() || rv.is_null() {
                        Ok(Value::Null)
                    } else {
                        let mut s = lv.to_text_lossy();
                        s.push_str(&rv.to_text_lossy());
                        Ok(Value::Text(s))
                    }
                }
                BinaryOp::Add
                | BinaryOp::Subtract
                | BinaryOp::Multiply
                | BinaryOp::Divide
                | BinaryOp::Remainder => Ok(eval_binary_arith(*op, &lv, &rv)),
                BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                    if lv.is_null() || rv.is_null() {
                        Ok(Value::Null)
                    } else {
                        let a = lv.to_integer_lossy();
                        let b = rv.to_integer_lossy();
                        let res = match op {
                            BinaryOp::BitAnd => a & b,
                            BinaryOp::BitOr => a | b,
                            BinaryOp::ShiftLeft => {
                                if b < 0 {
                                    let nb = b.saturating_neg();
                                    if nb >= 64 { if a < 0 { -1 } else { 0 } } else { a >> (nb as u32) }
                                } else if b >= 64 {
                                    0
                                } else {
                                    a.wrapping_shl(b as u32)
                                }
                            }
                            BinaryOp::ShiftRight => {
                                if b < 0 {
                                    let nb = b.saturating_neg();
                                    if nb >= 64 { 0 } else { a.wrapping_shl(nb as u32) }
                                } else if b >= 64 {
                                    if a < 0 { -1 } else { 0 }
                                } else {
                                    a >> (b as u32)
                                }
                            }
                            _ => unreachable!(),
                        };
                        Ok(Value::Integer(res))
                    }
                }
                BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Is
                | BinaryOp::IsNot => {
                    let coll = infer_comparison_collation(left, right, scopes);
                    let vdbe_op = match op {
                        BinaryOp::Lt => Opcode::Lt,
                        BinaryOp::Le => Opcode::Le,
                        BinaryOp::Gt => Opcode::Gt,
                        BinaryOp::Ge => Opcode::Ge,
                        BinaryOp::Eq => Opcode::Eq,
                        BinaryOp::Ne => Opcode::Ne,
                        BinaryOp::Is => Opcode::Is,
                        BinaryOp::IsNot => Opcode::IsNot,
                        _ => unreachable!(),
                    };
                    Ok(eval_comparison(vdbe_op, &lv, &rv, coll))
                }
                BinaryOp::And | BinaryOp::Or => unreachable!(),
            }
        }
        Expr::IsNull {
            expr: inner,
            negated,
        } => {
            let v = eval_expr(ctx, inner, params, scopes, ctes, agg_group)?;
            let is_n = v.is_null();
            Ok(Value::Integer(i64::from(if *negated { !is_n } else { is_n })))
        }
        Expr::Between {
            expr: val_e,
            low,
            high,
            negated,
        } => {
            let v = eval_expr(ctx, val_e, params, scopes, ctes, agg_group)?;
            let lo = eval_expr(ctx, low, params, scopes, ctes, agg_group)?;
            let hi = eval_expr(ctx, high, params, scopes, ctes, agg_group)?;
            let coll = infer_comparison_collation(val_e, low, scopes);
            let ge = eval_comparison(Opcode::Ge, &v, &lo, coll).truthiness();
            let le = eval_comparison(Opcode::Le, &v, &hi, coll).truthiness();
            let res = match (ge, le) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
            Ok(match res {
                None => Value::Null,
                Some(b) => Value::Integer(i64::from(if *negated { !b } else { b })),
            })
        }
        Expr::InList {
            expr: lhs_e,
            list,
            negated,
        } => {
            let lv = eval_expr(ctx, lhs_e, params, scopes, ctes, agg_group)?;
            if list.is_empty() {
                return Ok(Value::Integer(i64::from(*negated)));
            }
            if lv.is_null() {
                return Ok(Value::Null);
            }
            let mut saw_null = false;
            for item in list {
                let iv = eval_expr(ctx, item, params, scopes, ctes, agg_group)?;
                if iv.is_null() {
                    saw_null = true;
                } else if eval_comparison(Opcode::Eq, &lv, &iv, Collation::Binary).truthiness()
                    == Some(true)
                {
                    return Ok(Value::Integer(i64::from(!*negated)));
                }
            }
            if saw_null {
                Ok(Value::Null)
            } else {
                Ok(Value::Integer(i64::from(*negated)))
            }
        }
        Expr::InSubquery {
            expr: lhs_e,
            subquery,
            negated,
        } => {
            let lv = eval_expr(ctx, lhs_e, params, scopes, ctes, agg_group)?;
            let sub_out = exec_select(ctx, subquery, params, scopes, ctes)?;
            if sub_out.column_names.len() != 1 && !sub_out.column_names.is_empty() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "sub-select returns more than 1 column",
                ));
            }
            if sub_out.rows.is_empty() {
                return Ok(Value::Integer(i64::from(*negated)));
            }
            if lv.is_null() {
                return Ok(Value::Null);
            }
            let mut saw_null = false;
            for r in &sub_out.rows {
                let iv = &r[0];
                if iv.is_null() {
                    saw_null = true;
                } else if eval_comparison(Opcode::Eq, &lv, iv, Collation::Binary).truthiness()
                    == Some(true)
                {
                    return Ok(Value::Integer(i64::from(!*negated)));
                }
            }
            if saw_null {
                Ok(Value::Null)
            } else {
                Ok(Value::Integer(i64::from(*negated)))
            }
        }
        Expr::Like {
            expr: txt_e,
            pattern,
            escape,
            op,
            negated,
        } => {
            let tv = eval_expr(ctx, txt_e, params, scopes, ctes, agg_group)?;
            let pv = eval_expr(ctx, pattern, params, scopes, ctes, agg_group)?;
            if tv.is_null() || pv.is_null() {
                return Ok(Value::Null);
            }
            let esc = if let Some(ee) = escape {
                let ev = eval_expr(ctx, ee, params, scopes, ctes, agg_group)?;
                if ev.is_null() {
                    return Ok(Value::Null);
                }
                Some(parse_escape_char(&ev.to_text_lossy())?)
            } else {
                None
            };
            let matched = match op {
                LikeOp::Like => sqlite_like_match(&pv.to_text_lossy(), &tv.to_text_lossy(), esc),
                LikeOp::Glob => sqlite_glob_match(&pv.to_text_lossy(), &tv.to_text_lossy()),
            };
            Ok(Value::Integer(i64::from(if *negated { !matched } else { matched })))
        }
        Expr::Cast {
            expr: inner,
            type_name,
        } => {
            let v = eval_expr(ctx, inner, params, scopes, ctes, agg_group)?;
            Ok(eval_cast(v, type_name))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            let op_val = match operand {
                Some(oe) => Some(eval_expr(ctx, oe, params, scopes, ctes, agg_group)?),
                None => None,
            };
            for (we, te) in branches {
                let cond_true = if let Some(ref base) = op_val {
                    let wv = eval_expr(ctx, we, params, scopes, ctes, agg_group)?;
                    eval_comparison(Opcode::Eq, base, &wv, Collation::Binary).truthiness()
                        == Some(true)
                } else {
                    eval_expr(ctx, we, params, scopes, ctes, agg_group)?.truthiness() == Some(true)
                };
                if cond_true {
                    return eval_expr(ctx, te, params, scopes, ctes, agg_group);
                }
            }
            if let Some(el) = else_expr {
                eval_expr(ctx, el, params, scopes, ctes, agg_group)
            } else {
                Ok(Value::Null)
            }
        }
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => {
            // Check if this is an aggregate call
            let is_star = matches!(args, FunctionArgs::Star);
            let upper = name.to_ascii_uppercase();
            let agg_kind_opt = match args {
                FunctionArgs::Star => AggregateKind::from_name(name, true),
                FunctionArgs::Exprs(es) => {
                    if (upper == "MIN" || upper == "MAX") && es.len() != 1 {
                        None
                    } else {
                        AggregateKind::from_name(name, false)
                    }
                }
            };

            if let Some(agg_kind) = agg_kind_opt {
                let Some((group_rows, base_outer)) = agg_group else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of aggregate function {name}()"),
                    ));
                };
                let mut acc = AggregateAccumulator::new(agg_kind, *distinct);
                for gr in group_rows {
                    let row_scopes = push_scope(base_outer, gr);
                    if let Some(f_expr) = filter {
                        let fv = eval_expr(ctx, f_expr, params, &row_scopes, ctes, None)?;
                        if fv.truthiness() != Some(true) {
                            continue;
                        }
                    }
                    if is_star {
                        acc.step(&[])?;
                    } else if let FunctionArgs::Exprs(es) = args {
                        let mut a_vals = Vec::with_capacity(es.len());
                        for ae in es {
                            a_vals.push(eval_expr(ctx, ae, params, &row_scopes, ctes, None)?);
                        }
                        acc.step(&a_vals)?;
                    }
                }
                return acc.finalize();
            }

            if filter.is_some() || *distinct {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("{name}() is not an aggregate function"),
                ));
            }
            let expr_list = match args {
                FunctionArgs::Star => Vec::new(),
                FunctionArgs::Exprs(es) => es.clone(),
            };
            let mut a_vals = Vec::with_capacity(expr_list.len());
            for ae in &expr_list {
                a_vals.push(eval_expr(ctx, ae, params, scopes, ctes, agg_group)?);
            }
            eval_scalar_function(name, &a_vals, ctx.runtime)
        }
        Expr::Subquery(sub) => {
            let sub_out = exec_select(ctx, sub, params, scopes, ctes)?;
            if sub_out.column_names.len() > 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "sub-select returns more than 1 column",
                ));
            }
            Ok(sub_out
                .rows
                .first()
                .and_then(|r| r.first())
                .cloned()
                .unwrap_or(Value::Null))
        }
        Expr::Exists { subquery, negated } => {
            let sub_out = exec_select(ctx, subquery, params, scopes, ctes)?;
            let exists = !sub_out.rows.is_empty();
            Ok(Value::Integer(i64::from(if *negated { !exists } else { exists })))
        }
    }
}

fn infer_comparison_collation(left: &Expr, right: &Expr, scopes: &[EvalRow]) -> Collation {
    if let Expr::Collate { collation, .. } = left {
        return Collation::from_name(collation).unwrap_or_default();
    }
    if let Expr::Collate { collation, .. } = right {
        return Collation::from_name(collation).unwrap_or_default();
    }
    for e in [left, right] {
        if let Expr::Column { table, column } = e {
            for s in scopes.iter().rev() {
                if let Ok(Some((_, c))) = s.lookup(table.as_deref(), column) {
                    return c;
                }
            }
        }
    }
    Collation::Binary
}

fn eval_binary_arith(op: BinaryOp, lhs: &Value, rhs: &Value) -> Value {
    if lhs.is_null() || rhs.is_null() {
        return Value::Null;
    }
    let coerce = |v: &Value| match v {
        Value::Integer(_) | Value::Real(_) => v.clone(),
        Value::Text(s) => sqliters_types::parse_numeric_text(s)
            .unwrap_or_else(|| Value::Integer(v.to_integer_lossy())),
        Value::Blob(_) => Value::Integer(v.to_integer_lossy()),
        Value::Null => Value::Null,
    };
    let a = coerce(lhs);
    let b = coerce(rhs);
    match (a, b) {
        (Value::Integer(ia), Value::Integer(ib)) => match op {
            BinaryOp::Add => ia
                .checked_add(ib)
                .map_or_else(|| Value::real((ia as f64) + (ib as f64)), Value::Integer),
            BinaryOp::Subtract => ia
                .checked_sub(ib)
                .map_or_else(|| Value::real((ia as f64) - (ib as f64)), Value::Integer),
            BinaryOp::Multiply => ia
                .checked_mul(ib)
                .map_or_else(|| Value::real((ia as f64) * (ib as f64)), Value::Integer),
            BinaryOp::Divide => {
                if ib == 0 {
                    Value::Null
                } else if ia == i64::MIN && ib == -1 {
                    Value::real(-(ia as f64))
                } else {
                    Value::Integer(ia / ib)
                }
            }
            BinaryOp::Remainder => {
                if ib == 0 {
                    Value::Null
                } else if ia == i64::MIN && ib == -1 {
                    Value::Integer(0)
                } else {
                    Value::Integer(ia % ib)
                }
            }
            _ => unreachable!(),
        },
        (va, vb) => {
            let fa = va.to_real_lossy();
            let fb = vb.to_real_lossy();
            match op {
                BinaryOp::Add => Value::real(fa + fb),
                BinaryOp::Subtract => Value::real(fa - fb),
                BinaryOp::Multiply => Value::real(fa * fb),
                BinaryOp::Divide => {
                    if fb == 0.0 {
                        Value::Null
                    } else {
                        Value::real(fa / fb)
                    }
                }
                BinaryOp::Remainder => {
                    let ia = va.to_integer_lossy();
                    let ib = vb.to_integer_lossy();
                    if ib == 0 {
                        Value::Null
                    } else {
                        Value::Integer(ia.wrapping_rem(ib))
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

// ============================================================================
// Table Record <-> Logical Row Helpers & Constraints / Indexes
// ============================================================================

fn decode_table_logical_row(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    rowid: i64,
    payload: &[u8],
    params: &[Value],
) -> Result<Vec<Value>> {
    let mut vals = decode_record(payload)?;
    while vals.len() < tbl.columns.len() {
        let missing_idx = vals.len();
        let def_val = match &tbl.columns[missing_idx].default_expr {
            Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
            None => Value::Null,
        };
        vals.push(tbl.columns[missing_idx].affinity.apply(def_val));
    }
    vals.truncate(tbl.columns.len());
    if let Some(ipk) = tbl.rowid_alias_col
        && ipk < vals.len()
    {
        vals[ipk] = Value::Integer(rowid);
    }
    Ok(vals)
}

fn encode_table_physical_record(tbl: &TableSchema, logical_vals: &[Value]) -> Vec<u8> {
    let mut slots = logical_vals.to_vec();
    // Per C SQLite `insert.c:1361`: put NULL in the INTEGER PRIMARY KEY slot of the on-disk record!
    if let Some(ipk) = tbl.rowid_alias_col
        && ipk < slots.len()
    {
        slots[ipk] = Value::Null;
    }
    encode_record(&slots)
}

fn make_single_table_scope(tbl: &TableSchema, alias: Option<&str>, rowid: i64, vals: &[Value]) -> EvalRow {
    let eff = alias.unwrap_or(&tbl.name).to_owned();
    let mut cols: Vec<ScopedColumn> = tbl
        .columns
        .iter()
        .map(|c| ScopedColumn {
            table_alias: Some(eff.clone()),
            name: c.name.clone(),
            collation: c.collation,
            hidden_rowid: false,
            merged_using: false,
        })
        .collect();
    let mut row_vals = vals.to_vec();
    if !tbl.without_rowid {
        cols.push(ScopedColumn {
            table_alias: Some(eff),
            name: "rowid".to_owned(),
            collation: Collation::Binary,
            hidden_rowid: true,
            merged_using: false,
        });
        row_vals.push(Value::Integer(rowid));
    }
    EvalRow {
        cols,
        vals: row_vals,
    }
}

fn check_strict_column_type(tbl_name: &str, col: &ColumnSchema, val: &Value) -> Result<()> {
    if val.is_null() {
        return Ok(());
    }
    let upper = col.type_name.trim().to_ascii_uppercase();
    let ok = match upper.as_str() {
        "INT" | "INTEGER" => matches!(val, Value::Integer(_)),
        "REAL" => matches!(val, Value::Real(_) | Value::Integer(_)),
        "TEXT" => matches!(val, Value::Text(_)),
        "BLOB" => matches!(val, Value::Blob(_)),
        "ANY" | "" => true,
        _ => true,
    };
    if !ok {
        return Err(SqliteError::constraint(
            extended::SQLITE_CONSTRAINT_DATATYPE,
            format!(
                "cannot store {} value in {} column {}.{}",
                val.storage_class().typeof_str().to_ascii_uppercase(),
                col.type_name,
                tbl_name,
                col.name
            ),
        ));
    }
    Ok(())
}

fn index_row_passes_partial_where(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    idx: &IndexSchema,
    rowid: i64,
    logical_vals: &[Value],
    params: &[Value],
) -> Result<bool> {
    let Some(ref w) = idx.where_clause else {
        return Ok(true);
    };
    let scope = make_single_table_scope(tbl, None, rowid, logical_vals);
    Ok(eval_expr(ctx, w, params, &[scope], &CteStore::default(), None)?.truthiness() == Some(true))
}

fn build_index_key_values(idx: &IndexSchema, rowid: i64, logical_vals: &[Value]) -> Vec<Value> {
    let mut keys = Vec::with_capacity(idx.columns.len() + 1);
    for ic in &idx.columns {
        keys.push(logical_vals[ic.col_idx].clone());
    }
    keys.push(Value::Integer(rowid));
    keys
}

/// Deletes `rowid` AND all its secondary index entries from `tbl`.
fn remove_row_and_indexes(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    indexes: &[IndexSchema],
    rowid: i64,
    params: &[Value],
) -> Result<bool> {
    let Some(old_payload) = ctx.btree.seek_table_rowid(tbl.root_pgno, rowid)? else {
        return Ok(false);
    };
    let old_vals = decode_table_logical_row(ctx, tbl, rowid, &old_payload, params)?;
    for idx in indexes {
        if index_row_passes_partial_where(ctx, tbl, idx, rowid, &old_vals, params)? {
            let keys = build_index_key_values(idx, rowid, &old_vals);
            let orders = idx.key_sort_orders();
            ctx.btree.delete_index(idx.root_pgno, &keys, &orders)?;
        }
    }
    ctx.btree.delete_table(tbl.root_pgno, rowid)
}

/// Validates constraints (NOT NULL, STRICT, CHECK, Rowid PK, UNIQUE indexes), applies conflict resolution
/// (`IGNORE` / `REPLACE` / error), and writes the row + secondary index entries!
/// Returns `Ok(Some((final_rowid, final_logical_vals)))` if written, or `Ok(None)` if skipped via `IGNORE`.
#[allow(clippy::too_many_arguments)]
fn validate_and_write_row(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    indexes: &[IndexSchema],
    candidate_rowid: Option<i64>,
    mut raw_vals: Vec<Value>,
    stmt_conflict: Option<ConflictAction>,
    existing_self_rowid: Option<i64>,
    params: &[Value],
    bump_last_rowid: bool,
) -> Result<Option<(i64, Vec<Value>)>> {
    // 1. Determine final rowid & apply column affinities
    let mut final_rowid = match candidate_rowid {
        Some(r) => Some(r),
        None => match tbl.rowid_alias_col {
            Some(ipk) => {
                if raw_vals[ipk].is_null() {
                    None
                } else {
                    let coerced = TypeAffinity::Integer.apply(raw_vals[ipk].clone());
                    let Some(i) = coerced.as_i64() else {
                        return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                    };
                    Some(i)
                }
            }
            None => None,
        },
    };

    if final_rowid.is_none() {
        final_rowid = Some(allocate_table_rowid(ctx, tbl)?);
    }
    let rowid = final_rowid.unwrap();

    for (i, col) in tbl.columns.iter().enumerate() {
        if tbl.rowid_alias_col == Some(i) {
            raw_vals[i] = Value::Integer(rowid);
        } else {
            raw_vals[i] = col.affinity.apply(raw_vals[i].clone());
            if tbl.strict
                && col.type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = raw_vals[i]
            {
                raw_vals[i] = Value::real(iv as f64);
            }
        }
    }

    // 2. STRICT type check
    if tbl.strict {
        for (i, col) in tbl.columns.iter().enumerate() {
            check_strict_column_type(&tbl.name, col, &raw_vals[i])?;
        }
    }

    // 3. NOT NULL constraint check
    for (i, col) in tbl.columns.iter().enumerate() {
        if let Some(col_conflict) = col.not_null
            && raw_vals[i].is_null()
        {
            let eff = stmt_conflict.unwrap_or(col_conflict);
            match eff {
                ConflictAction::Ignore => return Ok(None),
                ConflictAction::Replace if col.default_expr.is_some() => {
                    let dv = eval_expr(
                        ctx,
                        col.default_expr.as_ref().unwrap(),
                        params,
                        &[],
                        &CteStore::default(),
                        None,
                    )?;
                    if dv.is_null() {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            format!("NOT NULL constraint failed: {}.{}", tbl.name, col.name),
                        ));
                    }
                    raw_vals[i] = col.affinity.apply(dv);
                }
                _ => {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        format!("NOT NULL constraint failed: {}.{}", tbl.name, col.name),
                    ));
                }
            }
        }
    }

    // 4. CHECK constraints (`NULL` or `true` passes in SQL; only `false` fails!)
    if !tbl.checks.is_empty() {
        let scope = make_single_table_scope(tbl, None, rowid, &raw_vals);
        for chk in &tbl.checks {
            let cv = eval_expr(ctx, chk, params, &[scope.clone()], &CteStore::default(), None)?;
            if cv.truthiness() == Some(false) {
                if stmt_conflict == Some(ConflictAction::Ignore) {
                    return Ok(None);
                }
                return Err(SqliteError::constraint(
                    extended::SQLITE_CONSTRAINT_CHECK,
                    format!("CHECK constraint failed: {}", tbl.name),
                ));
            }
        }
    }

    // 5. Rowid / INTEGER PRIMARY KEY uniqueness check
    if existing_self_rowid != Some(rowid)
        && ctx.btree.seek_table_rowid(tbl.root_pgno, rowid)?.is_some()
    {
        let eff = stmt_conflict.unwrap_or(tbl.rowid_pk_conflict);
        match eff {
            ConflictAction::Ignore => return Ok(None),
            ConflictAction::Replace => {
                remove_row_and_indexes(ctx, tbl, indexes, rowid, params)?;
            }
            _ => {
                let col_label = tbl
                    .rowid_alias_col
                    .map_or("rowid", |ipk| tbl.columns[ipk].name.as_str());
                return Err(SqliteError::constraint(
                    extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                    format!("UNIQUE constraint failed: {}.{}", tbl.name, col_label),
                ));
            }
        }
    }

    // 6. Secondary UNIQUE index constraint checks
    for idx in indexes {
        if !idx.unique {
            continue;
        }
        if !index_row_passes_partial_where(ctx, tbl, idx, rowid, &raw_vals, params)? {
            continue;
        }
        let indexed_vals: Vec<Value> = idx
            .columns
            .iter()
            .map(|ic| raw_vals[ic.col_idx].clone())
            .collect();
        // SQL standard & SQLite: NULLs are distinct in UNIQUE indexes!
        if indexed_vals.iter().any(Value::is_null) {
            continue;
        }
        let orders = idx.key_sort_orders();
        let hits = ctx.btree.seek_index_equal(
            idx.root_pgno,
            &indexed_vals,
            &orders[..indexed_vals.len()],
        )?;
        let mut conflicting_rowids = Vec::new();
        for hit in hits {
            if let Some(Value::Integer(existing_rid)) = hit.last()
                && Some(*existing_rid) != existing_self_rowid
                && *existing_rid != rowid
            {
                conflicting_rowids.push(*existing_rid);
            }
        }
        if !conflicting_rowids.is_empty() {
            let eff = stmt_conflict.unwrap_or(idx.conflict);
            match eff {
                ConflictAction::Ignore => return Ok(None),
                ConflictAction::Replace => {
                    for crid in conflicting_rowids {
                        remove_row_and_indexes(ctx, tbl, indexes, crid, params)?;
                    }
                }
                _ => {
                    let cols_str: Vec<String> = idx
                        .columns
                        .iter()
                        .map(|c| format!("{}.{}", tbl.name, c.name))
                        .collect();
                    let ext_code = if idx.origin == "pk" {
                        extended::SQLITE_CONSTRAINT_PRIMARYKEY
                    } else {
                        extended::SQLITE_CONSTRAINT_UNIQUE
                    };
                    return Err(SqliteError::constraint(
                        ext_code,
                        format!("UNIQUE constraint failed: {}", cols_str.join(", ")),
                    ));
                }
            }
        }
    }

    // 7. If updating an existing row in-place or moving rowids, remove old row & old index entries first
    if let Some(old_rid) = existing_self_rowid {
        remove_row_and_indexes(ctx, tbl, indexes, old_rid, params)?;
    }

    // 8. Write table B+Tree record & secondary index records
    let phys_rec = encode_table_physical_record(tbl, &raw_vals);
    ctx.btree.insert_table(tbl.root_pgno, rowid, &phys_rec)?;

    for idx in indexes {
        if index_row_passes_partial_where(ctx, tbl, idx, rowid, &raw_vals, params)? {
            let keys = build_index_key_values(idx, rowid, &raw_vals);
            let idx_rec = encode_record(&keys);
            let orders = idx.key_sort_orders();
            ctx.btree.insert_index(idx.root_pgno, &idx_rec, &orders)?;
        }
    }

    if tbl.autoincrement {
        update_sqlite_sequence(ctx, &tbl.name, rowid)?;
    }

    if bump_last_rowid {
        ctx.runtime.last_insert_rowid = rowid;
    }
    ctx.runtime.last_changes += 1;
    ctx.runtime.total_changes += 1;

    Ok(Some((rowid, raw_vals)))
}

fn allocate_table_rowid(ctx: &mut ExecContext<'_>, tbl: &TableSchema) -> Result<i64> {
    let base_next = ctx.btree.next_rowid(tbl.root_pgno)?;
    if !tbl.autoincrement {
        return Ok(base_next);
    }
    let Some(seq_tbl) = ctx.schema.find_user_table("sqlite_sequence").cloned() else {
        return Ok(base_next);
    };
    let rows = ctx.btree.scan_table(seq_tbl.root_pgno)?;
    for (_, payload) in rows {
        let cols = decode_record(&payload)?;
        if cols.len() >= 2 && cols[0].as_str() == Some(tbl.name.as_str()) {
            let prev_seq = cols[1].to_integer_lossy();
            let candidate = prev_seq.checked_add(1).ok_or_else(|| {
                SqliteError::new(ErrorCode::Full, "database or disk is full")
            })?;
            return Ok(candidate.max(base_next));
        }
    }
    Ok(base_next)
}

fn update_sqlite_sequence(ctx: &mut ExecContext<'_>, tbl_name: &str, rowid: i64) -> Result<()> {
    let Some(seq_tbl) = ctx.schema.find_user_table("sqlite_sequence").cloned() else {
        return Ok(());
    };
    let rows = ctx.btree.scan_table(seq_tbl.root_pgno)?;
    for (seq_rid, payload) in rows {
        let cols = decode_record(&payload)?;
        if cols.len() >= 2 && cols[0].as_str() == Some(tbl_name) {
            let cur = cols[1].to_integer_lossy();
            if rowid > cur {
                let updated = encode_record(&[
                    Value::Text(tbl_name.to_owned()),
                    Value::Integer(rowid),
                ]);
                ctx.btree.insert_table(seq_tbl.root_pgno, seq_rid, &updated)?;
            }
            return Ok(());
        }
    }
    if rowid > 0 {
        let new_rid = ctx.btree.next_rowid(seq_tbl.root_pgno)?;
        let rec = encode_record(&[Value::Text(tbl_name.to_owned()), Value::Integer(rowid)]);
        ctx.btree.insert_table(seq_tbl.root_pgno, new_rid, &rec)?;
    }
    Ok(())
}

// ============================================================================
// DML Executors: `INSERT`, `UPDATE`, `DELETE`
// ============================================================================

fn exec_insert(
    ctx: &mut ExecContext<'_>,
    ins: &InsertStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    if Schema::is_schema_table_name(&ins.table) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {} may not be modified", ins.table),
        ));
    }
    let Some(tbl) = ctx.schema.find_table(&ins.table) else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", ins.table),
        ));
    };
    let indexes = ctx.schema.indexes_for_table(&tbl.name);

    // Resolve target column mapping (`None` = all columns in order; `Some(names)` = mapped columns, `usize::MAX` for implicit `rowid`)
    let col_targets: Option<Vec<usize>> = if let Some(ref cnames) = ins.columns {
        let mut idxs = Vec::with_capacity(cnames.len());
        for cn in cnames {
            if let Some((ci, _)) = tbl.find_column(cn) {
                idxs.push(ci);
            } else if tbl.is_implicit_rowid_name(cn) {
                idxs.push(usize::MAX);
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} has no column named {cn}", tbl.name),
                ));
            }
        }
        Some(idxs)
    } else {
        None
    };

    // Materialize input value rows BEFORE mutating target table (handles `INSERT INTO t SELECT * FROM t` safely!)
    let source_value_rows: Vec<Vec<Value>> = match &ins.source {
        InsertSource::DefaultValues => vec![Vec::new()],
        InsertSource::Values(expr_rows) => {
            let mut out = Vec::with_capacity(expr_rows.len());
            for erow in expr_rows {
                let mut vrow = Vec::with_capacity(erow.len());
                for e in erow {
                    vrow.push(eval_expr(ctx, e, params, &[], &CteStore::default(), None)?);
                }
                out.push(vrow);
            }
            out
        }
        InsertSource::Select(sel) => {
            exec_select(ctx, sel, params, &[], &CteStore::default())?.rows
        }
    };

    ctx.runtime.last_changes = 0;

    let sample_scope = make_single_table_scope(&tbl, None, 0, &vec![Value::Null; tbl.columns.len()]);
    let (ret_col_names, ret_expanded) = if ins.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_projection(&ins.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();

    for src_row in source_value_rows {
        let mut explicit_rowid: Option<i64> = None;
        let mut col_vals = Vec::with_capacity(tbl.columns.len());
        for col in &tbl.columns {
            let def = match &col.default_expr {
                Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                None => Value::Null,
            };
            col_vals.push(def);
        }

        if matches!(ins.source, InsertSource::DefaultValues) {
            // Keep defaults
        } else if let Some(ref targets) = col_targets {
            if src_row.len() != targets.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "{} values for {} columns",
                        src_row.len(),
                        targets.len()
                    ),
                ));
            }
            for (&t_idx, val) in targets.iter().zip(src_row.into_iter()) {
                if t_idx == usize::MAX {
                    if !val.is_null() {
                        let c = TypeAffinity::Integer.apply(val);
                        let rid = c.as_i64().ok_or_else(|| {
                            SqliteError::new(ErrorCode::Mismatch, "datatype mismatch")
                        })?;
                        explicit_rowid = Some(rid);
                    }
                } else {
                    col_vals[t_idx] = val;
                }
            }
        } else {
            if src_row.len() != tbl.columns.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "table {} has {} columns but {} values were supplied",
                        tbl.name,
                        tbl.columns.len(),
                        src_row.len()
                    ),
                ));
            }
            col_vals = src_row;
        }

        if let Some((written_rid, written_vals)) = validate_and_write_row(
            ctx,
            &tbl,
            &indexes,
            explicit_rowid,
            col_vals,
            ins.or_conflict,
            None,
            params,
            true,
        )? && !ret_expanded.is_empty()
        {
            let rscope = make_single_table_scope(&tbl, None, written_rid, &written_vals);
            let mut rrow = Vec::with_capacity(ret_expanded.len());
            for (pe, _, _) in &ret_expanded {
                rrow.push(eval_expr(
                    ctx,
                    pe,
                    params,
                    &[rscope.clone()],
                    &CteStore::default(),
                    None,
                )?);
            }
            ret_rows.push(rrow);
        }
    }

    Ok(QueryOutput {
        column_names: ret_col_names,
        rows: ret_rows,
    })
}

fn exec_update(
    ctx: &mut ExecContext<'_>,
    upd: &UpdateStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    if Schema::is_schema_table_name(&upd.table) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {} may not be modified", upd.table),
        ));
    }
    let Some(tbl) = ctx.schema.find_table(&upd.table) else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", upd.table),
        ));
    };
    let indexes = ctx.schema.indexes_for_table(&tbl.name);

    // Resolve assignment target columns (`usize::MAX` = implicit `rowid`)
    let mut resolved_assigns = Vec::with_capacity(upd.assignments.len());
    for (cname, expr) in &upd.assignments {
        if let Some((ci, _)) = tbl.find_column(cname) {
            resolved_assigns.push((ci, expr));
        } else if tbl.is_implicit_rowid_name(cname) {
            resolved_assigns.push((usize::MAX, expr));
        } else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {cname}"),
            ));
        }
    }

    let candidates = read_table_with_plan(
        ctx,
        &tbl,
        upd.alias.as_deref(),
        upd.selection.as_ref(),
        params,
        &[],
        &CteStore::default(),
    )?;

    // Two-phase update: collect all matching `(old_rowid, new_rowid_opt, new_col_vals)` before mutating B-Trees!
    let mut pending_updates = Vec::new();
    for (old_rowid, payload) in candidates {
        let old_vals = decode_table_logical_row(ctx, &tbl, old_rowid, &payload, params)?;
        let scope = make_single_table_scope(&tbl, upd.alias.as_deref(), old_rowid, &old_vals);
        if let Some(ref w) = upd.selection {
            let cond = eval_expr(ctx, w, params, &[scope.clone()], &CteStore::default(), None)?;
            if cond.truthiness() != Some(true) {
                continue;
            }
        }

        let mut new_vals = old_vals.clone();
        let mut new_rowid_opt = Some(old_rowid);
        for &(target_idx, val_expr) in &resolved_assigns {
            let v = eval_expr(
                ctx,
                val_expr,
                params,
                &[scope.clone()],
                &CteStore::default(),
                None,
            )?;
            if target_idx == usize::MAX {
                if v.is_null() {
                    new_rowid_opt = None;
                } else {
                    let c = TypeAffinity::Integer.apply(v);
                    let rid = c.as_i64().ok_or_else(|| {
                        SqliteError::new(ErrorCode::Mismatch, "datatype mismatch")
                    })?;
                    new_rowid_opt = Some(rid);
                }
            } else {
                new_vals[target_idx] = v;
                if tbl.rowid_alias_col == Some(target_idx) {
                    new_rowid_opt = None; // Let `validate_and_write_row` derive from `new_vals[ipk]`!
                }
            }
        }
        pending_updates.push((old_rowid, new_rowid_opt, new_vals));
    }

    ctx.runtime.last_changes = 0;

    let sample_scope = make_single_table_scope(
        &tbl,
        upd.alias.as_deref(),
        0,
        &vec![Value::Null; tbl.columns.len()],
    );
    let (ret_col_names, ret_expanded) = if upd.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_projection(&upd.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();

    for (old_rowid, new_rowid_opt, new_vals) in pending_updates {
        if let Some((written_rid, written_vals)) = validate_and_write_row(
            ctx,
            &tbl,
            &indexes,
            new_rowid_opt,
            new_vals,
            upd.or_conflict,
            Some(old_rowid),
            params,
            false,
        )? && !ret_expanded.is_empty()
        {
            let rscope = make_single_table_scope(
                &tbl,
                upd.alias.as_deref(),
                written_rid,
                &written_vals,
            );
            let mut rrow = Vec::with_capacity(ret_expanded.len());
            for (pe, _, _) in &ret_expanded {
                rrow.push(eval_expr(
                    ctx,
                    pe,
                    params,
                    &[rscope.clone()],
                    &CteStore::default(),
                    None,
                )?);
            }
            ret_rows.push(rrow);
        }
    }

    Ok(QueryOutput {
        column_names: ret_col_names,
        rows: ret_rows,
    })
}

fn exec_delete(
    ctx: &mut ExecContext<'_>,
    del: &DeleteStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    if Schema::is_schema_table_name(&del.table) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {} may not be modified", del.table),
        ));
    }
    let Some(tbl) = ctx.schema.find_table(&del.table) else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", del.table),
        ));
    };
    let indexes = ctx.schema.indexes_for_table(&tbl.name);

    // Fast path (`OP_Clear`) for unconditional `DELETE FROM tbl` without `RETURNING`!
    if del.selection.is_none() && del.returning.is_empty() {
        let count = ctx.btree.scan_table(tbl.root_pgno)?.len() as u64;
        ctx.btree.clear_tree(tbl.root_pgno)?;
        for idx in &indexes {
            ctx.btree.clear_tree(idx.root_pgno)?;
        }
        ctx.runtime.last_changes = count;
        ctx.runtime.total_changes += count;
        return Ok(QueryOutput::default());
    }

    let candidates = read_table_with_plan(
        ctx,
        &tbl,
        del.alias.as_deref(),
        del.selection.as_ref(),
        params,
        &[],
        &CteStore::default(),
    )?;

    let mut targets = Vec::new();
    for (rowid, payload) in candidates {
        let vals = decode_table_logical_row(ctx, &tbl, rowid, &payload, params)?;
        let scope = make_single_table_scope(&tbl, del.alias.as_deref(), rowid, &vals);
        if let Some(ref w) = del.selection {
            let cond = eval_expr(ctx, w, params, &[scope.clone()], &CteStore::default(), None)?;
            if cond.truthiness() != Some(true) {
                continue;
            }
        }
        targets.push((rowid, scope));
    }

    ctx.runtime.last_changes = 0;

    let sample_scope = make_single_table_scope(
        &tbl,
        del.alias.as_deref(),
        0,
        &vec![Value::Null; tbl.columns.len()],
    );
    let (ret_col_names, ret_expanded) = if del.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_projection(&del.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();

    for (rowid, scope) in targets {
        if remove_row_and_indexes(ctx, &tbl, &indexes, rowid, params)? {
            ctx.runtime.last_changes += 1;
            ctx.runtime.total_changes += 1;
            if !ret_expanded.is_empty() {
                let mut rrow = Vec::with_capacity(ret_expanded.len());
                for (pe, _, _) in &ret_expanded {
                    rrow.push(eval_expr(
                        ctx,
                        pe,
                        params,
                        &[scope.clone()],
                        &CteStore::default(),
                        None,
                    )?);
                }
                ret_rows.push(rrow);
            }
        }
    }

    Ok(QueryOutput {
        column_names: ret_col_names,
        rows: ret_rows,
    })
}

// ============================================================================
// DDL Executors: `CREATE/DROP TABLE`, `CREATE/DROP INDEX`, `ALTER TABLE`
// ============================================================================

fn exec_create_table(ctx: &mut ExecContext<'_>, ct: &CreateTableStmt) -> Result<()> {
    if ct.name.len() >= 7 && ct.name[..7].eq_ignore_ascii_case("sqlite_") {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", ct.name),
        ));
    }
    if ctx.schema.find_user_table(&ct.name).is_some() || ctx.schema.find_index(&ct.name).is_some() {
        if ct.if_not_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {} already exists", ct.name),
        ));
    }

    // Validate AST & inline autoindexes before allocating pages
    let (tbl_preview, pending_autos) = build_table_schema_from_ast(ct, 2)?;
    let tbl_root = ctx.btree.create_table()?;
    insert_schema_entry(
        ctx.btree,
        "table",
        &ct.name,
        &ct.name,
        tbl_root,
        Some(&ct.raw_sql),
    )?;

    for auto in pending_autos {
        let idx_root = ctx.btree.create_index()?;
        insert_schema_entry(ctx.btree, "index", &auto.name, &ct.name, idx_root, None)?;
    }

    // Create `sqlite_sequence` table if `AUTOINCREMENT` is used and `sqlite_sequence` doesn't exist yet
    if tbl_preview.autoincrement && ctx.schema.find_user_table("sqlite_sequence").is_none() {
        let seq_root = ctx.btree.create_table()?;
        insert_schema_entry(
            ctx.btree,
            "table",
            "sqlite_sequence",
            "sqlite_sequence",
            seq_root,
            Some("CREATE TABLE sqlite_sequence(name,seq)"),
        )?;
    }

    *ctx.schema = Schema::load(ctx.btree)?;
    Ok(())
}

fn exec_drop_table(ctx: &mut ExecContext<'_>, if_exists: bool, name: &str) -> Result<()> {
    if Schema::is_schema_table_name(name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {name} may not be dropped"),
        ));
    }
    let Some(tbl) = ctx.schema.find_user_table(name).cloned() else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {name}"),
        ));
    };

    let indexes = ctx.schema.indexes_for_table(&tbl.name);
    for idx in indexes {
        ctx.btree.drop_tree(idx.root_pgno)?;
        delete_schema_entry(ctx.btree, idx.schema_rowid)?;
    }
    ctx.btree.drop_tree(tbl.root_pgno)?;
    delete_schema_entry(ctx.btree, tbl.schema_rowid)?;

    // Also delete entry from `sqlite_sequence` if present
    if let Some(seq_tbl) = ctx.schema.find_user_table("sqlite_sequence").cloned() {
        let rows = ctx.btree.scan_table(seq_tbl.root_pgno)?;
        for (rid, payload) in rows {
            let cols = decode_record(&payload)?;
            if cols.first().and_then(Value::as_str) == Some(tbl.name.as_str()) {
                ctx.btree.delete_table(seq_tbl.root_pgno, rid)?;
            }
        }
    }

    *ctx.schema = Schema::load(ctx.btree)?;
    Ok(())
}

fn exec_create_index(ctx: &mut ExecContext<'_>, ci: &CreateIndexStmt) -> Result<()> {
    if ci.name.len() >= 7 && ci.name[..7].eq_ignore_ascii_case("sqlite_") {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", ci.name),
        ));
    }
    if ctx.schema.find_index(&ci.name).is_some() || ctx.schema.find_user_table(&ci.name).is_some() {
        if ci.if_not_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("index {} already exists", ci.name),
        ));
    }
    let Some(tbl) = ctx.schema.find_user_table(&ci.table_name).cloned() else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", ci.table_name),
        ));
    };

    let idx_root = ctx.btree.create_index()?;
    let idx_schema = build_index_schema_from_ast(ci, &tbl, idx_root)?;
    let orders = idx_schema.key_sort_orders();

    // Backfill existing table rows into the new index B-Tree (and enforce UNIQUE if `ci.unique`!)
    let existing_rows = ctx.btree.scan_table(tbl.root_pgno)?;
    for (rowid, payload) in existing_rows {
        let vals = decode_table_logical_row(ctx, &tbl, rowid, &payload, &[])?;
        if !index_row_passes_partial_where(ctx, &tbl, &idx_schema, rowid, &vals, &[])? {
            continue;
        }
        let indexed_only: Vec<Value> = idx_schema
            .columns
            .iter()
            .map(|ic| vals[ic.col_idx].clone())
            .collect();
        if ci.unique && indexed_only.iter().all(|v| !v.is_null()) {
            let hits = ctx.btree.seek_index_equal(
                idx_root,
                &indexed_only,
                &orders[..indexed_only.len()],
            )?;
            if !hits.is_empty() {
                let cols_str: Vec<String> = idx_schema
                    .columns
                    .iter()
                    .map(|c| format!("{}.{}", tbl.name, c.name))
                    .collect();
                return Err(SqliteError::constraint(
                    extended::SQLITE_CONSTRAINT_UNIQUE,
                    format!("UNIQUE constraint failed: {}", cols_str.join(", ")),
                ));
            }
        }
        let keys = build_index_key_values(&idx_schema, rowid, &vals);
        let idx_rec = encode_record(&keys);
        ctx.btree.insert_index(idx_root, &idx_rec, &orders)?;
    }

    insert_schema_entry(
        ctx.btree,
        "index",
        &ci.name,
        &tbl.name,
        idx_root,
        Some(&ci.raw_sql),
    )?;
    *ctx.schema = Schema::load(ctx.btree)?;
    Ok(())
}

fn exec_drop_index(ctx: &mut ExecContext<'_>, if_exists: bool, name: &str) -> Result<()> {
    let Some(idx) = ctx.schema.find_index(name).cloned() else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such index: {name}"),
        ));
    };
    if idx.is_autoindex {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "index associated with UNIQUE or PRIMARY KEY constraint cannot be dropped",
        ));
    }
    ctx.btree.drop_tree(idx.root_pgno)?;
    delete_schema_entry(ctx.btree, idx.schema_rowid)?;
    *ctx.schema = Schema::load(ctx.btree)?;
    Ok(())
}

fn exec_alter_table(
    ctx: &mut ExecContext<'_>,
    table: &str,
    action: &AlterTableAction,
) -> Result<()> {
    let Some(tbl) = ctx.schema.find_user_table(table).cloned() else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {table}"),
        ));
    };

    match action {
        AlterTableAction::RenameTo(new_name) => {
            if ctx.schema.find_user_table(new_name).is_some()
                || ctx.schema.find_index(new_name).is_some()
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("there is already another table or index with this name: {new_name}"),
                ));
            }
            let new_sql = replace_first_identifier_after_table(&tbl.raw_sql, &tbl.name, new_name);
            update_schema_entry(
                ctx.btree,
                tbl.schema_rowid,
                "table",
                new_name,
                new_name,
                tbl.root_pgno,
                Some(&new_sql),
            )?;
            let indexes = ctx.schema.indexes_for_table(&tbl.name);
            let mut auto_n = 0usize;
            for idx in indexes {
                if idx.is_autoindex {
                    auto_n += 1;
                    let new_auto = format!("sqlite_autoindex_{new_name}_{auto_n}");
                    update_schema_entry(
                        ctx.btree,
                        idx.schema_rowid,
                        "index",
                        &new_auto,
                        new_name,
                        idx.root_pgno,
                        None,
                    )?;
                } else {
                    let updated_idx_sql = idx
                        .raw_sql
                        .as_deref()
                        .map(|s| replace_on_table_in_index_sql(s, &tbl.name, new_name));
                    update_schema_entry(
                        ctx.btree,
                        idx.schema_rowid,
                        "index",
                        &idx.name,
                        new_name,
                        idx.root_pgno,
                        updated_idx_sql.as_deref(),
                    )?;
                }
            }
        }
        AlterTableAction::RenameColumn { old_name, new_name } => {
            if tbl.find_column(old_name).is_none() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: \"{old_name}\""),
                ));
            }
            if tbl.find_column(new_name).is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("duplicate column name: {new_name}"),
                ));
            }
            let new_sql = replace_column_name_in_create_table(&tbl.raw_sql, old_name, new_name);
            update_schema_entry(
                ctx.btree,
                tbl.schema_rowid,
                "table",
                &tbl.name,
                &tbl.name,
                tbl.root_pgno,
                Some(&new_sql),
            )?;
        }
        AlterTableAction::AddColumn(cdef) => {
            if tbl.find_column(&cdef.name).is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("duplicate column name: {}", cdef.name),
                ));
            }
            let _validated = build_column_schema_from_def(cdef)?;
            let rendered_col = render_column_def_sql(cdef);
            let new_sql = append_column_to_create_table_sql(&tbl.raw_sql, &rendered_col)?;
            update_schema_entry(
                ctx.btree,
                tbl.schema_rowid,
                "table",
                &tbl.name,
                &tbl.name,
                tbl.root_pgno,
                Some(&new_sql),
            )?;
        }
    }

    *ctx.schema = Schema::load(ctx.btree)?;
    Ok(())
}

fn replace_first_identifier_after_table(sql: &str, old_name: &str, new_name: &str) -> String {
    if let Some(pos) = sql.to_ascii_uppercase().find("TABLE") {
        let after = &sql[pos + 5..];
        if let Some(rel) = find_case_insensitive_word(after, old_name) {
            let abs = pos + 5 + rel;
            let mut out = String::with_capacity(sql.len() + new_name.len());
            out.push_str(&sql[..abs]);
            out.push_str(new_name);
            out.push_str(&sql[abs + old_name.len()..]);
            return out;
        }
    }
    sql.to_owned()
}

fn replace_on_table_in_index_sql(sql: &str, old_tbl: &str, new_tbl: &str) -> String {
    if let Some(pos) = sql.to_ascii_uppercase().find(" ON ") {
        let after = &sql[pos + 4..];
        if let Some(rel) = find_case_insensitive_word(after, old_tbl) {
            let abs = pos + 4 + rel;
            let mut out = String::new();
            out.push_str(&sql[..abs]);
            out.push_str(new_tbl);
            out.push_str(&sql[abs + old_tbl.len()..]);
            return out;
        }
    }
    sql.to_owned()
}

fn replace_column_name_in_create_table(sql: &str, old_col: &str, new_col: &str) -> String {
    let Some(lparen) = sql.find('(') else {
        return sql.to_owned();
    };
    let body = &sql[lparen + 1..];
    if let Some(rel) = find_case_insensitive_word(body, old_col) {
        let abs = lparen + 1 + rel;
        let mut out = String::new();
        out.push_str(&sql[..abs]);
        out.push_str(new_col);
        out.push_str(&sql[abs + old_col.len()..]);
        return out;
    }
    sql.to_owned()
}

fn find_case_insensitive_word(hay: &str, word: &str) -> Option<usize> {
    let h_up = hay.to_ascii_uppercase();
    let w_up = word.to_ascii_uppercase();
    let mut start = 0usize;
    while let Some(rel) = h_up[start..].find(&w_up) {
        let idx = start + rel;
        let before_ok = idx == 0
            || !hay.as_bytes()[idx - 1].is_ascii_alphanumeric() && hay.as_bytes()[idx - 1] != b'_';
        let end = idx + w_up.len();
        let after_ok = end == hay.len()
            || !hay.as_bytes()[end].is_ascii_alphanumeric() && hay.as_bytes()[end] != b'_';
        if before_ok && after_ok {
            return Some(idx);
        }
        start = idx + 1;
    }
    None
}

fn render_column_def_sql(cdef: &sqliters_syntax::ColumnDef) -> String {
    let mut parts = vec![cdef.name.clone()];
    if !cdef.type_name.is_empty() {
        parts.push(cdef.type_name.clone());
    }
    for cons in &cdef.constraints {
        match cons {
            sqliters_syntax::ColumnConstraint::NotNull { .. } => parts.push("NOT NULL".to_owned()),
            sqliters_syntax::ColumnConstraint::Null => parts.push("NULL".to_owned()),
            sqliters_syntax::ColumnConstraint::Default(e) => {
                parts.push(format!("DEFAULT {}", expr_column_label(e)));
            }
            sqliters_syntax::ColumnConstraint::Collate(c) => parts.push(format!("COLLATE {c}")),
            _ => {}
        }
    }
    parts.join(" ")
}

fn append_column_to_create_table_sql(sql: &str, rendered_col: &str) -> Result<String> {
    let Some(rparen) = sql.rfind(')') else {
        return Err(SqliteError::corrupt("CREATE TABLE missing closing ')'"));
    };
    let mut out = String::with_capacity(sql.len() + rendered_col.len() + 4);
    out.push_str(sql[..rparen].trim_end());
    out.push_str(", ");
    out.push_str(rendered_col);
    out.push_str(&sql[rparen..]);
    Ok(out)
}

// ============================================================================
// `VACUUM` & `PRAGMA` Execution (`vacuum.c`, `pragma.c`)
// ============================================================================

fn exec_vacuum(ctx: &mut ExecContext<'_>) -> Result<QueryOutput> {
    if !is_in_autocommit(ctx) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "cannot VACUUM from within a transaction",
        ));
    }
    if ctx.btree.pager().page_count() == 0 {
        return Ok(QueryOutput::default());
    }

    // Build a fresh compacted in-memory Btree with identical page_size & header metadata,
    // copy all tables and rebuild all indexes cleanly, then write pages back!
    let page_size = ctx.btree.pager().page_size();
    let orig_hdr = ctx.btree.pager().header().clone();

    let mut fresh = Btree::open_in_memory()?;
    fresh.pager_mut().set_page_size(page_size)?;
    fresh.begin_write()?;
    {
        let fhdr = fresh.pager_mut().header_mut()?;
        fhdr.user_version = orig_hdr.user_version;
        fhdr.application_id = orig_hdr.application_id;
        fhdr.schema_cookie = orig_hdr.schema_cookie.wrapping_add(1);
    }

    for tbl in &ctx.schema.tables {
        let new_tbl_root = fresh.create_table()?;
        insert_schema_entry(
            &mut fresh,
            "table",
            &tbl.name,
            &tbl.name,
            new_tbl_root,
            Some(&tbl.raw_sql),
        )?;
        let rows = ctx.btree.scan_table(tbl.root_pgno)?;
        for (rid, payload) in rows {
            fresh.insert_table(new_tbl_root, rid, &payload)?;
        }
    }

    for idx in &ctx.schema.indexes {
        let new_idx_root = fresh.create_index()?;
        insert_schema_entry(
            &mut fresh,
            "index",
            &idx.name,
            &idx.table_name,
            new_idx_root,
            idx.raw_sql.as_deref(),
        )?;
        let orders = idx.key_sort_orders();
        let entries = ctx.btree.scan_index(idx.root_pgno)?;
        for rec in entries {
            fresh.insert_index(new_idx_root, &rec, &orders)?;
        }
    }

    // Copy compacted pages into `ctx.btree` inside a single atomic write transaction!
    let compacted_count = fresh.pager().page_count();
    let compacted_hdr = fresh.pager().header().clone();

    ctx.btree.begin_write()?;
    while ctx.btree.pager().page_count() < compacted_count {
        ctx.btree.pager_mut().allocate_raw_page()?;
    }
    // Note: if compacted_count < old page_count, let's copy pages 1..=compacted_count and update header
    // If old page_count was larger, push surplus trailing pages or shrink cleanly on commit:
    for pgno in 1..=compacted_count {
        let src = fresh.pager_mut().read_page(pgno)?.to_vec();
        let dst = ctx.btree.pager_mut().write_page(pgno)?;
        dst.copy_from_slice(&src);
    }
    {
        let hdr = ctx.btree.pager_mut().header_mut()?;
        hdr.first_freelist_trunk_page = 0;
        hdr.total_freelist_pages = 0;
        hdr.user_version = compacted_hdr.user_version;
        hdr.application_id = compacted_hdr.application_id;
        hdr.schema_cookie = compacted_hdr.schema_cookie;
    }
    // Put any remaining pages above `compacted_count` onto freelist if pager hasn't shrunk page_count directly
    let cur_total = ctx.btree.pager().page_count();
    if cur_total > compacted_count {
        for extra_pg in (compacted_count + 1)..=cur_total {
            ctx.btree.free_page(extra_pg)?;
        }
    }
    ctx.btree.commit()?;
    *ctx.schema = Schema::load(ctx.btree)?;
    Ok(QueryOutput::default())
}

fn exec_pragma(
    ctx: &mut ExecContext<'_>,
    name: &str,
    value: Option<&PragmaValue>,
) -> Result<QueryOutput> {
    let upper = name.to_ascii_uppercase();
    let single_col = |cname: &str, v: Value| QueryOutput {
        column_names: vec![cname.to_owned()],
        rows: vec![vec![v]],
    };

    match upper.as_str() {
        "INTEGRITY_CHECK" | "QUICK_CHECK" => run_read_autocommit(ctx, |ctx| {
            let roots = ctx.schema.all_btree_roots();
            let errs = ctx.btree.integrity_check(&roots)?;
            let rows = if errs.is_empty() {
                vec![vec![Value::Text("ok".to_owned())]]
            } else {
                errs.into_iter().map(|e| vec![Value::Text(e)]).collect()
            };
            Ok(QueryOutput {
                column_names: vec!["integrity_check".to_owned()],
                rows,
            })
        }),
        "PAGE_SIZE" => {
            if let Some(pv) = value {
                let sz = pragma_to_i64(pv) as u32;
                ctx.btree.pager_mut().set_page_size(sz)?;
                Ok(QueryOutput::default())
            } else {
                Ok(single_col(
                    "page_size",
                    Value::Integer(i64::from(ctx.btree.pager().page_size())),
                ))
            }
        }
        "PAGE_COUNT" => Ok(single_col(
            "page_count",
            Value::Integer(i64::from(ctx.btree.pager().page_count())),
        )),
        "FREELIST_COUNT" => Ok(single_col(
            "freelist_count",
            Value::Integer(i64::from(ctx.btree.pager().header().total_freelist_pages)),
        )),
        "USER_VERSION" => {
            if let Some(pv) = value {
                let v = pragma_to_i64(pv) as i32;
                run_write_autocommit(ctx, |ctx| {
                    ctx.btree.pager_mut().header_mut()?.user_version = v;
                    Ok(QueryOutput::default())
                })
            } else {
                Ok(single_col(
                    "user_version",
                    Value::Integer(i64::from(ctx.btree.pager().header().user_version)),
                ))
            }
        }
        "APPLICATION_ID" => {
            if let Some(pv) = value {
                let v = pragma_to_i64(pv) as i32;
                run_write_autocommit(ctx, |ctx| {
                    ctx.btree.pager_mut().header_mut()?.application_id = v;
                    Ok(QueryOutput::default())
                })
            } else {
                Ok(single_col(
                    "application_id",
                    Value::Integer(i64::from(ctx.btree.pager().header().application_id)),
                ))
            }
        }
        "SCHEMA_VERSION" => Ok(single_col(
            "schema_version",
            Value::Integer(i64::from(ctx.btree.pager().header().schema_cookie)),
        )),
        "ENCODING" => Ok(single_col("encoding", Value::Text("UTF-8".to_owned()))),
        "JOURNAL_MODE" => {
            let mode = if ctx.btree.pager().is_in_memory() {
                "memory"
            } else {
                "delete"
            };
            Ok(single_col("journal_mode", Value::Text(mode.to_owned())))
        }
        "FOREIGN_KEYS" => {
            if let Some(pv) = value {
                *ctx.foreign_keys = pragma_to_bool(pv);
                Ok(QueryOutput::default())
            } else {
                Ok(single_col(
                    "foreign_keys",
                    Value::Integer(i64::from(*ctx.foreign_keys)),
                ))
            }
        }
        "SYNCHRONOUS" => {
            if value.is_some() {
                Ok(QueryOutput::default())
            } else {
                Ok(single_col("synchronous", Value::Integer(2)))
            }
        }
        "CACHE_SIZE" => {
            if value.is_some() {
                Ok(QueryOutput::default())
            } else {
                Ok(single_col("cache_size", Value::Integer(-2000)))
            }
        }
        "DATABASE_LIST" => {
            let file_str = ctx
                .btree
                .pager()
                .db_path()
                .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
            Ok(QueryOutput {
                column_names: vec!["seq".to_owned(), "name".to_owned(), "file".to_owned()],
                rows: vec![vec![
                    Value::Integer(0),
                    Value::Text("main".to_owned()),
                    Value::Text(file_str),
                ]],
            })
        }
        "TABLE_INFO" => {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let tname = pragma_to_string(pv);
            let Some(tbl) = ctx.schema.find_table(&tname) else {
                return Ok(QueryOutput {
                    column_names: vec![
                        "cid".to_owned(),
                        "name".to_owned(),
                        "type".to_owned(),
                        "notnull".to_owned(),
                        "dflt_value".to_owned(),
                        "pk".to_owned(),
                    ],
                    rows: Vec::new(),
                });
            };
            let rows = tbl
                .columns
                .iter()
                .enumerate()
                .map(|(cid, col)| {
                    let dflt = col
                        .default_expr
                        .as_ref()
                        .map_or(Value::Null, |e| Value::Text(expr_column_label(e)));
                    vec![
                        Value::Integer(cid as i64),
                        Value::Text(col.name.clone()),
                        Value::Text(col.type_name.clone()),
                        Value::Integer(i64::from(col.not_null.is_some())),
                        dflt,
                        Value::Integer(col.pk_order as i64),
                    ]
                })
                .collect();
            Ok(QueryOutput {
                column_names: vec![
                    "cid".to_owned(),
                    "name".to_owned(),
                    "type".to_owned(),
                    "notnull".to_owned(),
                    "dflt_value".to_owned(),
                    "pk".to_owned(),
                ],
                rows,
            })
        }
        "INDEX_LIST" => {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let tname = pragma_to_string(pv);
            let idxs = ctx.schema.indexes_for_table(&tname);
            let rows = idxs
                .iter()
                .enumerate()
                .map(|(seq, idx)| {
                    vec![
                        Value::Integer(seq as i64),
                        Value::Text(idx.name.clone()),
                        Value::Integer(i64::from(idx.unique)),
                        Value::Text(idx.origin.to_owned()),
                        Value::Integer(i64::from(idx.where_clause.is_some())),
                    ]
                })
                .collect();
            Ok(QueryOutput {
                column_names: vec![
                    "seq".to_owned(),
                    "name".to_owned(),
                    "unique".to_owned(),
                    "origin".to_owned(),
                    "partial".to_owned(),
                ],
                rows,
            })
        }
        "INDEX_INFO" => {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let iname = pragma_to_string(pv);
            let Some(idx) = ctx.schema.find_index(&iname) else {
                return Ok(QueryOutput::default());
            };
            let rows = idx
                .columns
                .iter()
                .enumerate()
                .map(|(seqno, ic)| {
                    vec![
                        Value::Integer(seqno as i64),
                        Value::Integer(ic.col_idx as i64),
                        Value::Text(ic.name.clone()),
                    ]
                })
                .collect();
            Ok(QueryOutput {
                column_names: vec!["seqno".to_owned(), "cid".to_owned(), "name".to_owned()],
                rows,
            })
        }
        "COMPILE_OPTIONS" => Ok(QueryOutput {
            column_names: vec!["compile_options".to_owned()],
            rows: vec![
                vec![Value::Text("PURE_SAFE_RUST".to_owned())],
                vec![Value::Text("SQLITE_FORMAT_3".to_owned())],
                vec![Value::Text("THREADSAFE=1".to_owned())],
            ],
        }),
        _ => {
            // Unknown PRAGMAs in SQLite are silently ignored (no error)
            let _ = HashMap::<u8, u8>::new();
            Ok(QueryOutput::default())
        }
    }
}

fn pragma_to_i64(pv: &PragmaValue) -> i64 {
    match pv {
        PragmaValue::Integer(i) => *i,
        PragmaValue::Ident(s) | PragmaValue::String(s) => s.trim().parse::<i64>().unwrap_or(0),
    }
}

fn pragma_to_bool(pv: &PragmaValue) -> bool {
    match pv {
        PragmaValue::Integer(i) => *i != 0,
        PragmaValue::Ident(s) | PragmaValue::String(s) => matches!(
            s.trim().to_ascii_uppercase().as_str(),
            "1" | "ON" | "TRUE" | "YES"
        ),
    }
}

fn pragma_to_string(pv: &PragmaValue) -> String {
    match pv {
        PragmaValue::Integer(i) => i.to_string(),
        PragmaValue::Ident(s) | PragmaValue::String(s) => s.clone(),
    }
}
