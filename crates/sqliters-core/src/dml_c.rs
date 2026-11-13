//! Decisions that C SQLite takes while it *generates code* for a DML statement and that are
//! observable afterwards (row visit order, statement journal, what a failed statement leaves
//! behind). This engine interprets statements instead of generating VDBE programs, so the same
//! decisions are taken here from the statement and the schema:
//!
//! * the pass strategy of `sqlite3DeleteFrom` / `sqlite3Update` (`eOnePass`, delete.c / update.c),
//! * `sqlite3FkRequired` (fkey.c),
//! * `Parse.isMultiWrite` / `Parse.mayAbort` (`sqlite3BeginWriteOperation`, `sqlite3MultiWrite`,
//!   `sqlite3MayAbort`, build.c), whose conjunction is `Vdbe.usesStmtJournal`.

use crate::compiler::AccessPlan;
use crate::db_set::DatabaseSet;
use crate::schema::{IndexSchema, TableSchema};
use crate::trigger::{TriggerEventKind, TriggerSchema};
use sqliters_syntax::ast::*;
use sqliters_types::{DatabaseId, Pgno};

/// `ONEPASS_OFF` / `ONEPASS_SINGLE` / `ONEPASS_MULTI` (sqliteInt.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnePass {
    /// Two passes: the keys of the matching rows are collected first (RowSet / ephemeral table,
    /// read back in rowid / PRIMARY KEY order), then each row is processed.
    Off,
    /// The WHERE clause names at most one row.
    Single,
    /// Rows are processed while the scan is positioned on them, in scan order.
    Multi,
}

impl OnePass {
    /// (name used by the decision trace)
    #[allow(dead_code)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "two-pass",
            Self::Single => "onepass-single",
            Self::Multi => "onepass-multi",
        }
    }
}

/// What `sqlite3WhereOkOnePass` reports about the single-table loop of a DELETE / UPDATE.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ScanInfo {
    /// `WHERE_ONEROW`: rowid equality, or equality on every column of a UNIQUE index.
    pub one_row: bool,
    /// `aiCurOnePass[1]`: root of the secondary index the loop runs on, if any.
    pub index_root: Option<Pgno>,
    /// `WHERE_MULTI_OR`: the loop is a union of index scans (OR optimisation). An UPDATE then
    /// cannot run in one pass (a DELETE can: it passes `WHERE_DUPLICATES_OK`).
    pub multi_or: bool,
}

/// `Index.uniqNotNull`: a UNIQUE index all of whose key columns are NOT NULL table columns.
fn index_uniq_not_null(tbl: &TableSchema, idx: &IndexSchema) -> bool {
    idx.unique
        && idx.columns.iter().all(|c| match c.col_idx {
            Some(ci) => {
                tbl.rowid_alias_col == Some(ci)
                    || tbl.columns.get(ci).is_some_and(|col| col.not_null.is_some())
                    || (tbl.without_rowid && tbl.columns.get(ci).is_some_and(|col| col.is_primary_key))
            }
            None => false,
        })
}

/// `WHERE_ONEROW` of the loop C's planner picks for the one table of a DELETE / UPDATE, derived
/// from the access plan (where.c `whereShortCut` and `whereLoopAddBtreeIndex`).
pub(crate) fn scan_info(plan: &AccessPlan, tbl: &TableSchema, hinted: bool) -> ScanInfo {
    match plan {
        AccessPlan::RowidSeek { .. } => ScanInfo {
            one_row: true,
            index_root: None,
            multi_or: false,
        },
        // parse.y: `x IN (y)` with a single constant right-hand side is rewritten to `x == y`.
        AccessPlan::RowidIn { list } if list.len() == 1 => ScanInfo {
            one_row: true,
            index_root: None,
            multi_or: false,
        },
        AccessPlan::MultiIndexOr { .. } => ScanInfo {
            one_row: false,
            index_root: None,
            multi_or: true,
        },
        AccessPlan::IndexSeek {
            index,
            prefix_exprs,
            ..
        } => {
            let is_pk_tree = tbl.without_rowid && index.root_pgno == tbl.root_pgno;
            let all_eq = index.unique && prefix_exprs.len() == index.columns.len();
            // whereShortCut: any UNIQUE, non-partial index of at most 3 columns whose columns are
            // all constrained by `==` (not when the statement carries INDEXED BY / NOT INDEXED).
            let shortcut = !hinted && index.where_clause.is_none() && index.columns.len() <= 3;
            // whereLoopAddBtreeIndex: uniqNotNull, or a one-column UNIQUE index with `==`.
            let general = index_uniq_not_null(tbl, index) || index.columns.len() == 1;
            ScanInfo {
                one_row: all_eq && (shortcut || general || is_pk_tree),
                index_root: (!is_pk_tree).then_some(index.root_pgno),
                multi_or: false,
            }
        }
        AccessPlan::IndexFullScan { index, .. } | AccessPlan::IndexRangeScan { index, .. } => {
            let is_pk_tree = tbl.without_rowid && index.root_pgno == tbl.root_pgno;
            ScanInfo {
                one_row: false,
                index_root: (!is_pk_tree).then_some(index.root_pgno),
                multi_or: false,
            }
        }
        AccessPlan::FullScan | AccessPlan::RowidRange { .. } | AccessPlan::RowidIn { .. } => {
            ScanInfo::default()
        }
    }
}

/// where.c `whereShortCut`, rowid case, for a right-hand side this engine's access planner does
/// not turn into a seek (e.g. a scalar sub-select): the WHERE clause has a top-level conjunct
/// `rowid = X` / `X = rowid` (or `IS`) where X mentions no column outside a sub-select
/// (`prereqRight == 0`). The loop is then `WHERE_ONEROW` whatever else the clause says.
pub(crate) fn where_names_one_rowid(
    tbl: &TableSchema,
    // (all indexes of the schema: the ones of `tbl` are picked by name)
    indexes: &[IndexSchema],
    alias: Option<&str>,
    selection: &Expr,
) -> bool {
    // Columns that alone are the key of a UNIQUE, non-partial index (or of the PRIMARY KEY of
    // a WITHOUT ROWID table): `col = X` on one of them is one row as well.
    let mut uniq_cols: Vec<usize> = indexes
        .iter()
        .filter(|i| {
            i.unique
                && i.where_clause.is_none()
                && i.columns.len() == 1
                && i.table_name.eq_ignore_ascii_case(&tbl.name)
        })
        .filter_map(|i| i.columns[0].col_idx)
        .collect();
    if tbl.without_rowid
        && tbl.without_rowid_pk_cols.len() == 1
        && let Some(ci) = tbl.without_rowid_pk_cols[0].col_idx
    {
        uniq_cols.push(ci);
    }
    fn conjuncts<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
        if let Expr::Binary {
            left,
            op: BinaryOp::And,
            right,
        } = e
        {
            conjuncts(left, out);
            conjuncts(right, out);
        } else {
            out.push(e);
        }
    }
    let is_rowid = |e: &Expr| -> bool {
        let Expr::Column(c) = e else { return false };
        if c.schema.is_some() {
            return false;
        }
        if let Some(t) = &c.table
            && !t.eq_ignore_ascii_case(alias.unwrap_or(&tbl.name))
        {
            return false;
        }
        match tbl.find_column(&c.column) {
            Some((ci, _)) => tbl.rowid_alias_col == Some(ci) || uniq_cols.contains(&ci),
            None => !tbl.without_rowid && tbl.is_implicit_rowid_name(&c.column),
        }
    };
    let no_columns = |e: &Expr| -> bool {
        let mut ok = true;
        walk_expr(e, &mut |x| {
            if matches!(x, Expr::Column(_)) {
                ok = false;
            }
            Walk::Shallow
        });
        ok
    };
    let mut terms = Vec::new();
    conjuncts(selection, &mut terms);
    terms.iter().any(|t| match t {
        Expr::Binary {
            left,
            op: BinaryOp::Eq | BinaryOp::Is,
            right,
        } => (is_rowid(left) && no_columns(right)) || (is_rowid(right) && no_columns(left)),
        // parse.y: `x IN (y)` with one constant is `x == y`.
        Expr::InList {
            expr,
            list,
            negated: false,
        } => list.len() == 1 && is_rowid(expr) && no_columns(&list[0]),
        _ => false,
    })
}

/// `eOnePass` of `sqlite3DeleteFrom` (delete.c): `WHERE_ONEPASS_DESIRED`, plus
/// `WHERE_ONEPASS_MULTIROW` unless `bComplex` (triggers incl. RETURNING, foreign keys, a
/// sub-query in the WHERE clause).
pub(crate) fn delete_one_pass(scan: ScanInfo, complex: bool) -> OnePass {
    if scan.one_row {
        OnePass::Single
    } else if complex {
        OnePass::Off
    } else {
        OnePass::Multi
    }
}

/// `eOnePass` of `sqlite3Update` (update.c): `WHERE_ONEPASS_MULTIROW` only without triggers,
/// foreign keys, a key change, REPLACE conflict handling and sub-queries in the WHERE clause;
/// and "a one-pass strategy that might update more than one row may not be used if any column
/// of the index used for the scan is being updated".
pub(crate) fn update_one_pass(scan: ScanInfo, multirow_ok: bool, scan_index_rewritten: bool) -> OnePass {
    if scan.one_row {
        OnePass::Single
    } else if !multirow_ok || scan_index_rewritten || scan.multi_or {
        OnePass::Off
    } else {
        OnePass::Multi
    }
}

// ---------------------------------------------------------------------------------------------
// Expression walkers
// ---------------------------------------------------------------------------------------------

/// `EP_Subquery` / `NC_Subquery`: the expression contains a sub-select.
pub(crate) fn expr_has_subquery(e: &Expr) -> bool {
    let mut found = false;
    walk_expr(e, &mut |x| {
        if matches!(
            x,
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } | Expr::InTable { .. }
        ) {
            found = true;
        }
        Walk::Shallow
    });
    found
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub(crate) enum Walk {
    /// Do not descend into sub-selects (the callback sees the node holding one).
    Shallow,
    /// Descend into sub-selects as well.
    Deep,
}

/// Calls `f` on `e` and every sub-expression; `f`'s result says whether to enter sub-selects
/// hanging off the node.
pub(crate) fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr) -> Walk) {
    let deep = f(e) == Walk::Deep;
    match e {
        Expr::Unary { expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::IsNull { expr, .. } => walk_expr(expr, f),
        Expr::Binary { left, right, .. } => {
            walk_expr(left, f);
            walk_expr(right, f);
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            walk_expr(expr, f);
            walk_expr(low, f);
            walk_expr(high, f);
        }
        Expr::InList { expr, list, .. } => {
            walk_expr(expr, f);
            for x in list {
                walk_expr(x, f);
            }
        }
        Expr::InSubquery { expr, subquery, .. } => {
            walk_expr(expr, f);
            if deep {
                walk_select(subquery, f);
            }
        }
        Expr::InTable { expr, .. } => walk_expr(expr, f),
        Expr::Like {
            expr,
            pattern,
            escape,
            ..
        } => {
            walk_expr(expr, f);
            walk_expr(pattern, f);
            if let Some(x) = escape {
                walk_expr(x, f);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(x) = operand {
                walk_expr(x, f);
            }
            for (w, t) in branches {
                walk_expr(w, f);
                walk_expr(t, f);
            }
            if let Some(x) = else_expr {
                walk_expr(x, f);
            }
        }
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if let FunctionArgs::Exprs(es) = args {
                for x in es {
                    walk_expr(x, f);
                }
            }
            if let Some(x) = filter {
                walk_expr(x, f);
            }
            if let Some(WindowRef::Spec(ws)) = over {
                walk_window_spec(ws, f);
            }
        }
        Expr::Subquery(s) | Expr::Exists { subquery: s, .. } => {
            if deep {
                walk_select(s, f);
            }
        }
        Expr::Raise { message, .. } => {
            if let Some(m) = message {
                walk_expr(m, f);
            }
        }
        Expr::RowValue(es) => {
            for x in es {
                walk_expr(x, f);
            }
        }
        _ => {}
    }
}

fn walk_window_spec(ws: &WindowSpec, f: &mut dyn FnMut(&Expr) -> Walk) {
    for x in &ws.partition_by {
        walk_expr(x, f);
    }
    for o in &ws.order_by {
        walk_expr(&o.expr, f);
    }
    if let Some(fr) = &ws.frame {
        for b in [&fr.start, &fr.end] {
            if let WindowFrameBound::Preceding(x) | WindowFrameBound::Following(x) = b {
                walk_expr(x, f);
            }
        }
    }
}

/// Every expression of a SELECT, sub-selects in FROM included.
pub(crate) fn walk_select(sel: &SelectStmt, f: &mut dyn FnMut(&Expr) -> Walk) {
    for cte in &sel.ctes {
        walk_select(&cte.query, f);
    }
    let core = |c: &SelectCore, f: &mut dyn FnMut(&Expr) -> Walk| match c {
        SelectCore::Values(rows) => {
            for r in rows {
                for x in r {
                    walk_expr(x, f);
                }
            }
        }
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            windows,
            ..
        } => {
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    walk_expr(expr, f);
                }
            }
            if let Some(fc) = from {
                walk_from(fc, f);
            }
            if let Some(x) = selection {
                walk_expr(x, f);
            }
            for x in group_by {
                walk_expr(x, f);
            }
            if let Some(x) = having {
                walk_expr(x, f);
            }
            for (_, ws) in windows {
                walk_window_spec(ws, f);
            }
        }
    };
    core(&sel.body.head, f);
    for (_, c) in &sel.body.compounds {
        core(c, f);
    }
    for o in &sel.order_by {
        walk_expr(&o.expr, f);
    }
    if let Some(l) = &sel.limit {
        walk_expr(&l.limit, f);
        if let Some(o) = &l.offset {
            walk_expr(o, f);
        }
    }
}

fn walk_from(fc: &FromClause, f: &mut dyn FnMut(&Expr) -> Walk) {
    let tos = |t: &TableOrSubquery, f: &mut dyn FnMut(&Expr) -> Walk| match t {
        TableOrSubquery::Table { table_args, .. } => {
            if let Some(es) = table_args {
                for x in es {
                    walk_expr(x, f);
                }
            }
        }
        TableOrSubquery::TableFunction { args, .. } => {
            for x in args {
                walk_expr(x, f);
            }
        }
        TableOrSubquery::Subquery { query, .. } => walk_select(query, f),
    };
    tos(&fc.head, f);
    for j in &fc.joins {
        tos(&j.right, f);
        if let Some(JoinConstraint::On(x)) = &j.constraint {
            walk_expr(x, f);
        }
    }
}

/// Names of the tables / views a SELECT reads in its FROM clauses (sub-selects included).
fn select_from_names(sel: &SelectStmt, out: &mut Vec<QualifiedName>) {
    fn from(fc: &FromClause, out: &mut Vec<QualifiedName>) {
        let tos = |t: &TableOrSubquery, out: &mut Vec<QualifiedName>| match t {
            TableOrSubquery::Table { name, .. } => out.push(name.clone()),
            TableOrSubquery::Subquery { query, .. } => select_from_names(query, out),
            TableOrSubquery::TableFunction { .. } => {}
        };
        tos(&fc.head, out);
        for j in &fc.joins {
            tos(&j.right, out);
        }
    }
    for cte in &sel.ctes {
        select_from_names(&cte.query, out);
    }
    for c in std::iter::once(&sel.body.head).chain(sel.body.compounds.iter().map(|(_, c)| c)) {
        if let SelectCore::Select { from: Some(fc), .. } = c {
            from(fc, out);
        }
    }
    // sub-selects inside expressions
    let mut subs: Vec<&SelectStmt> = Vec::new();
    walk_select_shallow_subs(sel, &mut subs);
    for s in subs {
        select_from_names(s, out);
    }
}

fn walk_select_shallow_subs<'a>(sel: &'a SelectStmt, out: &mut Vec<&'a SelectStmt>) {
    fn ex<'a>(e: &'a Expr, out: &mut Vec<&'a SelectStmt>) {
        match e {
            Expr::Subquery(s) | Expr::Exists { subquery: s, .. } => out.push(s),
            Expr::InSubquery { expr, subquery, .. } => {
                ex(expr, out);
                out.push(subquery);
            }
            Expr::Unary { expr, .. }
            | Expr::Collate { expr, .. }
            | Expr::Cast { expr, .. }
            | Expr::IsNull { expr, .. }
            | Expr::InTable { expr, .. } => ex(expr, out),
            Expr::Binary { left, right, .. } => {
                ex(left, out);
                ex(right, out);
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                ex(expr, out);
                ex(low, out);
                ex(high, out);
            }
            Expr::InList { expr, list, .. } => {
                ex(expr, out);
                for x in list {
                    ex(x, out);
                }
            }
            Expr::Like {
                expr,
                pattern,
                escape,
                ..
            } => {
                ex(expr, out);
                ex(pattern, out);
                if let Some(x) = escape {
                    ex(x, out);
                }
            }
            Expr::Case {
                operand,
                branches,
                else_expr,
            } => {
                if let Some(x) = operand {
                    ex(x, out);
                }
                for (w, t) in branches {
                    ex(w, out);
                    ex(t, out);
                }
                if let Some(x) = else_expr {
                    ex(x, out);
                }
            }
            Expr::FunctionCall { args, filter, .. } => {
                if let FunctionArgs::Exprs(es) = args {
                    for x in es {
                        ex(x, out);
                    }
                }
                if let Some(x) = filter {
                    ex(x, out);
                }
            }
            Expr::RowValue(es) => {
                for x in es {
                    ex(x, out);
                }
            }
            _ => {}
        }
    }
    for c in std::iter::once(&sel.body.head).chain(sel.body.compounds.iter().map(|(_, c)| c)) {
        match c {
            SelectCore::Values(rows) => {
                for r in rows {
                    for x in r {
                        ex(x, out);
                    }
                }
            }
            SelectCore::Select {
                projection,
                selection,
                group_by,
                having,
                from,
                ..
            } => {
                for rc in projection {
                    if let ResultColumn::Expr { expr, .. } = rc {
                        ex(expr, out);
                    }
                }
                if let Some(x) = selection {
                    ex(x, out);
                }
                for x in group_by {
                    ex(x, out);
                }
                if let Some(x) = having {
                    ex(x, out);
                }
                if let Some(fc) = from {
                    for j in &fc.joins {
                        if let Some(JoinConstraint::On(x)) = &j.constraint {
                            ex(x, out);
                        }
                    }
                }
            }
        }
    }
    for o in &sel.order_by {
        ex(&o.expr, out);
    }
}

/// Registers (address, no value yet) every scalar sub-select of `e` that provably does not
/// refer to anything outside itself — the ones C evaluates once per statement (`OP_Once`, no
/// `EP_VarSelect`). The test is deliberately narrow: a FROM clause of plain base tables only,
/// no CTEs, no nested sub-selects, and every column reference names a column of one of those
/// tables. Anything else is left to per-row evaluation.
pub(crate) fn collect_uncorrelated_scalar_subqueries(
    db_set: &DatabaseSet,
    e: &Expr,
    out: &mut Vec<(usize, Option<sqliters_types::Value>)>,
) {
    let mut found: Vec<&SelectStmt> = Vec::new();
    fn top_subs<'a>(e: &'a Expr, out: &mut Vec<&'a SelectStmt>) {
        match e {
            Expr::Subquery(s) => out.push(s),
            Expr::Unary { expr, .. }
            | Expr::Collate { expr, .. }
            | Expr::Cast { expr, .. }
            | Expr::IsNull { expr, .. } => top_subs(expr, out),
            Expr::Binary { left, right, .. } => {
                top_subs(left, out);
                top_subs(right, out);
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                top_subs(expr, out);
                top_subs(low, out);
                top_subs(high, out);
            }
            Expr::InList { expr, list, .. } => {
                top_subs(expr, out);
                for x in list {
                    top_subs(x, out);
                }
            }
            Expr::Case {
                operand,
                branches,
                else_expr,
            } => {
                if let Some(x) = operand {
                    top_subs(x, out);
                }
                for (w, t) in branches {
                    top_subs(w, out);
                    top_subs(t, out);
                }
                if let Some(x) = else_expr {
                    top_subs(x, out);
                }
            }
            Expr::FunctionCall {
                args: FunctionArgs::Exprs(es),
                over: None,
                ..
            } => {
                for x in es {
                    top_subs(x, out);
                }
            }
            _ => {}
        }
    }
    top_subs(e, &mut found);
    for sel in found {
        if select_is_self_contained(db_set, sel) {
            let key = sel as *const SelectStmt as usize;
            if !out.iter().any(|(k, _)| *k == key) {
                out.push((key, None));
            }
        }
    }
}

fn select_is_self_contained(db_set: &DatabaseSet, sel: &SelectStmt) -> bool {
    if !sel.ctes.is_empty() || !sel.body.compounds.is_empty() {
        return false;
    }
    let SelectCore::Select {
        from: Some(fc),
        windows,
        projection,
        ..
    } = &sel.body.head
    else {
        return false;
    };
    if !windows.is_empty()
        || projection
            .iter()
            .any(|rc| !matches!(rc, ResultColumn::Expr { .. }))
    {
        return false;
    }
    // inner tables: (name or alias, column names)
    let mut inner: Vec<(String, Vec<String>, bool)> = Vec::new();
    for t in std::iter::once(&fc.head).chain(fc.joins.iter().map(|j| &j.right)) {
        let TableOrSubquery::Table {
            name,
            table_args: None,
            alias,
            ..
        } = t
        else {
            return false;
        };
        let Ok(Some((_, tbl))) = db_set.find_table_in_set(name) else {
            return false;
        };
        inner.push((
            alias.clone().unwrap_or_else(|| name.name.clone()),
            tbl.columns.iter().map(|c| c.name.clone()).collect(),
            !tbl.without_rowid,
        ));
    }
    if fc
        .joins
        .iter()
        .any(|j| matches!(j.constraint, Some(JoinConstraint::Using(_))) || j.natural)
    {
        return false;
    }
    let mut ok = true;
    walk_select(sel, &mut |x| {
        match x {
            Expr::Subquery(_)
            | Expr::Exists { .. }
            | Expr::InSubquery { .. }
            | Expr::InTable { .. }
            | Expr::Raise { .. }
            | Expr::Bind(_) => ok = false,
            Expr::Column(c) => {
                let is_rowid_name = |n: &str| {
                    ["rowid", "oid", "_rowid_"].iter().any(|r| n.eq_ignore_ascii_case(r))
                };
                let hit = inner.iter().any(|(tname, cols, has_rowid)| {
                    c.table.as_ref().is_none_or(|t| t.eq_ignore_ascii_case(tname))
                        && (cols.iter().any(|n| n.eq_ignore_ascii_case(&c.column))
                            || (*has_rowid && is_rowid_name(&c.column)))
                });
                if !hit || c.schema.is_some() {
                    ok = false;
                }
            }
            _ => {}
        }
        Walk::Shallow
    });
    ok
}

// ---------------------------------------------------------------------------------------------
// sqlite3FkRequired (fkey.c)
// ---------------------------------------------------------------------------------------------

/// Whether any table of the database holding `tbl` declares a foreign key that references it
/// (`sqlite3FkReferences(pTab) != 0`).
pub(crate) fn table_is_fk_parent(db_set: &DatabaseSet, db_id: DatabaseId, tbl_name: &str) -> bool {
    db_set.slot(db_id).is_some_and(|s| {
        s.schema.tables.iter().any(|t| {
            t.foreign_keys
                .iter()
                .any(|fk| fk.ref_table.eq_ignore_ascii_case(tbl_name))
        })
    })
}

/// `sqlite3FkRequired(pParse, pTab, 0, 0)`: a DELETE needs foreign key processing when foreign
/// keys are enabled and the table is the child or the parent of any constraint.
pub(crate) fn fk_required_delete(
    db_set: &DatabaseSet,
    db_id: DatabaseId,
    tbl: &TableSchema,
    fk_on: bool,
) -> bool {
    fk_on && (!tbl.foreign_keys.is_empty() || table_is_fk_parent(db_set, db_id, &tbl.name))
}

/// `sqlite3FkRequired(pParse, pTab, aChange, chngRowid)` for an UPDATE: 0 = no foreign key
/// processing, 1 = processing needed, 2 = "the update is a self-referencing child key change or
/// a parent key change with an action": the row is deleted and re-inserted.
pub(crate) fn fk_required_update(
    db_set: &DatabaseSet,
    db_id: DatabaseId,
    tbl: &TableSchema,
    fk_on: bool,
    modified_col: &[bool],
    chng_rowid: bool,
) -> u8 {
    if !fk_on {
        return 0;
    }
    let changed = |ci: usize| -> bool {
        modified_col.get(ci).copied().unwrap_or(false)
            || (chng_rowid && tbl.rowid_alias_col == Some(ci))
    };
    let mut ret = 1u8;
    let mut have = false;
    // child keys being modified
    for fk in &tbl.foreign_keys {
        if fk.from_cols.iter().any(|&ci| changed(ci)) {
            if fk.ref_table.eq_ignore_ascii_case(&tbl.name) {
                ret = 2;
            }
            have = true;
        }
    }
    // parent keys being modified
    if let Some(slot) = db_set.slot(db_id) {
        for ctbl in &slot.schema.tables {
            for fk in &ctbl.foreign_keys {
                if !fk.ref_table.eq_ignore_ascii_case(&tbl.name) {
                    continue;
                }
                if fk_parent_is_modified(tbl, fk, &changed) {
                    if fk.on_update != FkAction::NoAction {
                        return 2;
                    }
                    have = true;
                }
            }
        }
    }
    if have { ret } else { 0 }
}

/// fkey.c `fkParentIsModified`.
pub(crate) fn fk_parent_is_modified(
    tbl: &TableSchema,
    fk: &crate::schema::ForeignKeySchema,
    changed: &dyn Fn(usize) -> bool,
) -> bool {
    for i in 0..fk.from_cols.len() {
        let zkey = fk.ref_cols.as_ref().and_then(|c| c.get(i));
        for (ci, col) in tbl.columns.iter().enumerate() {
            if !changed(ci) {
                continue;
            }
            match zkey {
                Some(k) => {
                    if col.name.eq_ignore_ascii_case(k) {
                        return true;
                    }
                }
                None => {
                    if col.is_primary_key {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// fkey.c `fkChildIsModified`.
pub(crate) fn fk_child_is_modified(
    tbl: &TableSchema,
    fk: &crate::schema::ForeignKeySchema,
    modified_col: &[bool],
    chng_rowid: bool,
) -> bool {
    fk.from_cols.iter().any(|&ci| {
        modified_col.get(ci).copied().unwrap_or(false)
            || (chng_rowid && tbl.rowid_alias_col == Some(ci))
    })
}

// ---------------------------------------------------------------------------------------------
// Parse.isMultiWrite / Parse.mayAbort
// ---------------------------------------------------------------------------------------------

/// `Parse.isMultiWrite` and `Parse.mayAbort` of the top-level statement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StmtFlags {
    pub multi_write: bool,
    pub may_abort: bool,
}

impl StmtFlags {
    /// `Vdbe.usesStmtJournal` (vdbeaux.c `sqlite3VdbeMakeReady`).
    pub(crate) const fn uses_stmt_journal(self) -> bool {
        self.multi_write && self.may_abort
    }

    /// Both set: used wherever the statement shape is one this analysis does not model.
    const ALL: Self = Self {
        multi_write: true,
        may_abort: true,
    };
}

/// Functions C codes inline (`INLINE_FUNC` in func.c) — no `OP_Function`, hence no
/// `sqlite3MayAbort` from `sqlite3VdbeAddFunctionCall`.
fn is_inline_function(name: &str) -> bool {
    ["coalesce", "ifnull", "iif", "if", "likely", "unlikely", "likelihood", "sqlite_offset"]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

/// Built-in aggregates (`OP_AggStep`, not `sqlite3VdbeAddFunctionCall`). `min` / `max` are
/// aggregates only with one argument.
fn is_aggregate_call(name: &str, nargs: usize, star: bool) -> bool {
    let n = name.to_ascii_lowercase();
    match n.as_str() {
        "count" => true,
        "sum" | "total" | "avg" | "json_group_array" | "jsonb_group_array" | "median" => nargs == 1,
        "min" | "max" => nargs == 1 && !star,
        "group_concat" | "string_agg" | "json_group_object" | "jsonb_group_object" | "percentile"
        | "percentile_cont" | "percentile_disc" => nargs == 1 || nargs == 2,
        _ => false,
    }
}

struct Analyzer<'a> {
    db_set: &'a DatabaseSet,
    fk_on: bool,
    rec_triggers: bool,
    defer_fks: bool,
    f: StmtFlags,
    /// Trigger programs already coded for this statement (`Parse.pTriggerPrg`): one per trigger
    /// and conflict override.
    coded_triggers: Vec<(String, Option<ConflictAction>)>,
    /// Foreign key action programs already coded (`FKey.apTrigger[]`): child table, constraint,
    /// update?.
    coded_fk_actions: Vec<(String, usize, bool)>,
    /// Nesting depth: 0 while the top-level statement is coded (`pParse->pToplevel == 0`).
    depth: u32,
    /// Views being expanded (guards against a cyclic definition).
    view_stack: Vec<String>,
    /// Set while the UPDATE of a SET NULL action is coded: (child table, constraint id).
    set_null_fk: Option<(String, usize)>,
    /// `Parse.pAinc`: the AUTOINCREMENT tables an INSERT is coded for (anywhere in the
    /// statement's program, triggers included).
    autoinc: Vec<(DatabaseId, String)>,
}

impl Analyzer<'_> {
    fn may_abort(&mut self) {
        self.f.may_abort = true;
    }

    fn multi_write(&mut self) {
        self.f.multi_write = true;
    }

    /// Codes an expression: every scalar function call is `sqlite3VdbeAddFunctionCall`
    /// (`sqlite3MayAbort`), `RAISE(ABORT, ..)` is expr.c:5878, sub-selects are coded in place.
    fn expr(&mut self, e: &Expr) {
        let mut subs: Vec<&SelectStmt> = Vec::new();
        let mut abort = false;
        collect_expr(e, &mut abort, &mut subs);
        if abort {
            self.may_abort();
        }
        for s in subs {
            self.select(s);
        }
    }

    fn exprs<'e>(&mut self, es: impl IntoIterator<Item = &'e Expr>) {
        for e in es {
            self.expr(e);
        }
    }

    /// Codes a SELECT (`sqlite3Select`): its expressions, the views it reads, and
    /// select.c:2616/2624 — a LIMIT / OFFSET that is not an integer constant is checked with
    /// `OP_MustBeInt` (`sqlite3MayAbort`).
    fn select(&mut self, sel: &SelectStmt) {
        let mut abort = false;
        let mut subs: Vec<&SelectStmt> = Vec::new();
        walk_select(sel, &mut |x| {
            collect_node(x, &mut abort);
            Walk::Deep
        });
        let _ = &mut subs;
        if abort {
            self.may_abort();
        }
        self.select_limits(sel);
        let mut names = Vec::new();
        select_from_names(sel, &mut names);
        for qn in names {
            self.view_if_any(&qn);
        }
    }

    fn select_limits(&mut self, sel: &SelectStmt) {
        fn all<'a>(sel: &'a SelectStmt, out: &mut Vec<&'a SelectStmt>) {
            out.push(sel);
            for cte in &sel.ctes {
                all(&cte.query, out);
            }
            let mut subs = Vec::new();
            walk_select_shallow_subs(sel, &mut subs);
            for s in subs {
                all(s, out);
            }
            for c in std::iter::once(&sel.body.head).chain(sel.body.compounds.iter().map(|(_, c)| c))
            {
                if let SelectCore::Select { from: Some(fc), .. } = c {
                    for t in std::iter::once(&fc.head).chain(fc.joins.iter().map(|j| &j.right)) {
                        if let TableOrSubquery::Subquery { query, .. } = t {
                            all(query, out);
                        }
                    }
                }
            }
        }
        let mut sels = Vec::new();
        all(sel, &mut sels);
        for s in sels {
            if let Some(l) = &s.limit {
                if !matches!(l.limit, Expr::Integer(_)) {
                    self.may_abort();
                }
                if l.offset.is_some() {
                    self.may_abort();
                }
            }
        }
    }

    /// A view read by a query is expanded in place: its SELECT is coded too.
    fn view_if_any(&mut self, qn: &QualifiedName) {
        let Some((_, view)) = self.db_set.find_view_in_set(qn) else {
            return;
        };
        if self.view_stack.iter().any(|v| v.eq_ignore_ascii_case(&view.name)) {
            return;
        }
        let sel = view.select.clone();
        self.view_stack.push(view.name.clone());
        self.select(&sel);
        self.view_stack.pop();
    }

    fn triggers_for(
        &self,
        db_id: DatabaseId,
        name: &str,
        kind: TriggerEventKind,
        cols: Option<&[&str]>,
    ) -> Vec<TriggerSchema> {
        self.db_set
            .triggers_for_relation(db_id, name)
            .into_iter()
            .filter(|(_, tr)| tr.matches_event_and_columns(kind, cols))
            .map(|(_, tr)| tr)
            .collect()
    }

    /// `sqlite3CodeRowTrigger`: every matching trigger's program is coded once per conflict
    /// override (`getRowTrigger`); its statements set the flags of the top-level parse.
    fn code_triggers(&mut self, trigs: &[TriggerSchema], orconf: Option<ConflictAction>) {
        for tr in trigs {
            let key = (tr.name.to_ascii_lowercase(), orconf);
            if self.coded_triggers.contains(&key) {
                continue;
            }
            self.coded_triggers.push(key);
            self.depth += 1;
            if let Some(w) = &tr.when_clause {
                self.expr(w);
            }
            for step in &tr.body {
                match step {
                    TriggerStep::Select(s) => self.select(s),
                    TriggerStep::Insert(i) => {
                        let oc = orconf.or(i.or_conflict);
                        self.insert(i, oc);
                    }
                    TriggerStep::Update(u) => {
                        let oc = orconf.or(u.or_conflict);
                        self.update(u, oc);
                    }
                    TriggerStep::Delete(d) => self.delete(d),
                }
            }
            self.depth -= 1;
        }
    }

    /// `pTab->pIndex`: the indexes of the table — for a WITHOUT ROWID table including its
    /// PRIMARY KEY (the table b-tree itself).
    fn indexes_of(&self, db_id: DatabaseId, tbl: &TableSchema) -> Vec<IndexSchema> {
        let mut v = self
            .db_set
            .slot(db_id)
            .map(|s| s.schema.indexes_for_table(&tbl.name))
            .unwrap_or_default();
        if tbl.without_rowid
            && !v.iter().any(|i| i.root_pgno == tbl.root_pgno)
            && let Some(pk) = tbl.without_rowid_pk_as_index()
        {
            v.push(pk);
        }
        v
    }

    fn plan_scan(
        &self,
        db_id: DatabaseId,
        tbl: &TableSchema,
        alias: Option<&str>,
        hint: Option<&IndexHint>,
        selection: Option<&Expr>,
    ) -> ScanInfo {
        let Some(slot) = self.db_set.slot(db_id) else {
            return ScanInfo::default();
        };
        let plan = crate::compiler::choose_access_plan_with_context(
            tbl,
            &slot.name,
            alias,
            selection,
            &slot.schema,
            crate::compiler::QueryPlanContext {
                indexed_by: hint,
                ..Default::default()
            },
        );
        let mut scan = scan_info(&plan, tbl, hint.is_some());
        if hint.is_none()
            && !scan.one_row
            && selection.is_some_and(|w| {
                where_names_one_rowid(tbl, &slot.schema.indexes, alias, w)
            })
        {
            scan.one_row = true;
            scan.multi_or = false;
            scan.index_root = None;
        }
        scan
    }

    // ------------------------------------------------------------------ DELETE (delete.c)
    fn delete(&mut self, del: &DeleteStmt) {
        let top = self.depth == 0;
        for cte in &del.ctes {
            self.select(&cte.query);
        }
        let Ok(Some(rel)) = self.db_set.find_relation_in_set(&del.table) else {
            self.f = StmtFlags::ALL;
            return;
        };
        let (db_id, tbl) = match rel {
            (db_id, crate::db_set::ResolvedRelation::Table(t)) => (db_id, *t),
            (db_id, crate::db_set::ResolvedRelation::View(v)) => {
                // DELETE on a view: materialise the view, fire the INSTEAD OF triggers.
                let trigs = self.triggers_for(db_id, &v.name, TriggerEventKind::Delete, None);
                self.multi_write(); // bComplex (pTrigger); eOnePass is never SINGLE on the materialised view
                let sel = v.select.clone();
                self.select(&sel);
                if let Some(w) = &del.selection {
                    self.expr(w);
                }
                for rc in &del.returning {
                    if let ResultColumn::Expr { expr, .. } = rc {
                        self.expr(expr);
                    }
                }
                self.code_triggers(&trigs, None);
                return;
            }
        };
        if tbl.root_pgno == 0 {
            // delete.c:637 — OP_VUpdate: sqlite3MayAbort; one-row deletes clear isMultiWrite at
            // the top level (not modelled: keep the statement journal).
            self.f = StmtFlags::ALL;
            return;
        }
        let trigs = self.triggers_for(db_id, &tbl.name, TriggerEventKind::Delete, None);
        let has_trigger = !trigs.is_empty() || (top && !del.returning.is_empty());
        let fk = fk_required_delete(self.db_set, db_id, &tbl, self.fk_on);
        let mut complex = has_trigger || fk;
        // sqlite3BeginWriteOperation(pParse, bComplex, iDb)
        if complex {
            self.multi_write();
        }
        if del.selection.is_none() && !complex {
            // truncate optimisation: OP_Clear only.
            return;
        }
        if let Some(w) = &del.selection {
            self.expr(w);
            if expr_has_subquery(w) {
                complex = true;
            }
        }
        let scan = self.plan_scan(
            db_id,
            &tbl,
            del.alias.as_deref(),
            del.indexed_by.as_ref(),
            del.selection.as_ref(),
        );
        if delete_one_pass(scan, complex) != OnePass::Single {
            self.multi_write(); // delete.c:532
        }
        self.row_delete(db_id, &tbl, &trigs, None);
        if top {
            for rc in &del.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    self.expr(expr);
                }
            }
        }
    }

    /// `sqlite3GenerateRowDelete`: BEFORE triggers, `sqlite3FkCheck` for the old row, the
    /// deletes, `sqlite3FkActions`, AFTER triggers.
    fn row_delete(
        &mut self,
        db_id: DatabaseId,
        tbl: &TableSchema,
        trigs: &[TriggerSchema],
        orconf: Option<ConflictAction>,
    ) {
        self.code_triggers(trigs, orconf);
        if fk_required_delete(self.db_set, db_id, tbl, self.fk_on) {
            self.fk_check_old(db_id, tbl, None, false);
            self.fk_actions(db_id, tbl, None, false);
        }
        // sqlite3GenerateRowIndexDelete -> sqlite3GenerateIndexKey codes indexed expressions and
        // partial-index WHERE clauses.
        for idx in self.indexes_of(db_id, tbl) {
            self.index_key_exprs(&idx);
        }
    }

    fn index_key_exprs(&mut self, idx: &IndexSchema) {
        if let Some(w) = &idx.where_clause {
            self.expr(w);
        }
        for c in &idx.columns {
            if c.col_idx.is_none()
                && let Some(e) = &c.expr
            {
                self.expr(e);
            }
        }
    }

    /// `sqlite3FkCheck(pParse, pTab, regOld, 0, aChange, bChngRowid)` — the parent-side scan of
    /// the old row: fkey.c:1079, "if this is a deferred FK constraint, or a CASCADE or SET NULL
    /// action applies, then any foreign key violations caused by removing the parent key will be
    /// rectified by the action trigger. So do not set the may-abort flag in this case."
    fn fk_check_old(
        &mut self,
        db_id: DatabaseId,
        tbl: &TableSchema,
        update: Option<(&[bool], bool)>,
        _unused: bool,
    ) {
        let Some(slot) = self.db_set.slot(db_id) else {
            return;
        };
        let mut abort = false;
        for ctbl in &slot.schema.tables {
            for fk in &ctbl.foreign_keys {
                if !fk.ref_table.eq_ignore_ascii_case(&tbl.name) {
                    continue;
                }
                if let Some((modified, chng_rowid)) = update {
                    let changed = |ci: usize| -> bool {
                        modified.get(ci).copied().unwrap_or(false)
                            || (chng_rowid && tbl.rowid_alias_col == Some(ci))
                    };
                    if !fk_parent_is_modified(tbl, fk, &changed) {
                        continue;
                    }
                }
                let action = if update.is_some() { fk.on_update } else { fk.on_delete };
                if !(fk.deferred || self.defer_fks)
                    && action != FkAction::Cascade
                    && action != FkAction::SetNull
                {
                    abort = true;
                }
            }
        }
        if abort {
            self.may_abort();
        }
    }

    /// `sqlite3FkCheck(pParse, pTab, 0, regNew, aChange, bChngRowid)` — the child-side lookup of
    /// the new row (`fkLookupParent`, fkey.c:442-455): an immediate constraint either halts on
    /// the spot (single-row top-level INSERT: `sqlite3HaltConstraint(.., OE_Abort, ..)`) or
    /// counts the violation (`OP_FkCounter`) after `sqlite3MayAbort`.
    fn fk_check_new(&mut self, tbl: &TableSchema, update: Option<(&[bool], bool)>) {
        for fk in &tbl.foreign_keys {
            // `isSetNullAction`: "if this operation is being performed as part of a trigger
            // program that is actually a SET NULL action belonging to this very foreign key,
            // then omit this scan altogether."
            if self
                .set_null_fk
                .as_ref()
                .is_some_and(|(t, id)| *id == fk.id && t.eq_ignore_ascii_case(&tbl.name))
            {
                continue;
            }
            if let Some((modified, chng_rowid)) = update
                && !fk.ref_table.eq_ignore_ascii_case(&tbl.name)
                && !fk_child_is_modified(tbl, fk, modified, chng_rowid)
            {
                continue;
            }
            if !(fk.deferred || self.defer_fks) {
                self.may_abort();
            }
        }
    }

    /// `sqlite3FkActions`: for every constraint that references `tbl`, the action program
    /// (`fkActionTrigger`) is coded: RESTRICT is `SELECT RAISE(ABORT, ..)`, CASCADE / SET NULL /
    /// SET DEFAULT are a DELETE or UPDATE of the child table, coded like a trigger step.
    fn fk_actions(
        &mut self,
        db_id: DatabaseId,
        tbl: &TableSchema,
        update: Option<(&[bool], bool)>,
        _unused: bool,
    ) {
        let Some(slot) = self.db_set.slot(db_id) else {
            return;
        };
        let mut work: Vec<(TableSchema, crate::schema::ForeignKeySchema)> = Vec::new();
        for ctbl in &slot.schema.tables {
            for fk in &ctbl.foreign_keys {
                if !fk.ref_table.eq_ignore_ascii_case(&tbl.name) {
                    continue;
                }
                if let Some((modified, chng_rowid)) = update {
                    let changed = |ci: usize| -> bool {
                        modified.get(ci).copied().unwrap_or(false)
                            || (chng_rowid && tbl.rowid_alias_col == Some(ci))
                    };
                    if !fk_parent_is_modified(tbl, fk, &changed) {
                        continue;
                    }
                }
                work.push((ctbl.clone(), fk.clone()));
            }
        }
        for (ctbl, fk) in work {
            let is_update = update.is_some();
            let action = if is_update { fk.on_update } else { fk.on_delete };
            if action == FkAction::NoAction {
                continue; // fkActionTrigger returns 0 for OE_None... (no program)
            }
            let key = (ctbl.name.to_ascii_lowercase(), fk.id, is_update);
            if self.coded_fk_actions.contains(&key) {
                continue;
            }
            self.coded_fk_actions.push(key);
            self.depth += 1;
            match action {
                FkAction::Restrict => self.may_abort(), // RAISE(ABORT, 'FOREIGN KEY constraint failed')
                FkAction::Cascade if !is_update => self.child_delete(db_id, &ctbl),
                FkAction::Cascade | FkAction::SetNull | FkAction::SetDefault => {
                    if action == FkAction::SetDefault {
                        for &ci in &fk.from_cols {
                            if let Some(d) = ctbl.columns.get(ci).and_then(|c| c.default_expr.as_ref()) {
                                let d = d.clone();
                                self.expr(&d);
                            }
                        }
                    }
                    let mut modified = vec![false; ctbl.columns.len()];
                    for &ci in &fk.from_cols {
                        if let Some(m) = modified.get_mut(ci) {
                            *m = true;
                        }
                    }
                    self.set_null_fk = if action == FkAction::SetNull {
                        Some((ctbl.name.to_ascii_lowercase(), fk.id))
                    } else {
                        None
                    };
                    self.child_update(db_id, &ctbl, &modified);
                    self.set_null_fk = None;
                }
                FkAction::NoAction => {}
            }
            self.depth -= 1;
        }
    }

    /// The `DELETE FROM child WHERE <fk cols> = old.<key>` of an ON DELETE CASCADE action.
    fn child_delete(&mut self, db_id: DatabaseId, ctbl: &TableSchema) {
        let trigs = self.triggers_for(db_id, &ctbl.name, TriggerEventKind::Delete, None);
        // bComplex: the child table has a foreign key (this one), so sqlite3FkRequired is true.
        self.multi_write();
        self.row_delete(db_id, ctbl, &trigs, None);
    }

    /// The `UPDATE child SET <fk cols> = .. WHERE <fk cols> = old.<key>` of a foreign key action.
    fn child_update(&mut self, db_id: DatabaseId, ctbl: &TableSchema, modified: &[bool]) {
        let names: Vec<&str> = ctbl
            .columns
            .iter()
            .enumerate()
            .filter(|(i, _)| modified[*i])
            .map(|(_, c)| c.name.as_str())
            .collect();
        let trigs = self.triggers_for(db_id, &ctbl.name, TriggerEventKind::Update, Some(&names));
        // hasFK: the child key columns are modified.
        self.multi_write();
        self.update_row(db_id, ctbl, modified, false, &trigs, None, true);
    }

    // ------------------------------------------------------------------ UPDATE (update.c)
    fn update(&mut self, upd: &UpdateStmt, orconf: Option<ConflictAction>) {
        let top = self.depth == 0;
        for cte in &upd.ctes {
            self.select(&cte.query);
        }
        let Ok(Some(rel)) = self.db_set.find_relation_in_set(&upd.table) else {
            self.f = StmtFlags::ALL;
            return;
        };
        let assigned: Vec<&str> = upd
            .assignments
            .iter()
            .flat_map(|a| a.columns.iter().map(String::as_str))
            .collect();
        let (db_id, tbl) = match rel {
            (db_id, crate::db_set::ResolvedRelation::Table(t)) => (db_id, *t),
            (db_id, crate::db_set::ResolvedRelation::View(v)) => {
                let trigs =
                    self.triggers_for(db_id, &v.name, TriggerEventKind::Update, Some(&assigned));
                self.multi_write(); // pTrigger; the materialised view is never ONEPASS_SINGLE
                let sel = v.select.clone();
                self.select(&sel);
                self.exprs(upd.assignments.iter().map(|a| &a.expr));
                if let Some(w) = &upd.selection {
                    self.expr(w);
                }
                if let Some(fc) = &upd.from {
                    self.from_clause(fc);
                }
                for rc in &upd.returning {
                    if let ResultColumn::Expr { expr, .. } = rc {
                        self.expr(expr);
                    }
                }
                self.code_triggers(&trigs, orconf);
                return;
            }
        };
        if tbl.root_pgno == 0 {
            self.f = StmtFlags::ALL; // update.c:1331 sqlite3MultiWrite, :1362 sqlite3MayAbort
            return;
        }
        let mut modified = vec![false; tbl.columns.len()];
        let mut chng_rowid = false;
        let mut chng_pk = false;
        for name in &assigned {
            match tbl.find_column(name) {
                Some((ci, col)) => {
                    if tbl.rowid_alias_col == Some(ci) {
                        chng_rowid = true;
                    } else if tbl.without_rowid && col.is_primary_key {
                        chng_pk = true;
                    }
                    modified[ci] = true;
                }
                None => {
                    if !tbl.without_rowid && tbl.is_implicit_rowid_name(name) {
                        chng_rowid = true;
                    }
                }
            }
        }
        let modified = crate::engine::modified_cols_with_generated(&tbl, &modified);
        let chng_key = chng_rowid || chng_pk;
        let trigs = self.triggers_for(db_id, &tbl.name, TriggerEventKind::Update, Some(&assigned));
        let has_trigger = !trigs.is_empty() || (top && !upd.returning.is_empty());
        let has_fk = fk_required_update(self.db_set, db_id, &tbl, self.fk_on, &modified, chng_rowid);
        let indexes = self.indexes_of(db_id, &tbl);
        // bReplace: OR REPLACE, or (no OR clause and) a rewritten index with ON CONFLICT REPLACE.
        let mut b_replace = orconf == Some(ConflictAction::Replace);
        let mut rewritten: Vec<bool> = Vec::with_capacity(indexes.len());
        for idx in &indexes {
            let is_pk = tbl.without_rowid && idx.root_pgno == tbl.root_pgno;
            let rw = chng_key
                || has_fk > 1
                || is_pk
                || crate::engine::update_rewrites_index(&tbl, idx, &modified, chng_rowid, false);
            if rw
                && !(chng_key || has_fk > 1 || is_pk)
                && orconf.is_none()
                && idx.conflict == ConflictAction::Replace
                && idx.unique
            {
                b_replace = true;
            }
            rewritten.push(rw);
        }
        // sqlite3BeginWriteOperation(pParse, pTrigger || hasFK, iDb)
        if has_trigger || has_fk != 0 {
            self.multi_write();
        }
        self.exprs(upd.assignments.iter().map(|a| &a.expr));
        if let Some(w) = &upd.selection {
            self.expr(w);
        }
        if let Some(fc) = &upd.from {
            // update.c:716 — UPDATE ... FROM: sqlite3MultiWrite, eOnePass = ONEPASS_OFF.
            self.from_clause(fc);
            self.multi_write();
        } else {
            let scan = self.plan_scan(
                db_id,
                &tbl,
                upd.alias.as_deref(),
                upd.indexed_by.as_ref(),
                upd.selection.as_ref(),
            );
            if !scan.one_row {
                self.multi_write(); // update.c:769
            }
        }
        let _ = b_replace;
        if chng_rowid {
            self.may_abort(); // update.c:906 — OP_MustBeInt on the new rowid
        }
        self.update_row_with(db_id, &tbl, &modified, chng_key, &trigs, orconf, &indexes, &rewritten, has_fk);
        if top {
            for rc in &upd.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    self.expr(expr);
                }
            }
        }
    }

    fn from_clause(&mut self, fc: &FromClause) {
        let mut abort = false;
        walk_from(fc, &mut |x| {
            collect_node(x, &mut abort);
            Walk::Deep
        });
        if abort {
            self.may_abort();
        }
        for t in std::iter::once(&fc.head).chain(fc.joins.iter().map(|j| &j.right)) {
            match t {
                TableOrSubquery::Table { name, .. } => self.view_if_any(name),
                TableOrSubquery::Subquery { query, .. } => self.select(query),
                TableOrSubquery::TableFunction { .. } => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_row(
        &mut self,
        db_id: DatabaseId,
        tbl: &TableSchema,
        modified: &[bool],
        chng_rowid: bool,
        trigs: &[TriggerSchema],
        orconf: Option<ConflictAction>,
        _from_fk_action: bool,
    ) {
        let modified = crate::engine::modified_cols_with_generated(tbl, modified);
        let has_fk = fk_required_update(self.db_set, db_id, tbl, self.fk_on, &modified, chng_rowid);
        let indexes = self.indexes_of(db_id, tbl);
        let chng_pk = tbl.without_rowid
            && tbl
                .columns
                .iter()
                .enumerate()
                .any(|(i, c)| c.is_primary_key && modified[i]);
        let chng_key = chng_rowid || chng_pk;
        let rewritten: Vec<bool> = indexes
            .iter()
            .map(|idx| {
                chng_key
                    || has_fk > 1
                    || (tbl.without_rowid && idx.root_pgno == tbl.root_pgno)
                    || crate::engine::update_rewrites_index(tbl, idx, &modified, chng_rowid, false)
            })
            .collect();
        self.update_row_with(db_id, tbl, &modified, chng_key, trigs, orconf, &indexes, &rewritten, has_fk);
    }

    /// The per-row part of `sqlite3Update`: generated columns, BEFORE triggers, constraint
    /// checks, `sqlite3FkCheck` (old and new row), `sqlite3FkActions`, AFTER triggers.
    #[allow(clippy::too_many_arguments)]
    fn update_row_with(
        &mut self,
        db_id: DatabaseId,
        tbl: &TableSchema,
        modified: &[bool],
        chng_key: bool,
        trigs: &[TriggerSchema],
        orconf: Option<ConflictAction>,
        indexes: &[IndexSchema],
        rewritten: &[bool],
        has_fk: u8,
    ) {
        // sqlite3ComputeGeneratedColumns: every generated column is recomputed.
        for c in &tbl.columns {
            if let Some((g, _)) = &c.generated {
                let g = g.clone();
                self.expr(&g);
            }
        }
        self.code_triggers(trigs, orconf);
        self.constraint_checks(db_id, tbl, Some(modified), chng_key, orconf, indexes, Some(rewritten), UniqOverride::Stmt);
        if has_fk != 0 {
            let chng_rowid = chng_key && !tbl.without_rowid;
            self.fk_check_old(db_id, tbl, Some((modified, chng_rowid)), false);
            self.fk_check_new(tbl, Some((modified, chng_rowid)));
            self.fk_actions(db_id, tbl, Some((modified, chng_rowid)), false);
        }
    }

    // ------------------------------------------------------------------ INSERT (insert.c)
    fn insert(&mut self, ins: &InsertStmt, orconf: Option<ConflictAction>) {
        let top = self.depth == 0;
        for cte in &ins.ctes {
            self.select(&cte.query);
        }
        let Ok(Some(rel)) = self.db_set.find_relation_in_set(&ins.table) else {
            self.f = StmtFlags::ALL;
            return;
        };
        // A one-row VALUES clause is not a SELECT (insert.c: "pSelect = 0").
        let (is_select, source_exprs_single): (bool, Option<&Vec<Expr>>) = match &ins.source {
            InsertSource::Values(rows) if rows.len() == 1 => (false, rows.first()),
            InsertSource::Values(_) | InsertSource::Select(_) => (true, None),
            InsertSource::DefaultValues => (false, None),
        };
        let code_source = |a: &mut Self| match &ins.source {
            InsertSource::Values(rows) => {
                for r in rows {
                    a.exprs(r.iter());
                }
            }
            InsertSource::Select(s) => a.select(s),
            InsertSource::DefaultValues => {}
        };
        let (db_id, tbl) = match rel {
            (db_id, crate::db_set::ResolvedRelation::Table(t)) => (db_id, *t),
            (db_id, crate::db_set::ResolvedRelation::View(v)) => {
                let trigs = self.triggers_for(db_id, &v.name, TriggerEventKind::Insert, None);
                self.multi_write(); // pTrigger
                code_source(self);
                for rc in &ins.returning {
                    if let ResultColumn::Expr { expr, .. } = rc {
                        self.expr(expr);
                    }
                }
                self.code_triggers(&trigs, orconf);
                return;
            }
        };
        if tbl.root_pgno == 0 {
            self.f = StmtFlags::ALL; // insert.c:1559 — OP_VUpdate: sqlite3MayAbort
            return;
        }
        if tbl.autoincrement
            && !self
                .autoinc
                .iter()
                .any(|(d, n)| *d == db_id && n.eq_ignore_ascii_case(&tbl.name))
        {
            self.autoinc.push((db_id, tbl.name.clone())); // autoIncBegin
        }
        let trigs = self.triggers_for(db_id, &tbl.name, TriggerEventKind::Insert, None);
        let has_trigger = !trigs.is_empty() || (top && !ins.returning.is_empty());
        let has_before = trigs
            .iter()
            .any(|t| t.timing == TriggerTiming::Before);
        // sqlite3BeginWriteOperation(pParse, pSelect || pTrigger, iDb)
        if is_select || has_trigger {
            self.multi_write();
        }
        code_source(self);

        // ipkColumn: the position of the rowid in the source row.
        let mut ipk_given: Option<Option<&Expr>> = None; // Some(expr of a one-row VALUES, if any)
        if !tbl.without_rowid {
            match &ins.columns {
                Some(cols) => {
                    for (i, name) in cols.iter().enumerate() {
                        let is_ipk = match tbl.find_column(name) {
                            Some((ci, _)) => tbl.rowid_alias_col == Some(ci),
                            None => tbl.is_implicit_rowid_name(name),
                        };
                        if is_ipk {
                            ipk_given = Some(source_exprs_single.and_then(|r| r.get(i)));
                        }
                    }
                }
                None => {
                    if let Some(ipk) = tbl.rowid_alias_col
                        && !matches!(ins.source, InsertSource::DefaultValues)
                    {
                        // position among the non-generated columns
                        let pos = tbl.columns[..ipk].iter().filter(|c| c.generated.is_none()).count();
                        ipk_given = Some(source_exprs_single.and_then(|r| r.get(pos)));
                    }
                }
            }
        }
        let pk_chng = ipk_given.is_some();
        if let Some(e) = ipk_given {
            // insert.c:1461 (BEFORE triggers) and :1530 — OP_MustBeInt on the rowid, unless it is
            // a literal NULL of a one-row VALUES (then OP_NewRowid, "appendFlag").
            let literal_null = !is_select && matches!(e, Some(Expr::Null));
            if has_before || !literal_null {
                self.may_abort();
            }
        }
        // DEFAULT expressions of the columns that are not supplied.
        let supplied = |ci: usize| -> bool {
            match &ins.columns {
                Some(cols) => cols.iter().any(|n| tbl.find_column(n).is_some_and(|(c, _)| c == ci)),
                None => !matches!(ins.source, InsertSource::DefaultValues),
            }
        };
        for (ci, c) in tbl.columns.iter().enumerate() {
            if let Some((g, _)) = &c.generated {
                let g = g.clone();
                self.expr(&g);
            } else if !supplied(ci)
                && let Some(d) = &c.default_expr
            {
                let d = d.clone();
                self.expr(&d);
            }
        }
        self.code_triggers(&trigs, orconf);
        let indexes = self.indexes_of(db_id, &tbl);

        // UPSERT: ON CONFLICT DO NOTHING without a target is OR IGNORE; DO UPDATE is coded as a
        // nested sqlite3Update (ONEPASS_SINGLE).
        let mut override_conf = orconf;
        let mut upsert_update = false;
        for uc in &ins.upsert_clauses {
            if let Some(t) = &uc.target {
                for c in &t.columns {
                    if let Some(e) = &c.expr {
                        self.expr(e);
                    }
                }
                if let Some(w) = &t.where_clause {
                    self.expr(w);
                }
            }
            match &uc.action {
                UpsertAction::DoNothing => {
                    if uc.target.is_none() && ins.upsert_clauses.len() == 1 {
                        override_conf = Some(ConflictAction::Ignore);
                    }
                }
                UpsertAction::DoUpdate {
                    assignments,
                    where_clause,
                } => {
                    // `sqlite3UpsertDoUpdate` is coded where the conflict it answers is
                    // checked: for the rowid only when the statement supplies one (`pkChng`),
                    // for a UNIQUE index always.
                    let target_is_ipk = uc.target.as_ref().is_some_and(|t| {
                        !tbl.without_rowid
                            && t.columns.len() == 1
                            && match (&t.columns[0].expr, t.columns[0].name.as_str()) {
                                (Some(Expr::Column(c)), _) => match tbl.find_column(&c.column) {
                                    Some((ci, _)) => tbl.rowid_alias_col == Some(ci),
                                    None => tbl.is_implicit_rowid_name(&c.column),
                                },
                                (None, n) => match tbl.find_column(n) {
                                    Some((ci, _)) => tbl.rowid_alias_col == Some(ci),
                                    None => tbl.is_implicit_rowid_name(n),
                                },
                                _ => false,
                            }
                    });
                    let coded = match &uc.target {
                        Some(_) if target_is_ipk => pk_chng,
                        Some(_) => true,
                        None => pk_chng || indexes.iter().any(|i| i.unique),
                    };
                    if !coded {
                        continue;
                    }
                    upsert_update = true;
                    self.exprs(assignments.iter().map(|a| &a.expr));
                    if let Some(w) = where_clause {
                        self.expr(w);
                    }
                    let assigned: Vec<&str> = assignments
                        .iter()
                        .flat_map(|a| a.columns.iter().map(String::as_str))
                        .collect();
                    let mut modified = vec![false; tbl.columns.len()];
                    let mut chng_rowid = false;
                    for name in &assigned {
                        match tbl.find_column(name) {
                            Some((ci, _)) => {
                                if tbl.rowid_alias_col == Some(ci) {
                                    chng_rowid = true;
                                }
                                modified[ci] = true;
                            }
                            None => {
                                if !tbl.without_rowid && tbl.is_implicit_rowid_name(name) {
                                    chng_rowid = true;
                                }
                            }
                        }
                    }
                    let utrigs = self.triggers_for(
                        db_id,
                        &tbl.name,
                        TriggerEventKind::Update,
                        Some(&assigned),
                    );
                    let modified_g = crate::engine::modified_cols_with_generated(&tbl, &modified);
                    let has_fk = fk_required_update(
                        self.db_set,
                        db_id,
                        &tbl,
                        self.fk_on,
                        &modified_g,
                        chng_rowid,
                    );
                    if !utrigs.is_empty() || (top && !ins.returning.is_empty()) || has_fk != 0 {
                        self.multi_write();
                    }
                    if chng_rowid {
                        self.may_abort();
                    }
                    if tbl.without_rowid {
                        self.may_abort(); // upsert.c:311 (conflict on a secondary index)
                    }
                    self.update_row(db_id, &tbl, &modified, chng_rowid, &utrigs, None, false);
                }
            }
        }
        let _ = upsert_update;
        // insert.c: NOT NULL and CHECK are coded before the UPSERT clauses are looked at, with
        // the statement's own OR clause; only the uniqueness checks see DO NOTHING / DO UPDATE.
        let targetless = ins.upsert_clauses.len() == 1 && ins.upsert_clauses[0].target.is_none();
        let upsert_targets: Vec<Vec<String>> = if targetless {
            Vec::new()
        } else {
            ins.upsert_clauses
                .iter()
                .filter_map(|uc| uc.target.as_ref())
                .map(|t| {
                    t.columns
                        .iter()
                        .map(|c| match &c.expr {
                            Some(Expr::Column(cr)) => cr.column.to_ascii_lowercase(),
                            _ => c.name.to_ascii_lowercase(),
                        })
                        .collect()
                })
                .collect()
        };
        let uniq = if ins.upsert_clauses.is_empty() {
            UniqOverride::Stmt
        } else if targetless {
            UniqOverride::All
        } else {
            UniqOverride::Targets(&upsert_targets)
        };
        let _ = override_conf;
        self.constraint_checks(db_id, &tbl, None, pk_chng, orconf, &indexes, None, uniq);
        if self.fk_on {
            self.fk_check_insert(&tbl);
        }
        if top {
            for rc in &ins.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    self.expr(expr);
                }
            }
        }
    }

    /// `sqlite3FkCheck(pParse, pTab, 0, regIns, 0, 0)` for an INSERT. fkey.c:442: "if this is an
    /// INSERT statement that will insert exactly one row into the table, raise a constraint
    /// immediately instead of incrementing a counter" (`sqlite3HaltConstraint(.., OE_Abort)`),
    /// else `sqlite3MayAbort` before `OP_FkCounter`. Either way an immediate constraint sets
    /// mayAbort.
    fn fk_check_insert(&mut self, tbl: &TableSchema) {
        for fk in &tbl.foreign_keys {
            if !(fk.deferred || self.defer_fks) {
                self.may_abort();
            }
        }
    }

    /// `sqlite3GenerateConstraintChecks` (insert.c): NOT NULL, CHECK, rowid, UNIQUE / PRIMARY
    /// KEY — each `sqlite3HaltConstraint(.., OE_Abort, ..)` is a `sqlite3MayAbort`; REPLACE
    /// resolution that deletes rows is a `sqlite3MultiWrite`.
    #[allow(clippy::too_many_arguments)]
    fn constraint_checks(
        &mut self,
        db_id: DatabaseId,
        tbl: &TableSchema,
        update_modified: Option<&[bool]>,
        pk_chng: bool,
        override_conf: Option<ConflictAction>,
        indexes: &[IndexSchema],
        rewritten: Option<&[bool]>,
        uniq: UniqOverride<'_>,
    ) {
        let is_update = update_modified.is_some();
        // The conflict action of a uniqueness constraint over `cols` (lower-case names; the
        // rowid is `[]`), or `None` when an UPSERT clause answers its conflicts (no HALT).
        let uniq_action = |cols: &[String], own: ConflictAction| -> Option<ConflictAction> {
            match uniq {
                UniqOverride::Stmt => Some(override_conf.unwrap_or(own)),
                UniqOverride::All => None,
                UniqOverride::Targets(ts) => {
                    let hit = ts.iter().any(|t| {
                        if cols.is_empty() {
                            t.len() == 1
                                && match tbl.find_column(&t[0]) {
                                    Some((ci, _)) => tbl.rowid_alias_col == Some(ci),
                                    None => tbl.is_implicit_rowid_name(&t[0]),
                                }
                        } else {
                            t.len() == cols.len() && cols.iter().all(|c| t.contains(c))
                        }
                    });
                    if hit { None } else { Some(override_conf.unwrap_or(own)) }
                }
            }
        };
        // NOT NULL
        for (ci, c) in tbl.columns.iter().enumerate() {
            let Some(nn) = c.not_null else { continue };
            if tbl.rowid_alias_col == Some(ci) {
                continue;
            }
            if let Some(m) = update_modified
                && !m.get(ci).copied().unwrap_or(false)
                && c.generated.is_none()
            {
                continue;
            }
            let mut on_error = override_conf.unwrap_or(nn);
            if on_error == ConflictAction::Replace {
                if c.default_expr.is_none() || c.generated.is_some() {
                    on_error = ConflictAction::Abort;
                } else if let Some(d) = &c.default_expr {
                    let d = d.clone();
                    self.expr(&d);
                    // "REPLACE becomes ABORT on the 2nd pass" (which runs whenever a REPLACE
                    // was coded): OP_HaltIfNull with OE_Abort.
                    self.may_abort();
                }
            }
            if on_error == ConflictAction::Abort {
                self.may_abort();
            }
        }
        // CHECK
        for ck in &tbl.checks {
            if let Some(m) = update_modified {
                let updated = |cref: &ColumnRef| -> bool {
                    match tbl.find_column(&cref.column) {
                        Some((ci, _)) => {
                            m.get(ci).copied().unwrap_or(false)
                                || (pk_chng && tbl.rowid_alias_col == Some(ci))
                        }
                        None => pk_chng && tbl.is_implicit_rowid_name(&cref.column),
                    }
                };
                if !crate::engine::expr_mentions_column(&ck.expr, &updated) {
                    continue;
                }
            }
            let e = ck.expr.clone();
            self.expr(&e);
            let on_error = override_conf.unwrap_or(ConflictAction::Abort);
            if matches!(on_error, ConflictAction::Abort | ConflictAction::Replace) {
                self.may_abort();
            }
        }
        // "Determine if it is possible that triggers (either explicitly coded triggers or FK
        // resolution actions) might run as a result of deletes that happen when OR REPLACE
        // constraint handling runs": regTrigCnt.
        let del_trigs = if self.rec_triggers {
            self.triggers_for(db_id, &tbl.name, TriggerEventKind::Delete, None)
        } else {
            Vec::new()
        };
        let mut trig_cnt = (self.rec_triggers || self.fk_on)
            && (!del_trigs.is_empty() || fk_required_delete(self.db_set, db_id, tbl, self.fk_on));
        if matches!(uniq, UniqOverride::All) {
            // `overrideError` is OE_Ignore / OE_Update by now: not OE_Default, not OE_Replace.
            trig_cnt = false;
        }
        if trig_cnt && override_conf != Some(ConflictAction::Replace) {
            if override_conf.is_some() {
                trig_cnt = false;
            } else if !pk_chng || tbl.without_rowid || tbl.rowid_pk_conflict != ConflictAction::Replace {
                let any_replace = indexes.iter().any(|i| {
                    i.unique
                        && (if tbl.without_rowid && i.root_pgno == tbl.root_pgno {
                            tbl.without_rowid_pk_conflict
                        } else {
                            i.conflict
                        }) == ConflictAction::Replace
                });
                if !any_replace {
                    trig_cnt = false;
                }
            }
        }
        let mut n_replace_trig = 0u32;
        // rowid
        if pk_chng && !tbl.without_rowid {
            let on_error = uniq_action(&[], tbl.rowid_pk_conflict);
            match on_error {
                Some(ConflictAction::Abort) => self.may_abort(),
                Some(ConflictAction::Replace) => {
                    if trig_cnt {
                        self.multi_write(); // insert.c:2354
                        self.row_delete(db_id, tbl, &del_trigs, Some(ConflictAction::Replace));
                        n_replace_trig += 1;
                    } else if !indexes.is_empty() {
                        self.multi_write(); // insert.c:2370
                        for idx in indexes {
                            self.index_key_exprs(idx);
                        }
                    }
                }
                _ => {}
            }
        }
        // indexes
        for (i, idx) in indexes.iter().enumerate() {
            if let Some(rw) = rewritten
                && !rw.get(i).copied().unwrap_or(true)
            {
                continue;
            }
            self.index_key_exprs(idx);
            let is_pk = tbl.without_rowid && idx.root_pgno == tbl.root_pgno;
            if is_update && is_pk && !pk_chng {
                continue;
            }
            if !idx.unique {
                continue;
            }
            let own = if is_pk { tbl.without_rowid_pk_conflict } else { idx.conflict };
            let key_cols: Vec<String> = idx.columns.iter().map(|c| c.name.to_ascii_lowercase()).collect();
            let on_error = uniq_action(&key_cols, own);
            match on_error {
                Some(ConflictAction::Abort) => self.may_abort(),
                Some(ConflictAction::Replace) => {
                    // "Collision detection may be omitted if ..." (a lone WITHOUT ROWID PRIMARY
                    // KEY with REPLACE and neither triggers nor foreign keys).
                    let omit = is_pk
                        && indexes.len() == 1
                        && (!self.rec_triggers
                            || self
                                .triggers_for(db_id, &tbl.name, TriggerEventKind::Delete, None)
                                .is_empty())
                        && !fk_required_delete(self.db_set, db_id, tbl, self.fk_on);
                    if omit {
                        continue;
                    }
                    if trig_cnt {
                        self.multi_write(); // insert.c:2623
                        n_replace_trig += 1;
                    }
                    self.row_delete(db_id, tbl, &del_trigs, Some(ConflictAction::Replace));
                }
                _ => {}
            }
            if trig_cnt {
                self.may_abort(); // the recheck: sqlite3UniqueConstraint(pParse, OE_Abort, pIdx)
            }
        }
        if n_replace_trig > 0 && !tbl.without_rowid {
            self.may_abort(); // sqlite3RowidConstraint(pParse, OE_Abort, pTab)
        }
    }
}

/// How the uniqueness checks of `sqlite3GenerateConstraintChecks` resolve conflicts.
#[derive(Clone, Copy)]
enum UniqOverride<'a> {
    /// The statement's OR clause, else each constraint's own clause.
    Stmt,
    /// A single target-less `ON CONFLICT DO NOTHING` / `DO UPDATE`: every conflict is answered
    /// by the UPSERT (`overrideError = OE_Ignore` / `OE_Update`).
    All,
    /// `ON CONFLICT(cols)` clauses: the constraints they name are answered by the UPSERT.
    Targets(&'a [Vec<String>]),
}

/// Marks `abort` when the node itself is one whose code calls `sqlite3MayAbort`.
fn collect_node(x: &Expr, abort: &mut bool) {
    match x {
        Expr::FunctionCall {
            name, args, over, ..
        } => {
            let (nargs, star) = match args {
                FunctionArgs::Star => (0, true),
                FunctionArgs::Exprs(es) => (es.len(), false),
            };
            if over.is_some() {
                // window.c:1523 (frame bound checks) — not modelled exactly: assume it.
                *abort = true;
            } else if !is_inline_function(name) && !is_aggregate_call(name, nargs, star) {
                *abort = true;
            }
        }
        // LIKE / GLOB / REGEXP / MATCH are function calls; so are the JSON operators and the
        // CURRENT_* keywords.
        Expr::Like { .. } | Expr::CurrentTimeKw(_) => *abort = true,
        Expr::Binary {
            op: BinaryOp::JsonExtract | BinaryOp::JsonExtractText,
            ..
        } => *abort = true,
        Expr::Raise {
            action: RaiseAction::Abort,
            ..
        } => *abort = true,
        _ => {}
    }
}

fn collect_expr<'a>(e: &'a Expr, abort: &mut bool, subs: &mut Vec<&'a SelectStmt>) {
    walk_expr(e, &mut |x| {
        collect_node(x, abort);
        Walk::Shallow
    });
    fn subs_of<'a>(e: &'a Expr, out: &mut Vec<&'a SelectStmt>) {
        // a throw-away SELECT wrapper is not available: walk by hand
        match e {
            Expr::Subquery(s) | Expr::Exists { subquery: s, .. } => out.push(s),
            Expr::InSubquery { expr, subquery, .. } => {
                subs_of(expr, out);
                out.push(subquery);
            }
            Expr::Unary { expr, .. }
            | Expr::Collate { expr, .. }
            | Expr::Cast { expr, .. }
            | Expr::IsNull { expr, .. }
            | Expr::InTable { expr, .. } => subs_of(expr, out),
            Expr::Binary { left, right, .. } => {
                subs_of(left, out);
                subs_of(right, out);
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                subs_of(expr, out);
                subs_of(low, out);
                subs_of(high, out);
            }
            Expr::InList { expr, list, .. } => {
                subs_of(expr, out);
                for x in list {
                    subs_of(x, out);
                }
            }
            Expr::Like {
                expr,
                pattern,
                escape,
                ..
            } => {
                subs_of(expr, out);
                subs_of(pattern, out);
                if let Some(x) = escape {
                    subs_of(x, out);
                }
            }
            Expr::Case {
                operand,
                branches,
                else_expr,
            } => {
                if let Some(x) = operand {
                    subs_of(x, out);
                }
                for (w, t) in branches {
                    subs_of(w, out);
                    subs_of(t, out);
                }
                if let Some(x) = else_expr {
                    subs_of(x, out);
                }
            }
            Expr::FunctionCall { args, filter, .. } => {
                if let FunctionArgs::Exprs(es) = args {
                    for x in es {
                        subs_of(x, out);
                    }
                }
                if let Some(x) = filter {
                    subs_of(x, out);
                }
            }
            Expr::RowValue(es) => {
                for x in es {
                    subs_of(x, out);
                }
            }
            Expr::Raise { message, .. } => {
                if let Some(m) = message {
                    subs_of(m, out);
                }
            }
            _ => {}
        }
    }
    subs_of(e, subs);
}

/// [`dml_stmt_flags`] for the programs of the VDBE compiler (compiler.rs), which only accepts
/// tables without triggers, foreign keys, CHECK constraints, generated columns, defaults and
/// partial / expression indexes: the flags then depend on the statement and the table alone.
pub(crate) fn compiled_dml_flags(db_set: &DatabaseSet, stmt: CompiledDml<'_>) -> StmtFlags {
    let mut a = Analyzer {
        db_set,
        fk_on: false,
        rec_triggers: false,
        defer_fks: false,
        f: StmtFlags::default(),
        coded_triggers: Vec::new(),
        coded_fk_actions: Vec::new(),
        depth: 0,
        view_stack: Vec::new(),
        set_null_fk: None,
        autoinc: Vec::new(),
    };
    match stmt {
        CompiledDml::Insert(i) => a.insert(i, i.or_conflict),
        CompiledDml::Update(u) => a.update(u, u.or_conflict),
        CompiledDml::Delete(d) => a.delete(d),
    }
    a.f
}

/// [`dml_stmt_flags`] of a top-level INSERT.
pub(crate) fn dml_insert_flags(
    db_set: &DatabaseSet,
    ins: &InsertStmt,
    fk_on: bool,
    rec_triggers: bool,
) -> StmtFlags {
    let mut a = Analyzer {
        db_set,
        fk_on,
        rec_triggers,
        defer_fks: false,
        f: StmtFlags::default(),
        coded_triggers: Vec::new(),
        coded_fk_actions: Vec::new(),
        depth: 0,
        view_stack: Vec::new(),
        set_null_fk: None,
        autoinc: Vec::new(),
    };
    a.insert(ins, ins.or_conflict);
    a.f
}

/// The statement a VDBE write program is compiled from.
pub(crate) enum CompiledDml<'a> {
    Insert(&'a InsertStmt),
    Update(&'a UpdateStmt),
    Delete(&'a DeleteStmt),
}

/// `Parse.isMultiWrite` / `Parse.mayAbort` after C has generated the code of the top-level DML
/// statement `stmt` (INSERT / UPDATE / DELETE). Any other statement, and any shape this
/// analysis does not model (virtual tables, unresolvable names), reports both flags set. Also
/// returns `Parse.pAinc`: the AUTOINCREMENT tables an INSERT is coded for.
pub(crate) fn dml_stmt_flags(
    db_set: &DatabaseSet,
    stmt: &Stmt,
    fk_on: bool,
    rec_triggers: bool,
) -> (StmtFlags, Vec<(DatabaseId, String)>) {
    // A virtual-table target (or a name that does not resolve) reports both flags whatever else
    // the statement contains; answer that before the analysis copies the table definition.
    let target = match stmt {
        Stmt::Insert(i) => &i.table,
        Stmt::Update(u) => &u.table,
        Stmt::Delete(d) => &d.table,
        _ => return (StmtFlags::ALL, Vec::new()),
    };
    match db_set.find_relation_ref_in_set(target) {
        Ok(Some((_, crate::db_set::RelationRef::Table(t)))) if t.root_pgno != 0 => {}
        Ok(Some((_, crate::db_set::RelationRef::Table(_)))) | Ok(None) | Err(_) => {
            return (StmtFlags::ALL, Vec::new());
        }
        Ok(Some(_)) => {}
    }
    let mut a = Analyzer {
        db_set,
        fk_on,
        rec_triggers,
        defer_fks: false,
        f: StmtFlags::default(),
        coded_triggers: Vec::new(),
        coded_fk_actions: Vec::new(),
        depth: 0,
        view_stack: Vec::new(),
        set_null_fk: None,
        autoinc: Vec::new(),
    };
    match stmt {
        Stmt::Insert(i) => a.insert(i, i.or_conflict),
        Stmt::Update(u) => a.update(u, u.or_conflict),
        Stmt::Delete(d) => a.delete(d),
        _ => return (StmtFlags::ALL, Vec::new()),
    }
    (a.f, a.autoinc)
}
