//! SQL-to-VDBE bytecode compiler (`select.c`, `where.c`, `expr.c`, `insert.c`) and
//! `EXPLAIN` / `EXPLAIN QUERY PLAN` planner.
#![forbid(unsafe_code)]

use sqliters_syntax::{
    BinaryOp, BindParam, ColumnRef, CommonTableExpr, CompoundOp, CurrentTimeKind, Expr, FromClause,
    FunctionArgs, IndexHint, InsertSource, InsertStmt, JoinConstraint, JoinType, LikeOp,
    LimitClause, OrderByTerm, QualifiedName, ResultColumn, SelectBody, SelectCore, SelectStmt,
    SetAssignment, Stmt, TableOrSubquery, TriggerStep, TriggerTiming, UnaryOp, UpsertAction,
    WindowFrameBound, WindowRef, WindowSpec,
};
use sqliters_types::{
    Collation, DatabaseId, ErrorCode, KeySortOrder, Result, SqliteError, TypeAffinity, Value,
};
use sqliters_vdbe::{
    AggregateKind, Instruction, Opcode, P4, VdbeProgram, affinity_to_p5, sqlite_compare_affinity,
};

use crate::db_set::{DatabaseSet, ResolvedRelation};
use crate::execution_context::FunctionRegistry;
use crate::schema::{
    IndexSchema, IndexedColSchema, Schema, TableSchema, ViewSchema, fix_permanent_view_select,
    sqlite_log_est, sqlite_log_est_add,
};
use crate::trigger::TriggerEventKind;
use crate::window::{core_has_window, expr_contains_window, is_window_bound_constant};

#[derive(Debug, Clone)]
pub struct RangeBound {
    pub expr: Expr,
    pub inclusive: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QueryPlanContext<'a> {
    pub projection: Option<&'a [ResultColumn]>,
    pub group_by: &'a [Expr],
    pub having: Option<&'a Expr>,
    pub order_by: &'a [OrderByTerm],
    pub indexed_by: Option<&'a IndexHint>,
    pub is_count_star_only: bool,
}

fn explain_index_col_name(ic: &IndexedColSchema) -> &str {
    if ic.col_idx.is_some() {
        &ic.name
    } else if matches!(
        ic.expr.as_ref(),
        Some(Expr::Column(cref))
            if cref.table.is_none()
                && (cref.column.eq_ignore_ascii_case("rowid")
                    || cref.column.eq_ignore_ascii_case("_rowid_")
                    || cref.column.eq_ignore_ascii_case("oid"))
    ) {
        "rowid"
    } else {
        "<expr>"
    }
}

/// Chosen B-Tree access strategy for a single table (`where.c`).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum AccessPlan {
    FullScan,
    IndexFullScan {
        index: IndexSchema,
        is_covering: bool,
    },
    RowidSeek {
        key_expr: Expr,
    },
    RowidRange {
        lower: Option<RangeBound>,
        upper: Option<RangeBound>,
    },
    RowidIn {
        list: Vec<Expr>,
    },
    IndexSeek {
        index: IndexSchema,
        prefix_exprs: Vec<Expr>,
        is_covering: bool,
    },
    IndexRangeScan {
        index: IndexSchema,
        prefix_exprs: Vec<Expr>,
        prefix_in_lists: Vec<(usize, Vec<Expr>)>,
        range_col_idx: Option<usize>,
        lower: Option<RangeBound>,
        upper: Option<RangeBound>,
        is_covering: bool,
        skip_scan_cols: usize,
    },
    MultiIndexOr {
        branches: Vec<AccessPlan>,
    },
}

impl AccessPlan {
    pub fn explain_detail(&self, display_name: &str) -> String {
        match self {
            Self::FullScan => format!("SCAN {display_name}"),
            Self::IndexFullScan { index, is_covering } => {
                if index.table_without_rowid && index.origin == "pk" {
                    format!("SCAN {display_name}")
                } else {
                    let cov = if *is_covering { "COVERING " } else { "" };
                    format!("SCAN {display_name} USING {cov}INDEX {}", index.name)
                }
            }
            Self::RowidSeek { .. } | Self::RowidIn { .. } => {
                format!("SEARCH {display_name} USING INTEGER PRIMARY KEY (rowid=?)")
            }
            Self::RowidRange { lower, upper } => {
                let cond = match (lower, upper) {
                    (Some(_), Some(_)) => "rowid>? AND rowid<?",
                    (Some(_), None) => "rowid>?",
                    (None, Some(_)) => "rowid<?",
                    (None, None) => "rowid=?",
                };
                format!("SEARCH {display_name} USING INTEGER PRIMARY KEY ({cond})")
            }
            Self::IndexSeek {
                index,
                prefix_exprs,
                is_covering,
            } => {
                let cols: Vec<String> = index
                    .columns
                    .iter()
                    .take(prefix_exprs.len())
                    .map(|c| format!("{}=?", explain_index_col_name(c)))
                    .collect();
                if index.table_without_rowid && index.origin == "pk" {
                    format!(
                        "SEARCH {display_name} USING PRIMARY KEY ({})",
                        cols.join(" AND ")
                    )
                } else {
                    let cov = if *is_covering { "COVERING " } else { "" };
                    format!(
                        "SEARCH {display_name} USING {cov}INDEX {} ({})",
                        index.name,
                        cols.join(" AND ")
                    )
                }
            }
            Self::IndexRangeScan {
                index,
                prefix_exprs,
                prefix_in_lists,
                range_col_idx,
                lower,
                upper,
                is_covering,
                skip_scan_cols,
            } => {
                let n_eq = prefix_exprs.len() + prefix_in_lists.len() + *skip_scan_cols;
                let mut parts: Vec<String> = Vec::new();
                for i in 0..n_eq.min(index.columns.len()) {
                    let cname = explain_index_col_name(&index.columns[i]);
                    if i < *skip_scan_cols {
                        parts.push(format!("ANY({cname})"));
                    } else {
                        parts.push(format!("{cname}=?"));
                    }
                }
                if let Some(rc_idx) = *range_col_idx
                    && let Some(rc) = index.columns.get(rc_idx)
                {
                    let cname = explain_index_col_name(rc);
                    if lower.is_some() {
                        parts.push(format!("{cname}>?"));
                    }
                    if upper.is_some() {
                        parts.push(format!("{cname}<?"));
                    }
                }
                let cond_str = if parts.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", parts.join(" AND "))
                };
                if index.table_without_rowid && index.origin == "pk" {
                    format!("SEARCH {display_name} USING PRIMARY KEY{cond_str}")
                } else {
                    let cov = if *is_covering { "COVERING " } else { "" };
                    format!(
                        "SEARCH {display_name} USING {cov}INDEX {}{cond_str}",
                        index.name
                    )
                }
            }
            Self::MultiIndexOr { .. } => "MULTI-INDEX OR".to_owned(),
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
    choose_access_plan_with_schema_name(tbl, "main", alias, selection, schema)
}

/// Multi-database variant of [`choose_access_plan`] honoring `schema_name` in 3-part `schema.table.col` predicates.
pub fn choose_access_plan_with_schema_name(
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    selection: Option<&Expr>,
    schema: &Schema,
) -> AccessPlan {
    choose_access_plan_with_context(
        tbl,
        schema_name,
        alias,
        selection,
        schema,
        QueryPlanContext::default(),
    )
}

pub fn choose_access_plan_with_context(
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    selection: Option<&Expr>,
    schema: &Schema,
    ctx: QueryPlanContext<'_>,
) -> AccessPlan {
    if matches!(ctx.indexed_by, Some(IndexHint::NotIndexed)) {
        return AccessPlan::FullScan;
    }

    let forced_idx_name = match ctx.indexed_by {
        Some(IndexHint::IndexedBy(name)) => Some(name.as_str()),
        _ => None,
    };

    // Unfiltered `SELECT count(*) FROM tbl` (when no forced index and no WHERE clause)
    // can pick the narrowest non-partial index as a covering index scan (`where.c`).
    if selection.is_none()
        && forced_idx_name.is_none()
        && ctx.is_count_star_only
        && ctx.group_by.is_empty()
        && ctx.having.is_none()
    {
        let mut best_idx: Option<(i16, IndexSchema)> = None;
        for idx in schema.indexes_for_table(&tbl.name) {
            if idx.where_clause.is_some() || idx.is_unordered() {
                continue;
            }
            let sz = idx.effective_sz_idx_row(tbl);
            if sz < tbl.effective_sz_tab_row()
                && best_idx.as_ref().is_none_or(|(b_sz, _)| sz < *b_sz)
            {
                best_idx = Some((sz, idx));
            }
        }
        if let Some((_, index)) = best_idx {
            return AccessPlan::IndexFullScan {
                index,
                is_covering: true,
            };
        }
        return AccessPlan::FullScan;
    }

    let single_plan = choose_single_term_plan(tbl, schema_name, alias, selection, schema, ctx);
    if !matches!(
        single_plan,
        AccessPlan::FullScan | AccessPlan::IndexFullScan { .. }
    ) {
        return single_plan;
    }

    // Check for top-level `OR` (`MULTI-INDEX OR`) when not satisfied by a single index scan
    // and every OR branch can use an index (`whereexpr.c` / `where.c`).
    if let Some(where_expr) = selection {
        let mut or_terms = Vec::new();
        collect_or_disjuncts(where_expr, &mut or_terms);
        if or_terms.len() >= 2 {
            let mut branch_plans = Vec::with_capacity(or_terms.len());
            let mut all_indexed = true;
            for branch_expr in &or_terms {
                let sub_ctx = QueryPlanContext {
                    projection: None,
                    group_by: &[],
                    having: None,
                    order_by: &[],
                    indexed_by: ctx.indexed_by,
                    is_count_star_only: false,
                };
                let bp = choose_single_term_plan(
                    tbl,
                    schema_name,
                    alias,
                    Some(branch_expr),
                    schema,
                    sub_ctx,
                );
                if matches!(bp, AccessPlan::FullScan | AccessPlan::IndexFullScan { .. }) {
                    all_indexed = false;
                    break;
                }
                branch_plans.push(bp);
            }
            if all_indexed {
                return AccessPlan::MultiIndexOr {
                    branches: branch_plans,
                };
            }
        }
    }

    single_plan
}

fn collect_or_disjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Binary {
        left,
        op: BinaryOp::Or,
        right,
    } = expr
    {
        collect_or_disjuncts(left, out);
        collect_or_disjuncts(right, out);
    } else {
        out.push(expr);
    }
}

fn choose_single_term_plan(
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    selection: Option<&Expr>,
    schema: &Schema,
    ctx: QueryPlanContext<'_>,
) -> AccessPlan {
    let forced_idx_name = match ctx.indexed_by {
        Some(IndexHint::IndexedBy(name)) => Some(name.as_str()),
        _ => None,
    };

    let mut conjuncts = Vec::new();
    if let Some(where_expr) = selection {
        collect_and_conjuncts_with_between(where_expr, &mut conjuncts);
    }

    let tbl_row_est = tbl.stat1.n_row_log_est;
    let full_scan_cost: i16 = if forced_idx_name.is_some() {
        i16::MAX
    } else {
        tbl_row_est.saturating_add(16)
    };
    let mut best_cost: i16 = full_scan_cost;
    let mut best_sz: i16 = tbl.effective_sz_tab_row();
    let mut best_is_uniq = false;
    let mut best_eq_len: usize = 0;
    let mut best_plan = AccessPlan::FullScan;

    // 1. Check `rowid` / `INTEGER PRIMARY KEY` constraints (when table has rowid and no forced secondary index)
    if !tbl.without_rowid && forced_idx_name.is_none() {
        // 1a. `rowid = expr`
        let mut rowid_eq: Option<Expr> = None;
        let mut rowid_in: Option<Vec<Expr>> = None;
        let mut rowid_lower: Option<RangeBound> = None;
        let mut rowid_upper: Option<RangeBound> = None;

        for conj in &conjuncts {
            if let Some((col_name, rhs)) = extract_table_col_eq(conj, tbl, schema_name, alias)
                && is_rowid_or_ipk_name(tbl, &col_name)
            {
                let comp_aff = sqlite_compare_affinity(
                    Some(TypeAffinity::Integer),
                    compiler_expr_affinity(&rhs, Some((tbl, schema_name)), alias),
                );
                if index_affinity_ok(comp_aff, TypeAffinity::Integer) {
                    rowid_eq = Some(rhs);
                    break;
                }
            }
            if rowid_in.is_none()
                && let Some((col_name, list)) =
                    extract_table_col_in_list(conj, tbl, schema_name, alias)
                && is_rowid_or_ipk_name(tbl, &col_name)
            {
                rowid_in = Some(list);
            }
            if let Some((col_name, op, rhs)) =
                extract_table_col_range(conj, tbl, schema_name, alias)
                && is_rowid_or_ipk_name(tbl, &col_name)
            {
                let comp_aff = sqlite_compare_affinity(
                    Some(TypeAffinity::Integer),
                    compiler_expr_affinity(&rhs, Some((tbl, schema_name)), alias),
                );
                if index_affinity_ok(comp_aff, TypeAffinity::Integer) {
                    match op {
                        BinaryOp::Gt => {
                            if rowid_lower.is_none() {
                                rowid_lower = Some(RangeBound {
                                    expr: rhs,
                                    inclusive: false,
                                });
                            }
                        }
                        BinaryOp::Ge => {
                            if rowid_lower.is_none() {
                                rowid_lower = Some(RangeBound {
                                    expr: rhs,
                                    inclusive: true,
                                });
                            }
                        }
                        BinaryOp::Lt if rowid_upper.is_none() => {
                            rowid_upper = Some(RangeBound {
                                expr: rhs,
                                inclusive: false,
                            });
                        }
                        BinaryOp::Le if rowid_upper.is_none() => {
                            rowid_upper = Some(RangeBound {
                                expr: rhs,
                                inclusive: true,
                            });
                        }
                        _ => {}
                    }
                }
            }
            if let Some((col_name, low, high)) =
                extract_table_col_between(conj, tbl, schema_name, alias)
                && is_rowid_or_ipk_name(tbl, &col_name)
            {
                let low_aff = sqlite_compare_affinity(
                    Some(TypeAffinity::Integer),
                    compiler_expr_affinity(&low, Some((tbl, schema_name)), alias),
                );
                let high_aff = sqlite_compare_affinity(
                    Some(TypeAffinity::Integer),
                    compiler_expr_affinity(&high, Some((tbl, schema_name)), alias),
                );
                if index_affinity_ok(low_aff, TypeAffinity::Integer)
                    && index_affinity_ok(high_aff, TypeAffinity::Integer)
                {
                    if rowid_lower.is_none() {
                        rowid_lower = Some(RangeBound {
                            expr: low,
                            inclusive: true,
                        });
                    }
                    if rowid_upper.is_none() {
                        rowid_upper = Some(RangeBound {
                            expr: high,
                            inclusive: true,
                        });
                    }
                }
            }
        }

        if let Some(key_expr) = rowid_eq {
            // `where.c`: IPK equality seek cost is ~10 (1 row output).
            best_cost = 10;
            best_is_uniq = true;
            best_eq_len = 1;
            best_plan = AccessPlan::RowidSeek { key_expr };
        } else if let Some(list) = rowid_in {
            let n_in = sqlite_log_est((list.len().max(1)) as u64);
            let cost = n_in.saturating_add(10);
            if cost < best_cost {
                best_cost = cost;
                best_eq_len = 1;
                best_plan = AccessPlan::RowidIn { list };
            }
        } else if rowid_lower.is_some() || rowid_upper.is_some() {
            let mut r_size = tbl_row_est;
            if rowid_lower.is_some() {
                r_size = (r_size - 10).max(0);
            }
            if rowid_upper.is_some() {
                r_size = (r_size - 10).max(0);
            }
            let cost = sqlite_log_est_add(r_size + 16, 10);
            if cost < best_cost {
                best_cost = cost;
                best_plan = AccessPlan::RowidRange {
                    lower: rowid_lower,
                    upper: rowid_upper,
                };
            }
        }
    }

    // 2. Check secondary indexes (and primary Index B-Tree for `WITHOUT ROWID` tables)
    let mut indexes = schema.indexes_for_table(&tbl.name);
    if let Some(pk_idx) = tbl.without_rowid_pk_as_index() {
        indexes.insert(0, pk_idx);
    }

    for idx in indexes {
        if let Some(forced) = forced_idx_name
            && !idx.name.eq_ignore_ascii_case(forced)
        {
            continue;
        }
        // Partial index usability check (`whereUsablePartialIndex` in `whereexpr.c`):
        if let Some(ref part_where) = idx.where_clause {
            let implied = selection.is_some_and(|sel| {
                expr_implies_partial_where(sel, part_where, tbl, schema_name, alias)
            });
            if !implied {
                continue;
            }
        }

        let is_primary_without_rowid = idx.table_without_rowid && idx.origin == "pk";
        let is_covering = is_primary_without_rowid
            || index_is_covering_for_query(&idx, tbl, schema_name, alias, selection, &ctx);
        let idx_est = idx.effective_row_log_est(tbl);
        let idx_sz = idx.effective_sz_idx_row(tbl);
        let tab_sz = tbl.effective_sz_tab_row().max(1);

        // If `unordered` flag is set in `sqlite_stat1`, this index cannot be used for range scans,
        // full index scans, or partial prefix scans; only full-key equality seeks are allowed?
        // Actually in C SQLite (`where.c`: `IsUnordered(pProbe)`), an `unordered` stat1 tag means
        // the index is completely ignored for ALL index scans/seeks EXCEPT when forced or not at all!
        // Let's check: in `where.c:3668` (`whereLoopAddBtree`), if `pProbe->bUnordered`, it cannot be used
        // for range or ORDER BY, and in `analyze9.test` / `analyzeC.test`, `unordered` disables index scan!
        if idx.is_unordered() && forced_idx_name.is_none() {
            continue;
        }

        let mut prefix_exprs: Vec<Expr> = Vec::new();
        let mut prefix_in_lists: Vec<(usize, Vec<Expr>)> = Vec::new();
        let mut range_col_idx: Option<usize> = None;
        let mut lower: Option<RangeBound> = None;
        let mut upper: Option<RangeBound> = None;

        for (col_pos, ic) in idx.columns.iter().enumerate() {
            // Try equality match (`col = expr` or `expr = val` for expression index, or `col IS NULL`)
            let eq_match = conjuncts
                .iter()
                .find_map(|conj| match_index_col_eq(conj, ic, tbl, schema_name, alias));
            if let Some(rhs_expr) = eq_match {
                if prefix_in_lists.is_empty() {
                    prefix_exprs.push(rhs_expr);
                } else {
                    prefix_in_lists.push((col_pos, vec![rhs_expr]));
                }
                continue;
            }

            // Try `col IN (e1, e2, ...)` or `col IN <table_name>`
            let in_match = conjuncts
                .iter()
                .find_map(|conj| match_index_col_in_list(conj, ic, tbl, schema_name, alias))
                .or_else(|| {
                    conjuncts.iter().find_map(|conj| {
                        match_index_col_in_table(conj, ic, tbl, schema_name, alias, schema)
                    })
                });
            if let Some(list) = in_match {
                let in_est = estimate_in_expr_list_log(&list, schema);
                let col_gain = idx_est
                    .get(col_pos)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(idx_est.get(col_pos + 1).copied().unwrap_or(0));
                if col_pos == 0
                    || !idx.stat1.as_ref().is_some_and(|s| s.has_stat1)
                    || in_est <= col_gain
                {
                    prefix_in_lists.push((col_pos, list));
                    continue;
                }
            }

            // Try range bounds (`>`, `>=`, `<`, `<=`, `BETWEEN`) on this next column `col_pos`
            if !idx.is_unordered() {
                for conj in &conjuncts {
                    if let Some((op, rhs)) =
                        match_index_col_range(conj, ic, tbl, schema_name, alias)
                    {
                        match op {
                            BinaryOp::Gt => {
                                if lower.is_none() {
                                    lower = Some(RangeBound {
                                        expr: rhs,
                                        inclusive: false,
                                    });
                                    range_col_idx = Some(col_pos);
                                }
                            }
                            BinaryOp::Ge => {
                                if lower.is_none() {
                                    lower = Some(RangeBound {
                                        expr: rhs,
                                        inclusive: true,
                                    });
                                    range_col_idx = Some(col_pos);
                                }
                            }
                            BinaryOp::Lt if upper.is_none() => {
                                upper = Some(RangeBound {
                                    expr: rhs,
                                    inclusive: false,
                                });
                                range_col_idx = Some(col_pos);
                            }
                            BinaryOp::Le if upper.is_none() => {
                                upper = Some(RangeBound {
                                    expr: rhs,
                                    inclusive: true,
                                });
                                range_col_idx = Some(col_pos);
                            }
                            _ => {}
                        }
                    }
                    if let Some((low_e, high_e)) =
                        match_index_col_between(conj, ic, tbl, schema_name, alias)
                    {
                        if lower.is_none() {
                            lower = Some(RangeBound {
                                expr: low_e,
                                inclusive: true,
                            });
                            range_col_idx = Some(col_pos);
                        }
                        if upper.is_none() {
                            upper = Some(RangeBound {
                                expr: high_e,
                                inclusive: true,
                            });
                            range_col_idx = Some(col_pos);
                        }
                    }
                }
            }
            break;
        }

        // Skip-scan check (`whereLoopAddBtreeIndex` in `where.c`):
        // When `prefix_exprs` and `prefix_in_lists` and `range_col_idx` are empty on column 0,
        // check if skipping the first `k >= 1` columns (each having `aiRowLogEst[i] - aiRowLogEst[i+1] >= 42`,
        // i.e. < ~18 distinct values per prefix, and `!idx.is_no_skip_scan()`) allows matching equality/range
        // on column `k`!
        let mut skip_scan_cols: usize = 0;
        if prefix_exprs.is_empty()
            && prefix_in_lists.is_empty()
            && range_col_idx.is_none()
            && !idx.is_no_skip_scan()
            && !idx.is_unordered()
            && idx.stat1.as_ref().is_some_and(|s| s.has_stat1)
            && idx.columns.len() >= 2
        {
            let mut k = 0;
            while k + 1 < idx.columns.len() {
                // `where.c:3258`: skip-scan requires `pProbe->aiRowLogEst[nEq] >= 42`
                // where `aiRowLogEst[i]` is log2(est rows per distinct prefix of length i+1).
                let est_per_val = idx_est.get(k + 1).copied().unwrap_or(0);
                if est_per_val < 42 {
                    break;
                }
                k += 1;
                let ic = &idx.columns[k];
                if let Some(rhs_expr) = conjuncts
                    .iter()
                    .find_map(|conj| match_index_col_eq(conj, ic, tbl, schema_name, alias))
                {
                    skip_scan_cols = k;
                    prefix_exprs.push(rhs_expr);
                    // Also check subsequent columns after `k`!
                    for next_ic in idx.columns.iter().skip(k + 1) {
                        if let Some(next_rhs) = conjuncts.iter().find_map(|conj| {
                            match_index_col_eq(conj, next_ic, tbl, schema_name, alias)
                        }) {
                            prefix_exprs.push(next_rhs);
                        } else {
                            break;
                        }
                    }
                    break;
                }
                let mut skip_low = None;
                let mut skip_high = None;
                for conj in &conjuncts {
                    if let Some((op, rhs)) =
                        match_index_col_range(conj, ic, tbl, schema_name, alias)
                        && !ic.sort_order.desc
                    {
                        match op {
                            BinaryOp::Gt if skip_low.is_none() => {
                                skip_low = Some(RangeBound {
                                    expr: rhs,
                                    inclusive: false,
                                });
                            }
                            BinaryOp::Ge if skip_low.is_none() => {
                                skip_low = Some(RangeBound {
                                    expr: rhs,
                                    inclusive: true,
                                });
                            }
                            BinaryOp::Lt if skip_high.is_none() => {
                                skip_high = Some(RangeBound {
                                    expr: rhs,
                                    inclusive: false,
                                });
                            }
                            BinaryOp::Le if skip_high.is_none() => {
                                skip_high = Some(RangeBound {
                                    expr: rhs,
                                    inclusive: true,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                if skip_low.is_some() || skip_high.is_some() {
                    skip_scan_cols = k;
                    range_col_idx = Some(k);
                    lower = skip_low;
                    upper = skip_high;
                    break;
                }
            }
        }

        let n_eq = skip_scan_cols + prefix_exprs.len() + prefix_in_lists.len();
        let has_search = n_eq > 0 || range_col_idx.is_some();

        if has_search {
            // Compute search cost (`where.c`: `aiRowLogEst[nEq]` + IN multiplier + skip-scan multiplier + range discount + table lookup cost)
            let mut r_size = idx_est
                .get(n_eq.min(idx_est.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0);
            if range_col_idx.is_some() {
                if lower.is_some() {
                    r_size = (r_size - 10).max(0);
                }
                if upper.is_some() {
                    r_size = (r_size - 10).max(0);
                }
            }
            let mut n_seeks_log: i16 = 0;
            let mut has_in_table = false;
            for (_, list) in &prefix_in_lists {
                if list.len() == 1 && matches!(list[0], Expr::InTable { .. }) {
                    has_in_table = true;
                }
                n_seeks_log = n_seeks_log.saturating_add(estimate_in_expr_list_log(list, schema));
            }
            if skip_scan_cols > 0 {
                // Number of distinct prefixes of length `skip_scan_cols` is `idx_est[0] - idx_est[skip_scan_cols]`
                let n_distinct_skip =
                    (idx_est[0] - idx_est.get(skip_scan_cols).copied().unwrap_or(0)).max(10);
                n_seeks_log = n_seeks_log.saturating_add(n_distinct_skip);
            }
            let total_rows_out = r_size.saturating_add(n_seeks_log);
            // Seek overhead (~10 per seek) + scanning `total_rows_out` index entries
            let mut cost = sqlite_log_est_add(n_seeks_log + 10, total_rows_out);
            if !is_covering {
                // Table lookup per index hit (`where.c`: `rRun = sqlite3LogEstAdd(rRun, rSize + 16)`)
                cost = sqlite_log_est_add(cost, total_rows_out + 15);
            }
            if idx.where_clause.is_some() {
                // Slight bonus for usable partial index when costs are close
                cost = cost.saturating_sub(2);
            }

            let is_uniq = idx.unique
                && n_eq == idx.columns.len()
                && prefix_in_lists.is_empty()
                && skip_scan_cols == 0;
            let better = if cost != best_cost {
                cost < best_cost
            } else if is_covering && idx_sz != best_sz {
                idx_sz < best_sz
            } else if has_in_table {
                true
            } else {
                (is_uniq && !best_is_uniq) || (is_uniq == best_is_uniq && n_eq > best_eq_len)
            };

            if better {
                best_cost = cost;
                best_sz = idx_sz;
                best_is_uniq = is_uniq;
                best_eq_len = n_eq;
                if prefix_in_lists.is_empty() && range_col_idx.is_none() && skip_scan_cols == 0 {
                    best_plan = AccessPlan::IndexSeek {
                        index: idx,
                        prefix_exprs,
                        is_covering,
                    };
                } else {
                    best_plan = AccessPlan::IndexRangeScan {
                        index: idx,
                        prefix_exprs,
                        prefix_in_lists,
                        range_col_idx,
                        lower,
                        upper,
                        is_covering,
                        skip_scan_cols,
                    };
                }
            }
        } else {
            // Full index scan candidate (`where.c:4235-4254, 5823-5832`):
            // Eligible when:
            //   1. Forced by `INDEXED BY`, OR
            //   2. `is_covering && idx_sz < tab_sz`, OR
            //   3. Usable partial index (`idx.where_clause.is_some()`), OR
            //   4. Satisfies `ORDER BY`
            if is_primary_without_rowid {
                continue;
            }
            let satisfies_ob = !ctx.order_by.is_empty()
                && index_satisfies_order_by(&idx, tbl, schema_name, alias, selection, ctx.order_by);
            let eligible_full_scan = forced_idx_name.is_some()
                || (is_covering && idx_sz < tab_sz)
                || idx.where_clause.is_some()
                || satisfies_ob;
            if eligible_full_scan {
                let r_size = idx_est.first().copied().unwrap_or(tbl_row_est);
                let mut cost = r_size + 1 + (15 * idx_sz) / tab_sz;
                if !is_covering {
                    cost = sqlite_log_est_add(cost, r_size + 16);
                }
                if satisfies_ob {
                    // Saves an external sorter pass (~ 3 * r_size)
                    cost = cost.saturating_sub(16);
                }
                if idx.where_clause.is_some() && !idx.stat1.as_ref().is_some_and(|s| s.has_stat1) {
                    cost = cost.saturating_sub(4);
                }
                let better = if cost != best_cost {
                    cost < best_cost
                } else {
                    idx_sz < best_sz
                };
                if better {
                    best_cost = cost;
                    best_sz = idx_sz;
                    best_is_uniq = false;
                    best_eq_len = 0;
                    best_plan = AccessPlan::IndexFullScan {
                        index: idx,
                        is_covering,
                    };
                }
            }
        }
    }

    best_plan
}

fn is_rowid_or_ipk_name(tbl: &TableSchema, col_name: &str) -> bool {
    tbl.is_implicit_rowid_name(col_name)
        || tbl
            .rowid_alias_col
            .is_some_and(|ipk| tbl.columns[ipk].name.eq_ignore_ascii_case(col_name))
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

fn collect_and_conjuncts_with_between(expr: &Expr, out: &mut Vec<Expr>) {
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = expr
    {
        collect_and_conjuncts_with_between(left, out);
        collect_and_conjuncts_with_between(right, out);
    } else if let Expr::Between {
        expr: target,
        low,
        high,
        negated: false,
    } = expr
    {
        out.push(Expr::Binary {
            left: target.clone(),
            op: BinaryOp::Ge,
            right: low.clone(),
        });
        out.push(Expr::Binary {
            left: target.clone(),
            op: BinaryOp::Le,
            right: high.clone(),
        });
        out.push(expr.clone());
    } else if let Expr::Binary {
        left: d1,
        op: BinaryOp::Or,
        right: d2,
    } = expr
    {
        if !matches!(
            d1.as_ref(),
            Expr::Binary {
                op: BinaryOp::Or,
                ..
            }
        ) && !matches!(
            d2.as_ref(),
            Expr::Binary {
                op: BinaryOp::Or,
                ..
            }
        ) {
            let mut sub1 = Vec::new();
            let mut sub2 = Vec::new();
            collect_and_conjuncts(d1, &mut sub1);
            collect_and_conjuncts(d2, &mut sub2);
            for &e1 in &sub1 {
                for &e2 in &sub2 {
                    if let (
                        Expr::Binary {
                            left: l1,
                            op: op1,
                            right: r1,
                        },
                        Expr::Binary {
                            left: l2,
                            op: op2,
                            right: r2,
                        },
                    ) = (e1, e2)
                        && l1 == l2
                        && r1 == r2
                    {
                        if matches!(op1, BinaryOp::Eq | BinaryOp::Lt | BinaryOp::Le)
                            && matches!(op2, BinaryOp::Eq | BinaryOp::Lt | BinaryOp::Le)
                        {
                            let combined = if op1 == op2 { *op1 } else { BinaryOp::Le };
                            out.push(Expr::Binary {
                                left: l1.clone(),
                                op: combined,
                                right: r1.clone(),
                            });
                        } else if matches!(op1, BinaryOp::Eq | BinaryOp::Gt | BinaryOp::Ge)
                            && matches!(op2, BinaryOp::Eq | BinaryOp::Gt | BinaryOp::Ge)
                        {
                            let combined = if op1 == op2 { *op1 } else { BinaryOp::Ge };
                            out.push(Expr::Binary {
                                left: l1.clone(),
                                op: combined,
                                right: r1.clone(),
                            });
                        }
                    }
                }
            }
        }
        out.push(expr.clone());
    } else {
        out.push(expr.clone());
    }
}

fn extract_table_col_eq(
    expr: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
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

    if let Some(cname) = match_single_table_col(left, tbl, schema_name, alias)
        && !matches!(right.as_ref(), Expr::Collate { .. })
        && expr_is_table_independent(right)
    {
        return Some((cname, (**right).clone()));
    }
    if let Some(cname) = match_single_table_col(right, tbl, schema_name, alias)
        && !matches!(left.as_ref(), Expr::Collate { .. })
        && expr_is_table_independent(left)
    {
        return Some((cname, (**left).clone()));
    }
    None
}

fn extract_table_col_in_list(
    expr: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<(String, Vec<Expr>)> {
    let Expr::InList {
        expr: lhs,
        list,
        negated: false,
    } = expr
    else {
        return None;
    };
    let cname = match_single_table_col(lhs, tbl, schema_name, alias)?;
    if list.iter().all(expr_is_table_independent) {
        Some((cname, list.clone()))
    } else {
        None
    }
}

fn extract_table_col_range(
    expr: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<(String, BinaryOp, Expr)> {
    let Expr::Binary { left, op, right } = expr else {
        return None;
    };
    match op {
        BinaryOp::Gt | BinaryOp::Ge | BinaryOp::Lt | BinaryOp::Le => {}
        _ => return None,
    }
    if let Some(cname) = match_single_table_col(left, tbl, schema_name, alias)
        && !matches!(right.as_ref(), Expr::Collate { .. })
        && expr_is_table_independent(right)
    {
        return Some((cname, *op, (**right).clone()));
    }
    if let Some(cname) = match_single_table_col(right, tbl, schema_name, alias)
        && !matches!(left.as_ref(), Expr::Collate { .. })
        && expr_is_table_independent(left)
    {
        let flipped = match op {
            BinaryOp::Gt => BinaryOp::Lt,
            BinaryOp::Ge => BinaryOp::Le,
            BinaryOp::Lt => BinaryOp::Gt,
            BinaryOp::Le => BinaryOp::Ge,
            _ => return None,
        };
        return Some((cname, flipped, (**left).clone()));
    }
    None
}

fn extract_table_col_between(
    expr: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<(String, Expr, Expr)> {
    let Expr::Between {
        expr: target,
        low,
        high,
        negated: false,
    } = expr
    else {
        return None;
    };
    let cname = match_single_table_col(target, tbl, schema_name, alias)?;
    if expr_is_table_independent(low) && expr_is_table_independent(high) {
        Some((cname, (**low).clone(), (**high).clone()))
    } else {
        None
    }
}

fn match_index_col_target(
    candidate: &Expr,
    ic: &IndexedColSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<(TypeAffinity, Collation)> {
    let (candidate_inner, explicit_coll_name) = match candidate {
        Expr::Collate { expr, collation } => (expr.as_ref(), Some(collation.as_str())),
        other => (other, None),
    };
    if let Some(ci) = ic.col_idx {
        let cname = match_single_table_col(candidate_inner, tbl, schema_name, alias)?;
        if !cname.eq_ignore_ascii_case(&ic.name) {
            return None;
        }
        let col_coll_name = explicit_coll_name.unwrap_or_else(|| tbl.columns[ci].collation_name());
        if !ic
            .sort_order
            .collation_name()
            .eq_ignore_ascii_case(col_coll_name)
        {
            return None;
        }
        Some((tbl.columns[ci].affinity, ic.sort_order.collation))
    } else if let Some(ref idx_expr) = ic.expr {
        // Expression index column or rowid index column!
        if expr_ast_equiv_for_table(candidate_inner, idx_expr, tbl, schema_name, alias) {
            let expr_coll_name =
                explicit_coll_name.unwrap_or_else(|| infer_expr_collation_name(idx_expr, tbl));
            if !ic
                .sort_order
                .collation_name()
                .eq_ignore_ascii_case(expr_coll_name)
            {
                return None;
            }
            let aff = compiler_expr_affinity(idx_expr, Some((tbl, schema_name)), None)
                .unwrap_or(TypeAffinity::Blob);
            Some((aff, ic.sort_order.collation))
        } else {
            None
        }
    } else {
        None
    }
}

fn match_index_col_eq(
    conj: &Expr,
    ic: &IndexedColSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<Expr> {
    let Expr::Binary { left, op, right } = conj else {
        return None;
    };
    if *op != BinaryOp::Eq {
        return None;
    }
    if let Some((idx_aff, _)) = match_index_col_target(left, ic, tbl, schema_name, alias)
        && !matches!(right.as_ref(), Expr::Collate { .. })
        && expr_is_table_independent(right)
    {
        let rhs_aff = compiler_expr_affinity(right, Some((tbl, schema_name)), alias);
        let comp_aff = sqlite_compare_affinity(Some(idx_aff), rhs_aff);
        if index_affinity_ok(comp_aff, idx_aff) {
            return Some((**right).clone());
        }
    }
    if let Some((idx_aff, _)) = match_index_col_target(right, ic, tbl, schema_name, alias)
        && !matches!(left.as_ref(), Expr::Collate { .. })
        && expr_is_table_independent(left)
    {
        let rhs_aff = compiler_expr_affinity(left, Some((tbl, schema_name)), alias);
        let comp_aff = sqlite_compare_affinity(Some(idx_aff), rhs_aff);
        if index_affinity_ok(comp_aff, idx_aff) {
            return Some((**left).clone());
        }
    }
    None
}

fn match_index_col_in_list(
    conj: &Expr,
    ic: &IndexedColSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<Vec<Expr>> {
    let Expr::InList {
        expr: lhs,
        list,
        negated: false,
    } = conj
    else {
        return None;
    };
    let _ = match_index_col_target(lhs, ic, tbl, schema_name, alias)?;
    if list.iter().all(expr_is_table_independent) {
        Some(list.clone())
    } else {
        None
    }
}

fn match_index_col_in_table(
    conj: &Expr,
    ic: &IndexedColSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    schema: &Schema,
) -> Option<Vec<Expr>> {
    let Expr::InTable {
        expr: lhs,
        table,
        negated: false,
    } = conj
    else {
        return None;
    };
    let _ = match_index_col_target(lhs, ic, tbl, schema_name, alias)?;
    let rhs_tbl = schema.find_table(&table.name)?;
    if rhs_tbl.columns.len() == 1 {
        Some(vec![Expr::InTable {
            expr: Box::new(Expr::Null),
            table: table.clone(),
            negated: false,
        }])
    } else {
        None
    }
}

fn estimate_in_expr_list_log(list: &[Expr], schema: &Schema) -> i16 {
    if list.len() == 1
        && let Expr::InTable { table, .. } = &list[0]
    {
        return schema
            .find_table(&table.name)
            .map(|t| t.stat1.n_row_log_est)
            .unwrap_or(100);
    }
    sqlite_log_est((list.len().max(1)) as u64)
}

fn match_index_col_range(
    conj: &Expr,
    ic: &IndexedColSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<(BinaryOp, Expr)> {
    let Expr::Binary { left, op, right } = conj else {
        return None;
    };
    match op {
        BinaryOp::Gt | BinaryOp::Ge | BinaryOp::Lt | BinaryOp::Le => {}
        _ => return None,
    }
    if let Some((idx_aff, _)) = match_index_col_target(left, ic, tbl, schema_name, alias)
        && !matches!(right.as_ref(), Expr::Collate { .. })
        && expr_is_table_independent(right)
    {
        let rhs_aff = compiler_expr_affinity(right, Some((tbl, schema_name)), alias);
        let comp_aff = sqlite_compare_affinity(Some(idx_aff), rhs_aff);
        if index_affinity_ok(comp_aff, idx_aff) {
            return Some((*op, (**right).clone()));
        }
    }
    if let Some((idx_aff, _)) = match_index_col_target(right, ic, tbl, schema_name, alias)
        && !matches!(left.as_ref(), Expr::Collate { .. })
        && expr_is_table_independent(left)
    {
        let rhs_aff = compiler_expr_affinity(left, Some((tbl, schema_name)), alias);
        let comp_aff = sqlite_compare_affinity(Some(idx_aff), rhs_aff);
        if index_affinity_ok(comp_aff, idx_aff) {
            let flipped = match op {
                BinaryOp::Gt => BinaryOp::Lt,
                BinaryOp::Ge => BinaryOp::Le,
                BinaryOp::Lt => BinaryOp::Gt,
                BinaryOp::Le => BinaryOp::Ge,
                _ => return None,
            };
            return Some((flipped, (**left).clone()));
        }
    }
    None
}

fn match_index_col_between(
    conj: &Expr,
    ic: &IndexedColSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<(Expr, Expr)> {
    let Expr::Between {
        expr: target,
        low,
        high,
        negated: false,
    } = conj
    else {
        return None;
    };
    let (idx_aff, _) = match_index_col_target(target, ic, tbl, schema_name, alias)?;
    if !expr_is_table_independent(low) || !expr_is_table_independent(high) {
        return None;
    }
    let low_aff = sqlite_compare_affinity(
        Some(idx_aff),
        compiler_expr_affinity(low, Some((tbl, schema_name)), alias),
    );
    let high_aff = sqlite_compare_affinity(
        Some(idx_aff),
        compiler_expr_affinity(high, Some((tbl, schema_name)), alias),
    );
    if index_affinity_ok(low_aff, idx_aff) && index_affinity_ok(high_aff, idx_aff) {
        Some(((**low).clone(), (**high).clone()))
    } else {
        None
    }
}

/// Checks whether a query `WHERE` expression provably implies a partial index `WHERE` predicate
/// (`whereUsablePartialIndex` / `sqlite3ExprImpliesExpr` / `sqlite3ExprImpliesNonNullRow` in `whereexpr.c` & `expr.c`).
pub fn expr_implies_partial_where(
    query_where: &Expr,
    partial_where: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> bool {
    let mut part_conjuncts = Vec::new();
    collect_and_conjuncts(partial_where, &mut part_conjuncts);
    let mut query_conjuncts = Vec::new();
    collect_and_conjuncts(query_where, &mut query_conjuncts);

    part_conjuncts.into_iter().all(|p_conj| {
        // If `p_conj` is `col IS NOT NULL`, check if the entire `query_where` is null-rejecting for `col`
        if let Expr::IsNull {
            expr: inner,
            negated: true,
        } = p_conj
            && let Some(cname) = match_single_table_col(inner, tbl, schema_name, alias)
            && expr_implies_non_null_col(query_where, &cname, tbl, schema_name, alias)
        {
            return true;
        }
        query_conjuncts
            .iter()
            .any(|q_conj| single_conjunct_implies(q_conj, p_conj, tbl, schema_name, alias))
    })
}

fn single_conjunct_implies(
    q: &Expr,
    p: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> bool {
    if expr_ast_equiv_for_table(q, p, tbl, schema_name, alias) {
        return true;
    }
    // `OR` in `q`: `(A OR B)` implies `P` if both `A` implies `P` and `B` implies `P`
    if let Expr::Binary {
        left,
        op: BinaryOp::Or,
        right,
    } = q
    {
        return single_conjunct_implies(left, p, tbl, schema_name, alias)
            && single_conjunct_implies(right, p, tbl, schema_name, alias);
    }
    // `OR` in `p`: `Q` implies `(P1 OR P2)` if `Q` is syntactically equivalent to `P1` or `P2`
    // (`sqlite3ExprImpliesExpr` in `sqlite/src/expr.c`).
    if let Expr::Binary {
        left,
        op: BinaryOp::Or,
        right,
    } = p
    {
        return expr_ast_equiv_for_table(q, left, tbl, schema_name, alias)
            || expr_ast_equiv_for_table(q, right, tbl, schema_name, alias);
    }

    // If `p` is `X IS NOT NULL`, any null-rejecting comparison on `X` implies `p`
    if let Expr::IsNull {
        expr: p_inner,
        negated: true,
    } = p
        && expr_rejects_null_of_target(q, p_inner, tbl, schema_name, alias)
    {
        return true;
    }

    // `sqlite3ExprImpliesExpr`: compare `(X op_q C_q)` against `(X op_p C_p)` for constant literals
    if let Some((q_lhs, q_op, q_val)) = extract_expr_op_const(q)
        && let Some((p_lhs, p_op, p_val)) = extract_expr_op_const(p)
        && expr_ast_equiv_for_table(q_lhs, p_lhs, tbl, schema_name, alias)
        && let Some(ord) = compare_const_literals(&q_val, &p_val)
    {
        use std::cmp::Ordering;
        return match (q_op, p_op) {
            (BinaryOp::Eq, BinaryOp::Eq) => ord == Ordering::Equal,
            (BinaryOp::Eq, BinaryOp::Ne) => ord != Ordering::Equal,
            (BinaryOp::Eq, BinaryOp::Gt) => ord == Ordering::Greater,
            (BinaryOp::Eq, BinaryOp::Ge) => ord == Ordering::Greater || ord == Ordering::Equal,
            (BinaryOp::Eq, BinaryOp::Lt) => ord == Ordering::Less,
            (BinaryOp::Eq, BinaryOp::Le) => ord == Ordering::Less || ord == Ordering::Equal,
            (BinaryOp::Gt, BinaryOp::Gt | BinaryOp::Ge | BinaryOp::Ne) => {
                ord == Ordering::Greater || ord == Ordering::Equal
            }
            (BinaryOp::Ge, BinaryOp::Ge) => ord == Ordering::Greater || ord == Ordering::Equal,
            (BinaryOp::Ge, BinaryOp::Gt | BinaryOp::Ne) => ord == Ordering::Greater,
            (BinaryOp::Lt, BinaryOp::Lt | BinaryOp::Le | BinaryOp::Ne) => {
                ord == Ordering::Less || ord == Ordering::Equal
            }
            (BinaryOp::Le, BinaryOp::Le) => ord == Ordering::Less || ord == Ordering::Equal,
            (BinaryOp::Le, BinaryOp::Lt | BinaryOp::Ne) => ord == Ordering::Less,
            _ => false,
        };
    }

    // `X BETWEEN low AND high` implies `X >= low` and `X <= high`
    if let Expr::Between {
        expr: b_target,
        low,
        high,
        negated: false,
    } = q
    {
        let ge_e = Expr::Binary {
            left: b_target.clone(),
            op: BinaryOp::Ge,
            right: low.clone(),
        };
        let le_e = Expr::Binary {
            left: b_target.clone(),
            op: BinaryOp::Le,
            right: high.clone(),
        };
        if single_conjunct_implies(&ge_e, p, tbl, schema_name, alias)
            || single_conjunct_implies(&le_e, p, tbl, schema_name, alias)
        {
            return true;
        }
    }

    // `X IN (c1, c2, ...)` implies `p` if every `X = ci` implies `p`
    if let Expr::InList {
        expr: in_lhs,
        list,
        negated: false,
    } = q
        && !list.is_empty()
    {
        return list.iter().all(|item| {
            let eq = Expr::Binary {
                left: in_lhs.clone(),
                op: BinaryOp::Eq,
                right: Box::new(item.clone()),
            };
            single_conjunct_implies(&eq, p, tbl, schema_name, alias)
        });
    }

    false
}

fn extract_expr_op_const(expr: &Expr) -> Option<(&Expr, BinaryOp, Value)> {
    let Expr::Binary { left, op, right } = expr else {
        return None;
    };
    match op {
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Gt | BinaryOp::Ge | BinaryOp::Lt | BinaryOp::Le => {
        }
        _ => return None,
    }
    if let Some(v) = eval_const_literal_for_implication(right) {
        return Some((left.as_ref(), *op, v));
    }
    if let Some(v) = eval_const_literal_for_implication(left) {
        let flipped = match op {
            BinaryOp::Eq => BinaryOp::Eq,
            BinaryOp::Ne => BinaryOp::Ne,
            BinaryOp::Gt => BinaryOp::Lt,
            BinaryOp::Ge => BinaryOp::Le,
            BinaryOp::Lt => BinaryOp::Gt,
            BinaryOp::Le => BinaryOp::Ge,
            _ => return None,
        };
        return Some((right.as_ref(), flipped, v));
    }
    None
}

fn eval_const_literal_for_implication(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Integer(i) => Some(Value::Integer(*i)),
        Expr::Float(f) => Some(Value::Real(*f)),
        Expr::String(s) => Some(Value::Text(s.clone())),
        Expr::Bool(b) => Some(Value::Integer(i64::from(*b))),
        Expr::Unary {
            op: UnaryOp::Negate,
            expr: inner,
        } => match inner.as_ref() {
            Expr::Integer(i) => i.checked_neg().map(Value::Integer),
            Expr::Float(f) => Some(Value::Real(-*f)),
            _ => None,
        },
        Expr::Unary {
            op: UnaryOp::Plus,
            expr: inner,
        } => eval_const_literal_for_implication(inner),
        _ => None,
    }
}

fn compare_const_literals(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => Some(x.cmp(y)),
        (Value::Real(x), Value::Real(y)) => x.partial_cmp(y),
        (Value::Integer(x), Value::Real(y)) => (*x as f64).partial_cmp(y),
        (Value::Real(x), Value::Integer(y)) => x.partial_cmp(&(*y as f64)),
        (Value::Text(x), Value::Text(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn expr_rejects_null_of_target(
    q: &Expr,
    target: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> bool {
    match q {
        Expr::IsNull {
            expr: inner,
            negated: true,
        } => expr_ast_equiv_for_table(inner, target, tbl, schema_name, alias),
        Expr::Binary { left, op, right } => match op {
            BinaryOp::And => {
                expr_rejects_null_of_target(left, target, tbl, schema_name, alias)
                    || expr_rejects_null_of_target(right, target, tbl, schema_name, alias)
            }
            BinaryOp::Or => {
                expr_rejects_null_of_target(left, target, tbl, schema_name, alias)
                    && expr_rejects_null_of_target(right, target, tbl, schema_name, alias)
            }
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => {
                (expr_ast_equiv_for_table(left, target, tbl, schema_name, alias)
                    && !matches!(right.as_ref(), Expr::Null))
                    || (expr_ast_equiv_for_table(right, target, tbl, schema_name, alias)
                        && !matches!(left.as_ref(), Expr::Null))
            }
            _ => false,
        },
        Expr::Between {
            expr: b_e,
            negated: false,
            ..
        } => expr_ast_equiv_for_table(b_e, target, tbl, schema_name, alias),
        Expr::InList {
            expr: in_e,
            list,
            negated: false,
        } => !list.is_empty() && expr_ast_equiv_for_table(in_e, target, tbl, schema_name, alias),
        Expr::Like {
            expr: l_e,
            negated: false,
            ..
        } => expr_ast_equiv_for_table(l_e, target, tbl, schema_name, alias),
        _ => false,
    }
}

fn expr_implies_non_null_col(
    expr: &Expr,
    col_name: &str,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> bool {
    let target = Expr::Column(ColumnRef::unqualified(col_name.to_owned()));
    expr_rejects_null_of_target(expr, &target, tbl, schema_name, alias)
}

pub fn expr_ast_equiv_for_table(
    a: &Expr,
    b: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> bool {
    match (a, b) {
        (Expr::Null, Expr::Null) => true,
        (Expr::Bool(x), Expr::Bool(y)) => x == y,
        (Expr::Integer(x), Expr::Integer(y)) => x == y,
        (Expr::Float(x), Expr::Float(y)) => x.to_bits() == y.to_bits(),
        (Expr::String(x), Expr::String(y)) => x == y,
        (Expr::Blob(x), Expr::Blob(y)) => x == y,
        (Expr::Column(ca), Expr::Column(cb)) => {
            if !cref_matches_single_table(ca, tbl, schema_name, alias)
                || !cref_matches_single_table(cb, tbl, schema_name, alias)
            {
                return false;
            }
            if is_rowid_or_ipk_name(tbl, &ca.column) && is_rowid_or_ipk_name(tbl, &cb.column) {
                return true;
            }
            ca.column.eq_ignore_ascii_case(&cb.column)
        }
        (Expr::Unary { op: op_a, expr: ea }, Expr::Unary { op: op_b, expr: eb }) => {
            op_a == op_b && expr_ast_equiv_for_table(ea, eb, tbl, schema_name, alias)
        }
        (
            Expr::Binary {
                left: la,
                op: op_a,
                right: ra,
            },
            Expr::Binary {
                left: lb,
                op: op_b,
                right: rb,
            },
        ) => {
            if op_a == op_b
                && expr_ast_equiv_for_table(la, lb, tbl, schema_name, alias)
                && expr_ast_equiv_for_table(ra, rb, tbl, schema_name, alias)
            {
                return true;
            }
            // Commutative operators (`=`, `!=`, `+`, `*`, `AND`, `OR`)
            if matches!(
                op_a,
                BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Add
                    | BinaryOp::Multiply
                    | BinaryOp::And
                    | BinaryOp::Or
            ) && op_a == op_b
                && expr_ast_equiv_for_table(la, rb, tbl, schema_name, alias)
                && expr_ast_equiv_for_table(ra, lb, tbl, schema_name, alias)
            {
                return true;
            }
            false
        }
        (
            Expr::Collate {
                expr: ea,
                collation: ca,
            },
            Expr::Collate {
                expr: eb,
                collation: cb,
            },
        ) => {
            ca.eq_ignore_ascii_case(cb) && expr_ast_equiv_for_table(ea, eb, tbl, schema_name, alias)
        }
        (
            Expr::IsNull {
                expr: ea,
                negated: na,
            },
            Expr::IsNull {
                expr: eb,
                negated: nb,
            },
        ) => na == nb && expr_ast_equiv_for_table(ea, eb, tbl, schema_name, alias),
        (
            Expr::Cast {
                expr: ea,
                type_name: ta,
            },
            Expr::Cast {
                expr: eb,
                type_name: tb,
            },
        ) => {
            ta.eq_ignore_ascii_case(tb) && expr_ast_equiv_for_table(ea, eb, tbl, schema_name, alias)
        }
        (
            Expr::FunctionCall {
                name: na,
                distinct: da,
                args: aa,
                filter: fa,
                over: None,
            },
            Expr::FunctionCall {
                name: nb,
                distinct: db,
                args: ab,
                filter: fb,
                over: None,
            },
        ) => {
            if !na.eq_ignore_ascii_case(nb) || da != db {
                return false;
            }
            let args_eq = match (aa, ab) {
                (FunctionArgs::Star, FunctionArgs::Star) => true,
                (FunctionArgs::Exprs(esa), FunctionArgs::Exprs(esb)) => {
                    esa.len() == esb.len()
                        && esa
                            .iter()
                            .zip(esb.iter())
                            .all(|(x, y)| expr_ast_equiv_for_table(x, y, tbl, schema_name, alias))
                }
                _ => false,
            };
            let filter_eq = match (fa, fb) {
                (None, None) => true,
                (Some(fx), Some(fy)) => expr_ast_equiv_for_table(fx, fy, tbl, schema_name, alias),
                _ => false,
            };
            args_eq && filter_eq
        }
        (
            Expr::Between {
                expr: ea,
                low: la,
                high: ha,
                negated: na,
            },
            Expr::Between {
                expr: eb,
                low: lb,
                high: hb,
                negated: nb,
            },
        ) => {
            na == nb
                && expr_ast_equiv_for_table(ea, eb, tbl, schema_name, alias)
                && expr_ast_equiv_for_table(la, lb, tbl, schema_name, alias)
                && expr_ast_equiv_for_table(ha, hb, tbl, schema_name, alias)
        }
        (
            Expr::InList {
                expr: ea,
                list: la,
                negated: na,
            },
            Expr::InList {
                expr: eb,
                list: lb,
                negated: nb,
            },
        ) => {
            na == nb
                && la.len() == lb.len()
                && expr_ast_equiv_for_table(ea, eb, tbl, schema_name, alias)
                && la
                    .iter()
                    .zip(lb.iter())
                    .all(|(x, y)| expr_ast_equiv_for_table(x, y, tbl, schema_name, alias))
        }
        (
            Expr::Like {
                expr: ea,
                pattern: pa,
                escape: esca,
                op: opa,
                negated: na,
            },
            Expr::Like {
                expr: eb,
                pattern: pb,
                escape: escb,
                op: opb,
                negated: nb,
            },
        ) => {
            opa == opb
                && na == nb
                && expr_ast_equiv_for_table(ea, eb, tbl, schema_name, alias)
                && expr_ast_equiv_for_table(pa, pb, tbl, schema_name, alias)
                && match (esca, escb) {
                    (None, None) => true,
                    (Some(x), Some(y)) => expr_ast_equiv_for_table(x, y, tbl, schema_name, alias),
                    _ => false,
                }
        }
        _ => false,
    }
}

/// Checks if `idx` covers all columns/expressions of `tbl` referenced anywhere in the query
/// (`SELECT` projection, `WHERE` selection, `GROUP BY`, `HAVING`, `ORDER BY`).
fn index_is_covering_for_query(
    idx: &IndexSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    selection: Option<&Expr>,
    ctx: &QueryPlanContext<'_>,
) -> bool {
    let Some(proj) = ctx.projection else {
        // DML (`UPDATE` / `DELETE`) or unknown projection requires full table access
        return false;
    };

    for rc in proj {
        match rc {
            ResultColumn::Star => {
                for (ci, _) in tbl.columns.iter().enumerate() {
                    if !index_provides_table_col(idx, tbl, ci) {
                        return false;
                    }
                }
            }
            ResultColumn::TableStar { table, .. } => {
                let effective_table = alias.unwrap_or(&tbl.name);
                if table.eq_ignore_ascii_case(effective_table)
                    || table.eq_ignore_ascii_case(&tbl.name)
                {
                    for (ci, _) in tbl.columns.iter().enumerate() {
                        if !index_provides_table_col(idx, tbl, ci) {
                            return false;
                        }
                    }
                }
            }
            ResultColumn::Expr { expr, .. } => {
                if !expr_covered_by_index(expr, idx, tbl, schema_name, alias) {
                    return false;
                }
            }
        }
    }
    if let Some(w) = selection
        && !expr_covered_by_index(w, idx, tbl, schema_name, alias)
    {
        return false;
    }
    for g in ctx.group_by {
        if !expr_covered_by_index(g, idx, tbl, schema_name, alias) {
            return false;
        }
    }
    if let Some(h) = ctx.having
        && !expr_covered_by_index(h, idx, tbl, schema_name, alias)
    {
        return false;
    }
    for ob in ctx.order_by {
        if !expr_covered_by_index(&ob.expr, idx, tbl, schema_name, alias) {
            return false;
        }
    }
    true
}

pub fn index_provides_table_col(idx: &IndexSchema, tbl: &TableSchema, col_idx: usize) -> bool {
    if !tbl.without_rowid && tbl.rowid_alias_col == Some(col_idx) {
        return true;
    }
    if idx.columns.iter().any(|ic| ic.col_idx == Some(col_idx)) {
        return true;
    }
    if idx
        .trailing_pk_cols
        .iter()
        .any(|ic| ic.col_idx == Some(col_idx))
    {
        return true;
    }
    if let Some((ref gen_expr, _)) = tbl.columns[col_idx].generated {
        // A generated column is covered if either its expression is directly indexed or all columns
        // referenced by its generator expression are covered by the index!
        return expr_covered_by_index(gen_expr, idx, tbl, "main", None);
    }
    false
}

pub fn index_slot_for_table_col(
    idx: &IndexSchema,
    tbl: &TableSchema,
    col_idx: usize,
) -> Option<usize> {
    if let Some(pos) = idx
        .columns
        .iter()
        .position(|ic| ic.col_idx == Some(col_idx))
    {
        return Some(pos);
    }
    if tbl.without_rowid
        && let Some(pos) = idx
            .trailing_pk_cols
            .iter()
            .position(|ic| ic.col_idx == Some(col_idx))
    {
        return Some(idx.columns.len() + pos);
    }
    None
}

pub fn index_slot_for_expr(
    idx: &IndexSchema,
    expr: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<usize> {
    idx.columns.iter().position(|ic| {
        ic.expr
            .as_ref()
            .is_some_and(|ie| expr_ast_equiv_for_table(expr, ie, tbl, schema_name, alias))
    })
}

fn expr_covered_by_index(
    expr: &Expr,
    idx: &IndexSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> bool {
    if index_slot_for_expr(idx, expr, tbl, schema_name, alias).is_some() {
        return true;
    }
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::OversizedHex(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_)
        | Expr::Raise { .. } => true,
        Expr::Column(cref) => {
            if !cref_matches_single_table(cref, tbl, schema_name, alias) {
                return true;
            }
            if let Some((ci, _)) = tbl.find_column(&cref.column) {
                index_provides_table_col(idx, tbl, ci)
            } else if tbl.is_implicit_rowid_name(&cref.column) {
                !tbl.without_rowid
            } else {
                false
            }
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            expr_covered_by_index(inner, idx, tbl, schema_name, alias)
        }
        Expr::Binary { left, right, .. } => {
            expr_covered_by_index(left, idx, tbl, schema_name, alias)
                && expr_covered_by_index(right, idx, tbl, schema_name, alias)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_covered_by_index(e, idx, tbl, schema_name, alias)
                && expr_covered_by_index(low, idx, tbl, schema_name, alias)
                && expr_covered_by_index(high, idx, tbl, schema_name, alias)
        }
        Expr::InList { expr: e, list, .. } => {
            expr_covered_by_index(e, idx, tbl, schema_name, alias)
                && list
                    .iter()
                    .all(|item| expr_covered_by_index(item, idx, tbl, schema_name, alias))
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_covered_by_index(e, idx, tbl, schema_name, alias)
                && expr_covered_by_index(pattern, idx, tbl, schema_name, alias)
                && escape
                    .as_deref()
                    .is_none_or(|esc| expr_covered_by_index(esc, idx, tbl, schema_name, alias))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_deref()
                .is_none_or(|op| expr_covered_by_index(op, idx, tbl, schema_name, alias))
                && branches.iter().all(|(w, t)| {
                    expr_covered_by_index(w, idx, tbl, schema_name, alias)
                        && expr_covered_by_index(t, idx, tbl, schema_name, alias)
                })
                && else_expr
                    .as_deref()
                    .is_none_or(|el| expr_covered_by_index(el, idx, tbl, schema_name, alias))
        }
        Expr::RowValue(elems) => elems
            .iter()
            .all(|e| expr_covered_by_index(e, idx, tbl, schema_name, alias)),
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            let over_ok = match over {
                None => true,
                Some(WindowRef::Name(_)) => false,
                Some(WindowRef::Spec(ws)) => {
                    ws.base_window.is_none()
                        && ws
                            .partition_by
                            .iter()
                            .all(|e| expr_covered_by_index(e, idx, tbl, schema_name, alias))
                        && ws
                            .order_by
                            .iter()
                            .all(|ob| expr_covered_by_index(&ob.expr, idx, tbl, schema_name, alias))
                }
            };
            if !over_ok {
                return false;
            }
            let args_ok = match args {
                FunctionArgs::Star => true,
                FunctionArgs::Exprs(es) => es
                    .iter()
                    .all(|e| expr_covered_by_index(e, idx, tbl, schema_name, alias)),
            };
            let filter_ok = filter
                .as_deref()
                .is_none_or(|f| expr_covered_by_index(f, idx, tbl, schema_name, alias));
            args_ok && filter_ok
        }
        Expr::InTable { expr: e, .. } => expr_covered_by_index(e, idx, tbl, schema_name, alias),
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => false,
    }
}

pub fn index_satisfies_order_by(
    idx: &IndexSchema,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    selection: Option<&Expr>,
    order_by: &[OrderByTerm],
) -> bool {
    if order_by.is_empty() {
        return true;
    }
    let mut conjuncts = Vec::new();
    if let Some(w) = selection {
        collect_and_conjuncts(w, &mut conjuncts);
    }

    // Filter out ORDER BY terms that are already constrained to a constant by `WHERE col = const`
    let remaining_ob: Vec<&OrderByTerm> = order_by
        .iter()
        .filter(|ob| {
            !conjuncts.iter().any(|conj| {
                if let Some((cname, _)) = extract_table_col_eq(conj, tbl, schema_name, alias)
                    && let Some(ob_cname) =
                        match_single_table_col(&ob.expr, tbl, schema_name, alias)
                {
                    cname.eq_ignore_ascii_case(&ob_cname)
                } else {
                    false
                }
            })
        })
        .collect();
    if remaining_ob.is_empty() {
        return true;
    }

    // Skip leading index columns that are constrained by equality (`col = const`) in `WHERE`
    let mut first_non_eq = 0usize;
    while first_non_eq < idx.columns.len() {
        let ic = &idx.columns[first_non_eq];
        let has_eq = conjuncts
            .iter()
            .any(|conj| match_index_col_eq(conj, ic, tbl, schema_name, alias).is_some());
        if has_eq {
            first_non_eq += 1;
        } else {
            break;
        }
    }

    let rem_idx_cols = &idx.columns[first_non_eq..];
    if remaining_ob.len() > rem_idx_cols.len() {
        return false;
    }

    // Check forward or reverse scan direction
    for &reverse in &[false, true] {
        let mut ok = true;
        for (ob, ic) in remaining_ob.iter().zip(rem_idx_cols.iter()) {
            let ob_desc = ob.desc;
            let ob_nulls_last = ob.nulls_last.unwrap_or(ob_desc);
            let idx_desc = if reverse {
                !ic.sort_order.desc
            } else {
                ic.sort_order.desc
            };
            let idx_nulls_last = if reverse {
                !ic.sort_order.nulls_last
            } else {
                ic.sort_order.nulls_last
            };
            if ob_desc != idx_desc || ob_nulls_last != idx_nulls_last {
                ok = false;
                break;
            }
            if match_index_col_target(&ob.expr, ic, tbl, schema_name, alias).is_none() {
                ok = false;
                break;
            }
        }
        if ok {
            return true;
        }
    }
    false
}

fn cref_matches_single_table(
    cref: &ColumnRef,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> bool {
    if let Some(s) = &cref.schema
        && !s.eq_ignore_ascii_case(schema_name)
    {
        return false;
    }
    let effective_table = alias.unwrap_or(&tbl.name);
    if let Some(qual) = &cref.table
        && !qual.eq_ignore_ascii_case(effective_table)
    {
        return false;
    }
    true
}

fn match_single_table_col(
    expr: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> Option<String> {
    let Expr::Column(cref) = expr else {
        return None;
    };
    if !cref_matches_single_table(cref, tbl, schema_name, alias) {
        return None;
    }
    if tbl.find_column(&cref.column).is_some() || tbl.is_implicit_rowid_name(&cref.column) {
        Some(cref.column.clone())
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
        Expr::Column(_)
        | Expr::OversizedHex(_)
        | Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::InTable { .. }
        | Expr::Raise { .. } => false,
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
            name,
            args,
            filter,
            over,
            ..
        } => {
            if over.is_some() || expr_contains_aggregate(expr) || filter.is_some() {
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
    expr_contains_aggregate_with_functions(expr, &FunctionRegistry::default())
}

pub fn expr_contains_aggregate_with_functions(expr: &Expr, functions: &FunctionRegistry) -> bool {
    match expr {
        Expr::FunctionCall {
            name,
            args,
            filter,
            over,
            ..
        } => {
            if let Some(wref) = over {
                let in_args = match args {
                    FunctionArgs::Star => false,
                    FunctionArgs::Exprs(es) => es
                        .iter()
                        .any(|e| expr_contains_aggregate_with_functions(e, functions)),
                };
                let in_over = match wref {
                    WindowRef::Name(_) => false,
                    WindowRef::Spec(ws) => {
                        ws.partition_by
                            .iter()
                            .any(|e| expr_contains_aggregate_with_functions(e, functions))
                            || ws.order_by.iter().any(|ob| {
                                expr_contains_aggregate_with_functions(&ob.expr, functions)
                            })
                    }
                };
                return in_args
                    || filter
                        .as_deref()
                        .is_some_and(|e| expr_contains_aggregate_with_functions(e, functions))
                    || in_over;
            }
            let is_agg = match args {
                FunctionArgs::Star => {
                    functions.find_aggregate(name, 0).is_some()
                        || functions.has_aggregate_name(name)
                        || AggregateKind::from_name(name, true).is_some()
                }
                FunctionArgs::Exprs(es) => {
                    if functions.find_scalar(name, es.len()).is_some() {
                        false
                    } else if functions.find_aggregate(name, es.len()).is_some()
                        || functions.has_aggregate_name(name)
                    {
                        true
                    } else {
                        let upper = name.to_ascii_uppercase();
                        if (upper == "MIN" || upper == "MAX") && es.len() != 1 {
                            false
                        } else if upper == "COUNT" && es.is_empty() {
                            true
                        } else {
                            AggregateKind::from_name(name, false).is_some()
                        }
                    }
                }
            };
            if is_agg {
                return true;
            }
            filter
                .as_deref()
                .is_some_and(|e| expr_contains_aggregate_with_functions(e, functions))
                || match args {
                    FunctionArgs::Star => false,
                    FunctionArgs::Exprs(es) => es
                        .iter()
                        .any(|e| expr_contains_aggregate_with_functions(e, functions)),
                }
        }
        Expr::Unary { expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::IsNull { expr, .. }
        | Expr::Cast { expr, .. } => expr_contains_aggregate_with_functions(expr, functions),
        Expr::Binary { left, right, .. } => {
            expr_contains_aggregate_with_functions(left, functions)
                || expr_contains_aggregate_with_functions(right, functions)
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expr_contains_aggregate_with_functions(expr, functions)
                || expr_contains_aggregate_with_functions(low, functions)
                || expr_contains_aggregate_with_functions(high, functions)
        }
        Expr::InList { expr, list, .. } => {
            expr_contains_aggregate_with_functions(expr, functions)
                || list
                    .iter()
                    .any(|e| expr_contains_aggregate_with_functions(e, functions))
        }
        Expr::InTable { expr, .. } | Expr::InSubquery { expr, .. } => {
            expr_contains_aggregate_with_functions(expr, functions)
        }
        Expr::Like {
            expr,
            pattern,
            escape,
            ..
        } => {
            expr_contains_aggregate_with_functions(expr, functions)
                || expr_contains_aggregate_with_functions(pattern, functions)
                || escape
                    .as_deref()
                    .is_some_and(|e| expr_contains_aggregate_with_functions(e, functions))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_deref()
                .is_some_and(|e| expr_contains_aggregate_with_functions(e, functions))
                || branches.iter().any(|(w, t)| {
                    expr_contains_aggregate_with_functions(w, functions)
                        || expr_contains_aggregate_with_functions(t, functions)
                })
                || else_expr
                    .as_deref()
                    .is_some_and(|e| expr_contains_aggregate_with_functions(e, functions))
        }
        Expr::RowValue(elems) => elems
            .iter()
            .any(|e| expr_contains_aggregate_with_functions(e, functions)),
        _ => false,
    }
}

pub fn expr_contains_subquery(expr: &Expr) -> bool {
    match expr {
        Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::InTable { .. }
        | Expr::Raise { .. } => true,
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
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            over.is_some()
                || filter.as_deref().is_some_and(expr_contains_subquery)
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
        Expr::Column(cref) => cref.column.clone(),
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
    schema_name: &'a str,
    alias: Option<&'a str>,
    cursor: i32,
    covering_index: Option<(&'a IndexSchema, i32)>,
}

fn resolve_main_table_for_compiler(
    name: &QualifiedName,
    schema: &Schema,
) -> Result<Option<TableSchema>> {
    if let Some(s) = &name.schema {
        if s.eq_ignore_ascii_case("temp") {
            if Schema::is_schema_table_name(&name.name) {
                return Ok(None);
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", name.display_name()),
            ));
        }
        if !s.eq_ignore_ascii_case("main") {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", name.display_name()),
            ));
        }
        if Schema::is_temp_schema_table_name(&name.name) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", name.display_name()),
            ));
        }
    } else if Schema::is_temp_schema_table_name(&name.name) {
        return Ok(None);
    }

    if schema.find_view(&name.name).is_some() && schema.find_user_table(&name.name).is_none() {
        return Ok(None);
    }
    let Some(tbl) = schema.find_table(&name.name) else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        ));
    };
    Ok(Some(tbl))
}

fn resolve_db_set_table_for_compiler<'a>(
    name: &QualifiedName,
    db_set: &'a DatabaseSet,
) -> Result<Option<(DatabaseId, &'a str, &'a Schema, TableSchema)>> {
    let Some((db_id, rel)) = db_set.find_relation_in_set(name)? else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        ));
    };
    let tbl = match rel {
        ResolvedRelation::Table(t) => *t,
        ResolvedRelation::View(_) => return Ok(None),
    };
    if tbl.root_pgno == 0 {
        return Ok(None);
    }
    let Some(slot) = db_set.slot(db_id) else {
        return Ok(None);
    };
    if slot.btree.is_none() {
        return Ok(None);
    }
    Ok(Some((db_id, slot.name.as_str(), &slot.schema, tbl)))
}

/// Attempts to compile `stmt` into a pure [`VdbeProgram`] for direct step-by-step execution
/// on [`sqliters_vdbe::VdbeVm`]. Returns `Ok(None)` if the query uses multi-table joins, CTEs,
/// compound set operations, aggregates/`GROUP BY`, `DISTINCT`, or subqueries (handled by `engine`).
pub fn try_compile_select(stmt: &SelectStmt, schema: &Schema) -> Result<Option<VdbeProgram>> {
    try_compile_select_with_functions(stmt, schema, &FunctionRegistry::default())
}

/// Attempts to compile `stmt` into a [`VdbeProgram`] using connection-registered `functions`.
pub fn try_compile_select_with_functions(
    stmt: &SelectStmt,
    schema: &Schema,
    functions: &FunctionRegistry,
) -> Result<Option<VdbeProgram>> {
    try_compile_select_core(stmt, functions, |name| {
        let Some(tbl) = resolve_main_table_for_compiler(name, schema)? else {
            return Ok(None);
        };
        Ok(Some((DatabaseId::MAIN, "main", schema, tbl)))
    })
}

/// Multi-database variant of [`try_compile_select_with_functions`] resolving unqualified tables via
/// `temp -> main -> attached` and stamping resolved [`DatabaseId`] slots on all B-tree/transaction
/// instructions.
pub fn try_compile_select_in_db_set(
    stmt: &SelectStmt,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
) -> Result<Option<VdbeProgram>> {
    let res = try_compile_select_core(stmt, functions, |name| {
        resolve_db_set_table_for_compiler(name, db_set)
    })?;
    if let Some(ref prog) = res
        && prog.column_names.len() > db_set.column_limit()
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "too many columns in result set",
        ));
    }
    Ok(res)
}

/// Returns `Ok(None)` whenever `ins` specifies `UPSERT` (`ON CONFLICT ...`), targets a relation
/// with active triggers, or uses any construct requiring full interpreter execution in `engine.rs`.
pub fn try_compile_insert(ins: &InsertStmt, schema: &Schema) -> Result<Option<VdbeProgram>> {
    if !ins.upsert_clauses.is_empty() {
        return Ok(None);
    }
    if schema.triggers.iter().any(|tr| {
        tr.is_active()
            && tr.tbl_name.eq_ignore_ascii_case(&ins.table.name)
            && tr.matches_event_and_columns(TriggerEventKind::Insert, None)
    }) {
        return Ok(None);
    }
    Ok(None)
}

/// Multi-database variant of [`try_compile_insert`]; delegates to `engine.rs` (`Ok(None)`) whenever
/// `UPSERT` is present, active triggers exist on the target relation (in `target_db_id` or `TEMP`),
/// or foreign keys / interpreter features apply.
pub fn try_compile_insert_in_db_set(
    ins: &InsertStmt,
    db_set: &DatabaseSet,
    _functions: &FunctionRegistry,
) -> Result<Option<VdbeProgram>> {
    if !ins.upsert_clauses.is_empty() {
        return Ok(None);
    }
    if let Ok(Some((target_db_id, rel))) = db_set.find_relation_in_set(&ins.table) {
        let rel_name = match &rel {
            ResolvedRelation::Table(t) => t.name.as_str(),
            ResolvedRelation::View(v) => v.name.as_str(),
        };
        if matches!(rel, ResolvedRelation::View(_))
            || db_set.has_active_triggers_for_event(
                target_db_id,
                rel_name,
                TriggerEventKind::Insert,
                None,
            )
        {
            return Ok(None);
        }
    }
    Ok(None)
}

fn try_compile_select_core<'a, F>(
    stmt: &SelectStmt,
    functions: &FunctionRegistry,
    resolve_table: F,
) -> Result<Option<VdbeProgram>>
where
    F: FnOnce(&QualifiedName) -> Result<Option<(DatabaseId, &'a str, &'a Schema, TableSchema)>>,
{
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
        windows,
    } = &stmt.body.head
    else {
        return Ok(None);
    };

    if *distinct
        || !group_by.is_empty()
        || having.is_some()
        || !windows.is_empty()
        || core_has_window(&stmt.body.head, &stmt.order_by)
    {
        return Ok(None);
    }

    // Check for aggregates or subqueries in projection / selection / order_by / limit
    for rc in projection {
        if let ResultColumn::Expr { expr, .. } = rc
            && (expr_contains_aggregate_with_functions(expr, functions)
                || expr_contains_subquery(expr))
        {
            return Ok(None);
        }
    }
    if let Some(w) = selection
        && (expr_contains_aggregate_with_functions(w, functions) || expr_contains_subquery(w))
    {
        return Ok(None);
    }
    for ob in &stmt.order_by {
        if expr_contains_aggregate_with_functions(&ob.expr, functions)
            || expr_contains_subquery(&ob.expr)
        {
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
            compile_tableless_select(
                projection,
                selection.as_ref(),
                stmt.limit.as_ref(),
                functions,
            )
        }
        Some(FromClause { head, joins }) if joins.is_empty() => {
            let TableOrSubquery::Table {
                name,
                table_args: None,
                alias,
                indexed_by,
            } = head
            else {
                return Ok(None);
            };
            if matches!(indexed_by, Some(IndexHint::IndexedBy(_))) {
                return Ok(None);
            }
            let Some((db_id, slot_name, slot_schema, tbl)) = resolve_table(name)? else {
                return Ok(None);
            };
            if tbl.columns.iter().any(|c| c.generated.is_some()) {
                return Ok(None);
            }
            compile_single_table_select(
                SingleTableSource {
                    db_id,
                    schema_name: slot_name,
                    tbl: &tbl,
                    alias: alias.as_deref(),
                    indexed_by: indexed_by.as_ref(),
                },
                projection,
                selection.as_ref(),
                &stmt.order_by,
                stmt.limit.as_ref(),
                slot_schema,
                functions,
            )
        }
        Some(_) => Ok(None),
    }
}

fn compile_tableless_select(
    projection: &[ResultColumn],
    selection: Option<&Expr>,
    limit: Option<&LimitClause>,
    functions: &FunctionRegistry,
) -> Result<Option<VdbeProgram>> {
    let mut prog = VdbeProgram::new();
    let init_addr = prog.emit(Instruction::new(Opcode::Init, 0, 0, 0));

    let mut proj_exprs = Vec::with_capacity(projection.len());
    let mut col_names = Vec::with_capacity(projection.len());
    for rc in projection {
        match rc {
            ResultColumn::Star | ResultColumn::TableStar { .. } => {
                return Err(SqliteError::new(ErrorCode::Error, "no tables specified"));
            }
            ResultColumn::Expr {
                expr,
                alias,
                span_text,
            } => {
                let name = alias.clone().unwrap_or_else(|| match expr {
                    Expr::Column(cref) => cref.column.clone(),
                    _ => span_text.clone().unwrap_or_else(|| expr_column_label(expr)),
                });
                col_names.push(name);
                proj_exprs.push(expr.clone());
            }
        }
    }
    prog.column_names = col_names;

    let mut jumps_to_halt = Vec::new();

    if let Some(lim) = limit {
        let lim_reg = prog.alloc_register();
        if compile_expr(&mut prog, &lim.limit, None, functions, lim_reg).is_err() {
            return Ok(None);
        }
        let zero_reg = prog.alloc_register();
        prog.emit(Instruction::new(Opcode::Integer, 0, zero_reg, 0));
        let le_reg = prog.alloc_register();
        prog.emit(Instruction::new(Opcode::Le, lim_reg, zero_reg, le_reg));
        jumps_to_halt.push(prog.emit(Instruction::new(Opcode::JumpIfTrue, le_reg, 0, 0)));

        if let Some(off_expr) = &lim.offset {
            let off_reg = prog.alloc_register();
            if compile_expr(&mut prog, off_expr, None, functions, off_reg).is_err() {
                return Ok(None);
            }
            let gt_reg = prog.alloc_register();
            prog.emit(Instruction::new(Opcode::Gt, off_reg, zero_reg, gt_reg));
            jumps_to_halt.push(prog.emit(Instruction::new(Opcode::JumpIfTrue, gt_reg, 0, 0)));
        }
    }

    if let Some(w) = selection {
        let cond_reg = prog.alloc_register();
        if compile_expr(&mut prog, w, None, functions, cond_reg).is_err() {
            return Ok(None);
        }
        jumps_to_halt.push(prog.emit(Instruction::new(Opcode::JumpIfFalse, cond_reg, 0, 1)));
    }

    let out_base = prog.alloc_registers(proj_exprs.len());
    for (i, e) in proj_exprs.iter().enumerate() {
        if compile_expr(&mut prog, e, None, functions, out_base + i as i32).is_err() {
            return Ok(None);
        }
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
    Ok(Some(prog))
}

struct SingleTableSource<'a> {
    db_id: DatabaseId,
    schema_name: &'a str,
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
    functions: &FunctionRegistry,
) -> Result<Option<VdbeProgram>> {
    let SingleTableSource {
        db_id,
        schema_name,
        tbl,
        alias,
        indexed_by,
    } = source;
    let mut prog = VdbeProgram::new();
    let init_addr = prog.emit(Instruction::new(Opcode::Init, 0, 0, 0));

    // Expand projection (`*`, `t.*`, `s.t.*`, expressions)
    let table_ref_name = alias.unwrap_or(&tbl.name);
    let mut expanded: Vec<(Expr, String, Option<String>)> = Vec::new();
    for rc in projection {
        match rc {
            ResultColumn::Star => {
                for c in &tbl.columns {
                    expanded.push((
                        Expr::Column(ColumnRef::unqualified(c.name.clone())),
                        c.name.clone(),
                        None,
                    ));
                }
            }
            ResultColumn::TableStar {
                schema: star_schema,
                table: tname,
            } => {
                let schema_ok = match star_schema.as_deref() {
                    Some(s) => s.eq_ignore_ascii_case(schema_name),
                    None => true,
                };
                if !schema_ok || !tname.eq_ignore_ascii_case(table_ref_name) {
                    let disp = match star_schema {
                        Some(s) => format!("{s}.{tname}"),
                        None => tname.clone(),
                    };
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such table: {disp}"),
                    ));
                }
                for c in &tbl.columns {
                    expanded.push((
                        Expr::Column(ColumnRef {
                            schema: star_schema.clone(),
                            table: Some(tname.clone()),
                            column: c.name.clone(),
                        }),
                        c.name.clone(),
                        None,
                    ));
                }
            }
            ResultColumn::Expr {
                expr,
                alias: al,
                span_text,
            } => {
                let label = al.clone().unwrap_or_else(|| match expr {
                    Expr::Column(cref) => {
                        if let Some((_, col)) = tbl.find_column(&cref.column) {
                            col.name.clone()
                        } else if tbl.is_implicit_rowid_name(&cref.column) {
                            tbl.rowid_alias_col
                                .map(|idx| tbl.columns[idx].name.clone())
                                .unwrap_or_else(|| "rowid".to_owned())
                        } else {
                            cref.column.clone()
                        }
                    }
                    _ => span_text.clone().unwrap_or_else(|| expr_column_label(expr)),
                });
                expanded.push((expr.clone(), label, al.clone()));
            }
        }
    }
    prog.column_names = expanded.iter().map(|(_, name, _)| name.clone()).collect();

    // Choose access plan first so covering index queries do NOT open or seek the table cursor!
    if matches!(indexed_by, Some(IndexHint::IndexedBy(_))) {
        return Ok(None);
    }
    let plan = choose_access_plan_with_context(
        tbl,
        schema_name,
        alias,
        selection,
        schema,
        QueryPlanContext {
            projection: Some(projection),
            group_by: &[],
            having: None,
            order_by,
            indexed_by,
            is_count_star_only: false,
        },
    );

    // Delegate range scans, IN-list seeks, and MULTI-INDEX OR to the relational engine with B-Tree range seeks
    if matches!(
        plan,
        AccessPlan::RowidRange { .. }
            | AccessPlan::RowidIn { .. }
            | AccessPlan::IndexRangeScan { .. }
            | AccessPlan::MultiIndexOr { .. }
    ) {
        return Ok(None);
    }

    let covering_secondary_idx: Option<&IndexSchema> = match &plan {
        AccessPlan::IndexFullScan {
            index,
            is_covering: true,
        }
        | AccessPlan::IndexSeek {
            index,
            is_covering: true,
            ..
        } if !(tbl.without_rowid && index.origin == "pk" && index.root_pgno == tbl.root_pgno) => {
            Some(index)
        }
        _ => None,
    };

    // Begin read transaction & open table or covering index cursor
    let tbl_cursor = prog.alloc_cursor();
    let mut idx_cursor_opt = None;
    let body_start = prog.current_addr();
    prog.emit(Instruction::new(Opcode::Transaction, db_id.index() as i32, 0, 0).with_db(db_id));

    let covering_binding = if let Some(cov_idx) = covering_secondary_idx {
        let icur = prog.alloc_cursor();
        idx_cursor_opt = Some(icur);
        prog.emit(
            Instruction::new(Opcode::OpenReadIndex, icur, cov_idx.root_pgno as i32, 0)
                .with_db(db_id)
                .with_p4(P4::KeyInfo(cov_idx.key_sort_orders()))
                .with_comment(cov_idx.name.clone()),
        );
        Some((cov_idx, icur))
    } else {
        if tbl.without_rowid {
            prog.emit(
                Instruction::new(Opcode::OpenReadIndex, tbl_cursor, tbl.root_pgno as i32, 0)
                    .with_db(db_id)
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
                .with_db(db_id)
                .with_comment(tbl.name.clone()),
            );
        }
        None
    };

    let binding = TableBinding {
        tbl,
        schema_name,
        alias,
        cursor: tbl_cursor,
        covering_index: covering_binding,
    };

    // Optional LIMIT / OFFSET registers initialized before loop
    let limit_regs = if let Some(lim) = limit {
        let rem_reg = prog.alloc_register();
        compile_expr(&mut prog, &lim.limit, None, functions, rem_reg)?;
        let off_reg = if let Some(off_e) = &lim.offset {
            let r = prog.alloc_register();
            compile_expr(&mut prog, off_e, None, functions, r)?;
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

    // Optional Sorter setup for ORDER BY (elided when a natural ascending rowid scan,
    // `WITHOUT ROWID` primary B-Tree scan, or forward index scan already satisfies `ORDER BY`!)
    let sorter_setup = if !order_by.is_empty() {
        let mut sort_sources = Vec::with_capacity(order_by.len());
        let mut sort_orders = Vec::with_capacity(order_by.len());
        let mut resolved_exprs = Vec::with_capacity(order_by.len());
        for ob in order_by {
            let Some((src, coll_ref_expr)) = resolve_order_by_source(&ob.expr, &expanded, tbl)
            else {
                return Ok(None);
            };
            let coll_name = if let Expr::Collate { collation, .. } = &ob.expr {
                collation.as_str()
            } else {
                infer_expr_collation_name(&coll_ref_expr, tbl)
            };
            let Some(coll) = Collation::from_name(coll_name) else {
                return Ok(None);
            };
            let desc = ob.desc;
            let nulls_last = ob.nulls_last.unwrap_or(desc);
            resolved_exprs.push(coll_ref_expr);
            sort_sources.push(src);
            sort_orders.push(KeySortOrder {
                desc,
                nulls_last,
                collation: coll,
                custom_collation: None,
            });
        }

        let natural_rowid_order = order_by.len() == 1
            && !order_by[0].desc
            && matches!(plan, AccessPlan::FullScan | AccessPlan::RowidSeek { .. })
            && resolved_exprs
                .first()
                .is_some_and(|e| is_table_rowid_column_expr(e, tbl, schema_name, alias));

        let natural_without_rowid_order = tbl.without_rowid
            && matches!(plan, AccessPlan::FullScan)
            && order_by.len() <= tbl.without_rowid_pk_cols.len()
            && resolved_exprs
                .iter()
                .zip(sort_orders.iter())
                .zip(tbl.without_rowid_pk_cols.iter())
                .all(|((re, so), pk_ic)| {
                    *so == pk_ic.sort_order
                        && pk_ic.col_idx.is_some_and(|pk_ci| {
                            is_exact_table_column_expr(re, tbl, schema_name, alias, pk_ci)
                        })
                });

        let natural_index_order = match &plan {
            AccessPlan::IndexFullScan { index, .. } => {
                order_by.len() <= index.columns.len()
                    && resolved_exprs
                        .iter()
                        .zip(sort_orders.iter())
                        .zip(index.columns.iter())
                        .all(|((re, so), ic)| {
                            *so == ic.sort_order
                                && match_index_col_target(re, ic, tbl, schema_name, alias).is_some()
                        })
            }
            _ => false,
        };

        if natural_rowid_order || natural_without_rowid_order || natural_index_order {
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
        AccessPlan::IndexFullScan { index, is_covering } => {
            let icur = if let Some((_, cov_cur)) = covering_binding {
                cov_cur
            } else {
                let c = prog.alloc_cursor();
                idx_cursor_opt = Some(c);
                prog.emit(
                    Instruction::new(Opcode::OpenReadIndex, c, index.root_pgno as i32, 0)
                        .with_db(db_id)
                        .with_p4(P4::KeyInfo(index.key_sort_orders()))
                        .with_comment(index.name.clone()),
                );
                c
            };
            seek_advance_cursor = Some(icur);
            let rewind = prog.emit(Instruction::new(Opcode::Rewind, icur, 0, 0));
            jumps_after_scan.push(rewind);
            let top = prog.current_addr();
            if !*is_covering {
                if tbl.without_rowid {
                    let mut pk_slots_in_idx = Vec::new();
                    for pk_col in &tbl.without_rowid_pk_cols {
                        let Some(slot) = index.without_rowid_pk_slot_in_index(pk_col) else {
                            return Ok(None);
                        };
                        pk_slots_in_idx.push(slot);
                    }
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
            }
            top
        }
        AccessPlan::RowidSeek { key_expr } => {
            let key_reg = prog.alloc_register();
            compile_expr(&mut prog, key_expr, None, functions, key_reg)?;
            let seek = prog.emit(Instruction::new(Opcode::SeekRowid, tbl_cursor, 0, key_reg));
            jumps_after_scan.push(seek);
            prog.current_addr()
        }
        AccessPlan::IndexSeek {
            index,
            prefix_exprs,
            is_covering,
        } => {
            let is_primary_without_rowid =
                tbl.without_rowid && index.origin == "pk" && index.root_pgno == tbl.root_pgno;

            if is_primary_without_rowid {
                let key_base = prog.alloc_registers(prefix_exprs.len());
                for (i, ke) in prefix_exprs.iter().enumerate() {
                    let r = key_base + i as i32;
                    compile_expr(&mut prog, ke, None, functions, r)?;
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
                if tbl.without_rowid && !*is_covering {
                    for pk_col in &tbl.without_rowid_pk_cols {
                        let Some(slot) = index.without_rowid_pk_slot_in_index(pk_col) else {
                            return Ok(None);
                        };
                        pk_slots_in_idx.push(slot);
                    }
                }

                let icur = if let Some((_, cov_cur)) = covering_binding {
                    cov_cur
                } else {
                    let c = prog.alloc_cursor();
                    idx_cursor_opt = Some(c);
                    prog.emit(
                        Instruction::new(Opcode::OpenReadIndex, c, index.root_pgno as i32, 0)
                            .with_db(db_id)
                            .with_p4(P4::KeyInfo(index.key_sort_orders()))
                            .with_comment(index.name.clone()),
                    );
                    c
                };
                seek_advance_cursor = Some(icur);
                let key_base = prog.alloc_registers(prefix_exprs.len());
                for (i, ke) in prefix_exprs.iter().enumerate() {
                    let r = key_base + i as i32;
                    compile_expr(&mut prog, ke, None, functions, r)?;
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
                if !*is_covering {
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
                }
                top
            }
        }
        AccessPlan::RowidRange { .. }
        | AccessPlan::RowidIn { .. }
        | AccessPlan::IndexRangeScan { .. }
        | AccessPlan::MultiIndexOr { .. } => unreachable!(),
    };

    // Evaluate full WHERE filter (also handles residual conjuncts after index/rowid seek!)
    if let Some(w) = selection {
        let cond_reg = prog.alloc_register();
        if compile_expr(&mut prog, w, Some(&binding), functions, cond_reg).is_err() {
            // If `WHERE` references a SELECT result-column alias, fall back to the relational engine
            return Ok(None);
        }
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
                functions,
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
                    compile_expr(
                        &mut prog,
                        se,
                        Some(&binding),
                        functions,
                        row_base + i as i32,
                    )?;
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
            compile_expr(
                &mut prog,
                pe,
                Some(&binding),
                functions,
                out_base + j as i32,
            )?;
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
        AccessPlan::IndexFullScan { .. } => {
            let adv_cur = seek_advance_cursor.expect("index full scan cursor allocated");
            prog.emit(Instruction::new(Opcode::Next, adv_cur, loop_top as i32, 0));
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
        AccessPlan::RowidRange { .. }
        | AccessPlan::RowidIn { .. }
        | AccessPlan::IndexRangeScan { .. }
        | AccessPlan::MultiIndexOr { .. } => unreachable!(),
    }

    let after_scan = prog.current_addr();
    for j in jumps_after_scan {
        prog.patch_p2(j, after_scan);
    }

    if let Some(icur) = idx_cursor_opt {
        prog.emit(Instruction::new(Opcode::Close, icur, 0, 0));
    }
    if covering_binding.is_none() {
        prog.emit(Instruction::new(Opcode::Close, tbl_cursor, 0, 0));
    }

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

fn is_table_rowid_column_expr(
    expr: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
) -> bool {
    if tbl.without_rowid {
        return false;
    }
    let Expr::Column(cref) = expr else {
        return false;
    };
    if !cref_matches_single_table(cref, tbl, schema_name, alias) {
        return false;
    }
    tbl.is_implicit_rowid_name(&cref.column)
        || tbl
            .rowid_alias_col
            .is_some_and(|ipk| tbl.columns[ipk].name.eq_ignore_ascii_case(&cref.column))
}

fn is_exact_table_column_expr(
    expr: &Expr,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    target_col_idx: usize,
) -> bool {
    let Expr::Column(cref) = expr else {
        return false;
    };
    if !cref_matches_single_table(cref, tbl, schema_name, alias) {
        return false;
    }
    tbl.columns
        .get(target_col_idx)
        .is_some_and(|c| c.name.eq_ignore_ascii_case(&cref.column))
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
    if let Expr::Column(ColumnRef {
        schema: None,
        table: None,
        column,
    }) = inner
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
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => {
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
        | Expr::IsNull { expr: inner, .. }
        | Expr::InTable { expr: inner, .. } => expr_uses_select_alias(inner, expanded, tbl),
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

fn infer_expr_collation_name<'a>(expr: &'a Expr, tbl: &'a TableSchema) -> &'a str {
    match expr {
        Expr::Collate { collation, .. } => collation.as_str(),
        Expr::Column(cref) => tbl
            .find_column(&cref.column)
            .map_or("BINARY", |(_, c)| c.collation_name()),
        _ => "BINARY",
    }
}

// ============================================================================
// Expression -> VDBE Register Compiler (`sqlite3ExprCode`)
// ============================================================================

fn compile_expr(
    prog: &mut VdbeProgram,
    expr: &Expr,
    binding: Option<&TableBinding<'_>>,
    functions: &FunctionRegistry,
    dest: i32,
) -> Result<()> {
    if !matches!(expr, Expr::Column(_))
        && let Some(b) = binding
        && let Some((cov_idx, cov_cur)) = b.covering_index
        && let Some(slot) = index_slot_for_expr(cov_idx, expr, b.tbl, b.schema_name, b.alias)
    {
        prog.emit(Instruction::new(Opcode::Column, cov_cur, slot as i32, dest));
        return Ok(());
    }

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
        Expr::OversizedHex(raw) => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("hex literal too big: {raw}"),
            ));
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
        Expr::Column(cref) => {
            let Some(b) = binding else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: {}", cref.display_name()),
                ));
            };
            if !cref_matches_single_table(cref, b.tbl, b.schema_name, b.alias) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: {}", cref.display_name()),
                ));
            }
            if let Some((cov_idx, cov_cur)) = b.covering_index {
                if let Some((col_idx, col_schema)) = b.tbl.find_column(&cref.column) {
                    if let Some(slot) = index_slot_for_table_col(cov_idx, b.tbl, col_idx) {
                        prog.emit(
                            Instruction::new(Opcode::Column, cov_cur, slot as i32, dest)
                                .with_p4(col_schema.default_p4_for_table(b.tbl.strict))
                                .with_comment(col_schema.name.clone()),
                        );
                    } else if !b.tbl.without_rowid && b.tbl.rowid_alias_col == Some(col_idx) {
                        prog.emit(
                            Instruction::new(Opcode::Rowid, cov_cur, dest, 0)
                                .with_comment(col_schema.name.clone()),
                        );
                    } else if let Some((ref gen_expr, _)) = col_schema.generated {
                        compile_expr(prog, gen_expr, Some(b), functions, dest)?;
                    } else {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such column: {}", cref.display_name()),
                        ));
                    }
                } else if b.tbl.is_implicit_rowid_name(&cref.column) {
                    prog.emit(Instruction::new(Opcode::Rowid, cov_cur, dest, 0));
                } else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such column: {}", cref.display_name()),
                    ));
                }
            } else if let Some((col_idx, col_schema)) = b.tbl.find_column(&cref.column) {
                if b.tbl.rowid_alias_col == Some(col_idx) {
                    // C SQLite `insert.c:1361`: INTEGER PRIMARY KEY slot in record is NULL; read from B+Tree rowid!
                    prog.emit(
                        Instruction::new(Opcode::Rowid, b.cursor, dest, 0)
                            .with_comment(col_schema.name.clone()),
                    );
                } else if let Some((ref gen_expr, false)) = col_schema.generated {
                    compile_expr(prog, gen_expr, Some(b), functions, dest)?;
                } else {
                    let phys_slot = b.tbl.physical_record_slot(col_idx);
                    prog.emit(
                        Instruction::new(Opcode::Column, b.cursor, phys_slot as i32, dest)
                            .with_p4(col_schema.default_p4_for_table(b.tbl.strict))
                            .with_comment(col_schema.name.clone()),
                    );
                }
            } else if b.tbl.is_implicit_rowid_name(&cref.column) {
                prog.emit(Instruction::new(Opcode::Rowid, b.cursor, dest, 0));
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: {}", cref.display_name()),
                ));
            }
        }
        Expr::Collate { expr: inner, .. } => {
            compile_expr(prog, inner, binding, functions, dest)?;
        }
        Expr::Unary { op, expr: inner } => match op {
            UnaryOp::Plus => {
                // Unary `+` is a no-op expression wrapper in SQLite (`+col` strips affinity)
                compile_expr(prog, inner, binding, functions, dest)?;
            }
            UnaryOp::Negate => {
                let tmp = prog.alloc_register();
                compile_expr(prog, inner, binding, functions, tmp)?;
                prog.emit(Instruction::new(Opcode::Negate, tmp, dest, 0));
            }
            UnaryOp::BitNot => {
                let tmp = prog.alloc_register();
                compile_expr(prog, inner, binding, functions, tmp)?;
                prog.emit(Instruction::new(Opcode::BitNot, tmp, dest, 0));
            }
            UnaryOp::Not => {
                let tmp = prog.alloc_register();
                compile_expr(prog, inner, binding, functions, tmp)?;
                prog.emit(Instruction::new(Opcode::Not, tmp, dest, 0));
            }
        },
        Expr::Binary { left, op, right } => {
            let lhs = prog.alloc_register();
            let rhs = prog.alloc_register();
            compile_expr(prog, left, binding, functions, lhs)?;
            compile_expr(prog, right, binding, functions, rhs)?;
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
                let l_aff = compiler_expr_affinity(
                    left,
                    binding.map(|b| (b.tbl, b.schema_name)),
                    binding.and_then(|b| b.alias),
                );
                let r_aff = compiler_expr_affinity(
                    right,
                    binding.map(|b| (b.tbl, b.schema_name)),
                    binding.and_then(|b| b.alias),
                );
                let comp_aff = sqlite_compare_affinity(l_aff, r_aff);
                if let Some(custom_coll) = resolve_custom_binary_collation(left, right, binding) {
                    ins = ins
                        .with_p4(P4::Str(custom_coll))
                        .with_p5(u16::from(affinity_to_p5(comp_aff)));
                } else {
                    let coll = resolve_binary_collation(left, right, binding);
                    ins = ins
                        .with_p4(P4::Collation(coll))
                        .with_p5(u16::from(affinity_to_p5(comp_aff)));
                }
            }
            prog.emit(ins);
        }
        Expr::IsNull {
            expr: inner,
            negated,
        } => {
            let tmp = prog.alloc_register();
            compile_expr(prog, inner, binding, functions, tmp)?;
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
            compile_expr(prog, inner, binding, functions, tmp)?;
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
                    functions,
                    dest,
                )?;
            } else {
                compile_expr(prog, &combined, binding, functions, dest)?;
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
                if list.len() >= 2 {
                    let eph_cur = prog.alloc_cursor();
                    prog.emit(Instruction::new(Opcode::OpenEphemeral, eph_cur, 1, 0));
                }
                let mut disj: Option<Expr> = None;
                for item in list {
                    // Per `sqlite/src/expr.c` (`exprINAffinity`), `lhs IN (list)` applies ONLY
                    // `lhs`'s affinity and collation, never propagating an RHS column's affinity
                    // or collation. Unary `+` preserves runtime value while stripping RHS column
                    // affinity/collation metadata.
                    let rhs_stripped = Expr::Unary {
                        op: UnaryOp::Plus,
                        expr: Box::new(item.clone()),
                    };
                    let eq = Expr::Binary {
                        left: lhs_e.clone(),
                        op: BinaryOp::Eq,
                        right: Box::new(rhs_stripped),
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
                compile_expr(prog, &final_e, binding, functions, dest)?;
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
                LikeOp::Like => "like",
                LikeOp::Glob => "glob",
                LikeOp::Regexp => "regexp",
                LikeOp::Match => "match",
            };
            let argc = if escape.is_some() { 3 } else { 2 };
            functions.validate_call(fname, false, argc, false, false, false)?;
            let arg_base = prog.alloc_registers(argc);
            // Note: SQLite's LIKE/GLOB/REGEXP/MATCH function takes `(pattern, string [, escape])`!
            compile_expr(prog, pattern, binding, functions, arg_base)?;
            compile_expr(prog, txt_e, binding, functions, arg_base + 1)?;
            if let Some(esc) = escape {
                compile_expr(prog, esc, binding, functions, arg_base + 2)?;
            }
            let needs_bool_norm = matches!(op, LikeOp::Regexp | LikeOp::Match)
                || functions.find_scalar(fname, argc).is_some();
            let res_reg = if *negated || needs_bool_norm {
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
            } else if needs_bool_norm {
                prog.emit(Instruction::new(Opcode::Not, res_reg, res_reg, 0));
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
                compile_expr(prog, op_e, binding, functions, r)?;
                Some(r)
            } else {
                None
            };

            let mut end_jumps = Vec::new();
            for (when_e, then_e) in branches {
                let cond_reg = prog.alloc_register();
                if let Some(base_r) = op_reg {
                    let w_reg = prog.alloc_register();
                    compile_expr(prog, when_e, binding, functions, w_reg)?;
                    let op_e = operand.as_ref().unwrap();
                    let l_aff = compiler_expr_affinity(
                        op_e,
                        binding.map(|b| (b.tbl, b.schema_name)),
                        binding.and_then(|b| b.alias),
                    );
                    let r_aff = compiler_expr_affinity(
                        when_e,
                        binding.map(|b| (b.tbl, b.schema_name)),
                        binding.and_then(|b| b.alias),
                    );
                    let comp_aff = sqlite_compare_affinity(l_aff, r_aff);
                    let mut ins = Instruction::new(Opcode::Eq, base_r, w_reg, cond_reg)
                        .with_p5(u16::from(affinity_to_p5(comp_aff)));
                    if let Some(custom_coll) =
                        resolve_custom_binary_collation(op_e, when_e, binding)
                    {
                        ins = ins.with_p4(P4::Str(custom_coll));
                    } else {
                        let coll = resolve_binary_collation(op_e, when_e, binding);
                        ins = ins.with_p4(P4::Collation(coll));
                    }
                    prog.emit(ins);
                } else {
                    compile_expr(prog, when_e, binding, functions, cond_reg)?;
                }
                let j_false = prog.emit(Instruction::new(Opcode::JumpIfFalse, cond_reg, 0, 1));
                compile_expr(prog, then_e, binding, functions, dest)?;
                end_jumps.push(prog.emit(goto_placeholder()));
                let next_branch = prog.current_addr();
                prog.patch_p2(j_false, next_branch);
            }

            if let Some(el) = else_expr {
                compile_expr(prog, el, binding, functions, dest)?;
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
            over,
        } => {
            if over.is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("misuse of window function {name}()"),
                ));
            }
            let (star_args, expr_list) = match args {
                FunctionArgs::Star => (true, Vec::new()),
                FunctionArgs::Exprs(es) => (false, es.clone()),
            };
            functions.validate_call(
                name,
                star_args,
                expr_list.len(),
                *distinct,
                filter.is_some(),
                false,
            )?;
            if !star_args
                && !*distinct
                && filter.is_none()
                && functions.find_scalar(name, expr_list.len()).is_none()
            {
                let upper = name.to_ascii_uppercase();
                match upper.as_str() {
                    "COALESCE" | "IFNULL" => {
                        compile_expr(prog, &expr_list[0], binding, functions, dest)?;
                        let mut end_jumps = Vec::with_capacity(expr_list.len().saturating_sub(1));
                        for arg in &expr_list[1..] {
                            end_jumps.push(prog.emit(Instruction::new(
                                Opcode::NotNull,
                                dest,
                                0,
                                0,
                            )));
                            compile_expr(prog, arg, binding, functions, dest)?;
                        }
                        let end_addr = prog.current_addr();
                        for j in end_jumps {
                            prog.patch_p2(j, end_addr);
                        }
                        return Ok(());
                    }
                    "IIF" | "IF" => {
                        let mut branches = Vec::with_capacity(expr_list.len() / 2);
                        let mut idx = 0;
                        while idx + 1 < expr_list.len() {
                            branches.push((expr_list[idx].clone(), expr_list[idx + 1].clone()));
                            idx += 2;
                        }
                        let else_expr = if idx < expr_list.len() {
                            Some(Box::new(expr_list[idx].clone()))
                        } else {
                            None
                        };
                        let case_expr = Expr::Case {
                            operand: None,
                            branches,
                            else_expr,
                        };
                        return compile_expr(prog, &case_expr, binding, functions, dest);
                    }
                    "NULLIF" if expr_list.len() == 2 => {
                        compile_expr(prog, &expr_list[0], binding, functions, dest)?;
                        let r1 = prog.alloc_register();
                        compile_expr(prog, &expr_list[1], binding, functions, r1)?;
                        let eq_r = prog.alloc_register();
                        let mut ins = Instruction::new(Opcode::Eq, dest, r1, eq_r);
                        if let Some(custom_coll) =
                            resolve_custom_binary_collation(&expr_list[0], &expr_list[1], binding)
                        {
                            ins = ins.with_p4(P4::Str(custom_coll));
                        } else {
                            let coll =
                                resolve_binary_collation(&expr_list[0], &expr_list[1], binding);
                            ins = ins.with_p4(P4::Collation(coll));
                        }
                        prog.emit(ins);
                        let skip_null =
                            prog.emit(Instruction::new(Opcode::JumpIfFalse, eq_r, 0, 1));
                        prog.emit(Instruction::new(Opcode::Null, 0, dest, 0));
                        let end_addr = prog.current_addr();
                        prog.patch_p2(skip_null, end_addr);
                        return Ok(());
                    }
                    "MIN" | "MAX" if expr_list.len() >= 2 => {
                        let first_custom = expr_list
                            .iter()
                            .find_map(|a| resolve_custom_binary_collation(a, &Expr::Null, binding));
                        let first_builtin = expr_list
                            .iter()
                            .map(|a| resolve_binary_collation(a, &Expr::Null, binding))
                            .find(|c| *c != Collation::Binary)
                            .unwrap_or(Collation::Binary);
                        if first_custom.is_some() || first_builtin != Collation::Binary {
                            let is_max = upper == "MAX";
                            let cmp_op = if is_max { Opcode::Gt } else { Opcode::Lt };
                            compile_expr(prog, &expr_list[0], binding, functions, dest)?;
                            let mut null_jumps = Vec::with_capacity(expr_list.len());
                            null_jumps.push(prog.emit(Instruction::new(
                                Opcode::IsNull,
                                dest,
                                0,
                                0,
                            )));
                            let cand_reg = prog.alloc_register();
                            let cmp_reg = prog.alloc_register();
                            for arg in &expr_list[1..] {
                                compile_expr(prog, arg, binding, functions, cand_reg)?;
                                null_jumps.push(prog.emit(Instruction::new(
                                    Opcode::IsNull,
                                    cand_reg,
                                    0,
                                    0,
                                )));
                                let mut ins = Instruction::new(cmp_op, cand_reg, dest, cmp_reg);
                                if let Some(ref cname) = first_custom {
                                    ins = ins.with_p4(P4::Str(cname.clone()));
                                } else {
                                    ins = ins.with_p4(P4::Collation(first_builtin));
                                }
                                prog.emit(ins);
                                let skip_copy =
                                    prog.emit(Instruction::new(Opcode::JumpIfFalse, cmp_reg, 0, 1));
                                prog.emit(Instruction::new(Opcode::Move, cand_reg, dest, 0));
                                let after_copy = prog.current_addr();
                                prog.patch_p2(skip_copy, after_copy);
                            }
                            let done_jump = prog.emit(goto_placeholder());
                            let null_addr = prog.current_addr();
                            for j in null_jumps {
                                prog.patch_p2(j, null_addr);
                            }
                            prog.emit(Instruction::new(Opcode::Null, 0, dest, 0));
                            let end_addr = prog.current_addr();
                            prog.patch_p2(done_jump, end_addr);
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
            let arg_base = prog.alloc_registers(expr_list.len().max(1));
            for (i, a) in expr_list.iter().enumerate() {
                compile_expr(prog, a, binding, functions, arg_base + i as i32)?;
            }
            prog.emit(
                Instruction::new(Opcode::Function, arg_base, 0, dest).with_p4(P4::Func {
                    name: name.clone(),
                    argc: expr_list.len(),
                }),
            );
        }
        Expr::Raise { .. } => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "RAISE() may only be used within a trigger-program",
            ));
        }
        Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::InTable { .. }
        | Expr::RowValue(_) => {
            return Err(SqliteError::misuse(
                "subquery must be evaluated by relational engine",
            ));
        }
    }
    Ok(())
}

fn resolve_custom_binary_collation(
    left: &Expr,
    right: &Expr,
    binding: Option<&TableBinding<'_>>,
) -> Option<String> {
    if let Expr::Collate { collation, .. } = left {
        return if Collation::from_name(collation).is_none() {
            Some(collation.clone())
        } else {
            None
        };
    }
    if let Expr::Collate { collation, .. } = right {
        return if Collation::from_name(collation).is_none() {
            Some(collation.clone())
        } else {
            None
        };
    }
    if let Some(b) = binding {
        if let Expr::Column(cref) = left
            && let Some((_, c)) = b.tbl.find_column(&cref.column)
            && let Some(ref cname) = c.custom_collation
        {
            return Some(cname.clone());
        }
        if let Expr::Column(cref) = right
            && let Some((_, c)) = b.tbl.find_column(&cref.column)
            && let Some(ref cname) = c.custom_collation
        {
            return Some(cname.clone());
        }
    }
    None
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
        if let Expr::Column(cref) = left
            && let Some((_, c)) = b.tbl.find_column(&cref.column)
        {
            return c.collation;
        }
        if let Expr::Column(cref) = right
            && let Some((_, c)) = b.tbl.find_column(&cref.column)
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
    tbl_and_schema: Option<(&TableSchema, &str)>,
    alias: Option<&str>,
) -> Option<TypeAffinity> {
    match expr {
        Expr::Collate { expr: inner, .. } => compiler_expr_affinity(inner, tbl_and_schema, alias),
        Expr::Cast { type_name, .. } => Some(TypeAffinity::from_declared_type(type_name)),
        Expr::Column(cref) => {
            let (t, schema_name) = tbl_and_schema?;
            if !cref_matches_single_table(cref, t, schema_name, alias) {
                return None;
            }
            if let Some((_, c)) = t.find_column(&cref.column) {
                Some(c.affinity)
            } else if t.is_implicit_rowid_name(&cref.column) {
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
    compile_for_explain_with_functions(stmt, schema, &FunctionRegistry::default())
}

fn is_pure_count_star_projection(projection: &[ResultColumn]) -> bool {
    if projection.len() != 1 {
        return false;
    }
    match &projection[0] {
        ResultColumn::Expr {
            expr:
                Expr::FunctionCall {
                    name,
                    distinct: false,
                    args,
                    filter: None,
                    over: None,
                },
            ..
        } if name.eq_ignore_ascii_case("count") => match args {
            FunctionArgs::Star => true,
            FunctionArgs::Exprs(es) => es.is_empty(),
        },
        _ => false,
    }
}

fn is_unfiltered_base_table_count_star<F>(sel: &SelectStmt, resolve_table: F) -> bool
where
    F: Fn(&QualifiedName) -> bool,
{
    if !sel.body.compounds.is_empty() {
        return false;
    }
    let SelectCore::Select {
        distinct: false,
        projection,
        from: Some(fc),
        selection: None,
        group_by,
        having: None,
        windows,
    } = &sel.body.head
    else {
        return false;
    };
    if !group_by.is_empty() || !windows.is_empty() || !fc.joins.is_empty() {
        return false;
    }
    if !is_pure_count_star_projection(projection) {
        return false;
    }
    let TableOrSubquery::Table {
        name,
        table_args: None,
        ..
    } = &fc.head
    else {
        return false;
    };
    if name.schema.is_none()
        && sel
            .ctes
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(&name.name))
    {
        return false;
    }
    resolve_table(name)
}

fn expr_contains_multi_in_list(expr: &Expr) -> bool {
    match expr {
        Expr::InList { list, .. } => {
            list.len() >= 2 || list.iter().any(expr_contains_multi_in_list)
        }
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::IsNull { expr: e, .. }
        | Expr::Cast { expr: e, .. } => expr_contains_multi_in_list(e),
        Expr::Binary { left, right, .. } => {
            expr_contains_multi_in_list(left) || expr_contains_multi_in_list(right)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_contains_multi_in_list(e)
                || expr_contains_multi_in_list(low)
                || expr_contains_multi_in_list(high)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_contains_multi_in_list(e)
                || expr_contains_multi_in_list(pattern)
                || escape.as_deref().is_some_and(expr_contains_multi_in_list)
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_some_and(expr_contains_multi_in_list)
                || branches
                    .iter()
                    .any(|(w, t)| expr_contains_multi_in_list(w) || expr_contains_multi_in_list(t))
                || else_expr
                    .as_deref()
                    .is_some_and(expr_contains_multi_in_list)
        }
        Expr::FunctionCall {
            args: FunctionArgs::Exprs(es),
            filter,
            ..
        } => {
            es.iter().any(expr_contains_multi_in_list)
                || filter.as_deref().is_some_and(expr_contains_multi_in_list)
        }
        Expr::RowValue(es) => es.iter().any(expr_contains_multi_in_list),
        _ => false,
    }
}

fn select_contains_multi_in_list(sel: &SelectStmt) -> bool {
    if let SelectCore::Select {
        projection,
        selection,
        having,
        ..
    } = &sel.body.head
    {
        if projection.iter().any(
            |rc| matches!(rc, ResultColumn::Expr { expr, .. } if expr_contains_multi_in_list(expr)),
        ) {
            return true;
        }
        if selection.as_ref().is_some_and(expr_contains_multi_in_list) {
            return true;
        }
        if having.as_ref().is_some_and(expr_contains_multi_in_list) {
            return true;
        }
    }
    false
}

/// Generates a representative [`VdbeProgram`] for `EXPLAIN <stmt>` using connection `functions`.
pub fn compile_for_explain_with_functions(
    stmt: &Stmt,
    schema: &Schema,
    functions: &FunctionRegistry,
) -> Result<VdbeProgram> {
    match stmt {
        Stmt::Select(sel) => {
            if let Some(prog) = try_compile_select_with_functions(sel, schema, functions)? {
                return Ok(prog);
            }
            let is_cnt = is_unfiltered_base_table_count_star(sel, |qn| {
                schema.find_table(&qn.name).is_some()
            });
            let mut prog = VdbeProgram::new();
            prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Transaction, 0, 0, 0));
            if is_cnt {
                prog.emit(Instruction::new(Opcode::OpenRead, 0, 2, 0).with_comment("Count"));
            } else {
                prog.emit(
                    Instruction::new(Opcode::OpenEphemeral, 0, 0, 0)
                        .with_comment("relational query"),
                );
            }
            prog.emit(Instruction::new(Opcode::ResultRow, 1, 1, 0));
            prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
            Ok(prog)
        }
        Stmt::Insert(ins) => {
            let tbl = find_table_qualified_main(&ins.table, schema)?;
            compile_insert_explain_table(&tbl, DatabaseId::MAIN, schema, None, ins, functions)
        }
        Stmt::Update(upd) => {
            let tbl = find_table_qualified_main(&upd.table, schema)?;
            compile_update_explain_table(&tbl, DatabaseId::MAIN)
        }
        Stmt::Delete(del) => {
            let tbl = find_table_qualified_main(&del.table, schema)?;
            compile_delete_explain_table(&tbl, DatabaseId::MAIN)
        }
        _ => {
            let mut prog = VdbeProgram::new();
            prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Transaction, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
            Ok(prog)
        }
    }
}

/// Multi-database variant of [`compile_for_explain_with_functions`].
pub fn compile_for_explain_in_db_set(
    stmt: &Stmt,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
) -> Result<VdbeProgram> {
    match stmt {
        Stmt::Select(sel) => {
            if let Some(prog) = try_compile_select_in_db_set(sel, db_set, functions)? {
                return Ok(prog);
            }
            let is_cnt = is_unfiltered_base_table_count_star(sel, |qn| {
                matches!(
                    db_set.find_relation_in_set(qn),
                    Ok(Some((_, ResolvedRelation::Table(_))))
                )
            });
            let mut prog = VdbeProgram::new();
            prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Transaction, 0, 0, 0));
            if is_cnt {
                prog.emit(Instruction::new(Opcode::OpenRead, 0, 2, 0).with_comment("Count"));
            } else if select_contains_multi_in_list(sel)
                || select_needs_ephemeral_in_db_set(sel, &[], db_set, 0)
            {
                let comment = if select_uses_seek_scan_in_db_set(sel, db_set) {
                    "SeekScan"
                } else {
                    "relational query"
                };
                prog.emit(Instruction::new(Opcode::OpenEphemeral, 0, 0, 0).with_comment(comment));
                if select_has_repeated_from_table_in_db_set(sel) {
                    prog.emit(
                        Instruction::new(Opcode::OpenEphemeral, 1, 0, 0)
                            .with_p4(P4::Str("OpenDup".to_owned()))
                            .with_comment("OpenDup"),
                    );
                }
            } else {
                prog.emit(Instruction::new(Opcode::OpenRead, 0, 2, 0).with_comment("flattened"));
            }
            prog.emit(Instruction::new(Opcode::ResultRow, 1, 1, 0));
            prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
            Ok(prog)
        }
        Stmt::Insert(ins) => match db_set.find_relation_in_set(&ins.table)? {
            Some((db_id, ResolvedRelation::Table(tbl))) => {
                let slot_schema = &db_set.slot(db_id).expect("resolved slot exists").schema;
                let mut prog = compile_insert_explain_table(
                    &tbl,
                    db_id,
                    slot_schema,
                    Some(db_set),
                    ins,
                    functions,
                )?;
                append_matching_triggers_for_explain(
                    &mut prog,
                    db_set,
                    db_id,
                    &tbl.name,
                    false,
                    TriggerEventKind::Insert,
                    None,
                );
                Ok(prog)
            }
            Some((db_id, ResolvedRelation::View(v))) => {
                let mut prog = VdbeProgram::new();
                prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
                prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
                append_matching_triggers_for_explain(
                    &mut prog,
                    db_set,
                    db_id,
                    &v.name,
                    true,
                    TriggerEventKind::Insert,
                    None,
                );
                Ok(prog)
            }
            None => Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", ins.table.display_name()),
            )),
        },
        Stmt::Update(upd) => match db_set.find_relation_in_set(&upd.table)? {
            Some((db_id, ResolvedRelation::Table(tbl))) => {
                let mut prog = compile_update_explain_table(&tbl, db_id)?;
                let mod_cols = resolve_update_explain_modified_cols(Some(&tbl), &upd.assignments);
                let mod_refs: Vec<&str> = mod_cols.iter().map(String::as_str).collect();
                append_matching_triggers_for_explain(
                    &mut prog,
                    db_set,
                    db_id,
                    &tbl.name,
                    false,
                    TriggerEventKind::Update,
                    Some(&mod_refs),
                );
                Ok(prog)
            }
            Some((db_id, ResolvedRelation::View(v))) => {
                let mut prog = VdbeProgram::new();
                prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
                prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
                let mod_cols = resolve_update_explain_modified_cols(None, &upd.assignments);
                let mod_refs: Vec<&str> = mod_cols.iter().map(String::as_str).collect();
                append_matching_triggers_for_explain(
                    &mut prog,
                    db_set,
                    db_id,
                    &v.name,
                    true,
                    TriggerEventKind::Update,
                    Some(&mod_refs),
                );
                Ok(prog)
            }
            None => Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", upd.table.display_name()),
            )),
        },
        Stmt::Delete(del) => match db_set.find_relation_in_set(&del.table)? {
            Some((db_id, ResolvedRelation::Table(tbl))) => {
                let mut prog = compile_delete_explain_table(&tbl, db_id)?;
                append_matching_triggers_for_explain(
                    &mut prog,
                    db_set,
                    db_id,
                    &tbl.name,
                    false,
                    TriggerEventKind::Delete,
                    None,
                );
                Ok(prog)
            }
            Some((db_id, ResolvedRelation::View(v))) => {
                let mut prog = VdbeProgram::new();
                prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
                prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
                append_matching_triggers_for_explain(
                    &mut prog,
                    db_set,
                    db_id,
                    &v.name,
                    true,
                    TriggerEventKind::Delete,
                    None,
                );
                Ok(prog)
            }
            None => Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", del.table.display_name()),
            )),
        },
        _ => {
            let mut prog = VdbeProgram::new();
            prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Transaction, 0, 1, 0));
            prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
            Ok(prog)
        }
    }
}

fn find_table_qualified_main(name: &QualifiedName, schema: &Schema) -> Result<TableSchema> {
    if let Some(s) = &name.schema
        && !s.eq_ignore_ascii_case("main")
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        ));
    }
    if name.schema.is_some() && Schema::is_temp_schema_table_name(&name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        ));
    }
    schema.find_table(&name.name).ok_or_else(|| {
        SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        )
    })
}

fn compile_insert_explain_table(
    tbl: &TableSchema,
    db_id: DatabaseId,
    schema: &Schema,
    db_set: Option<&DatabaseSet>,
    ins: &InsertStmt,
    functions: &FunctionRegistry,
) -> Result<VdbeProgram> {
    let mut prog = VdbeProgram::new();
    prog.read_only = false;
    prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    prog.emit(Instruction::new(Opcode::Transaction, db_id.index() as i32, 1, 0).with_db(db_id));
    let tcur = prog.alloc_cursor();
    prog.emit(
        Instruction::new(
            Opcode::OpenWrite,
            tcur,
            tbl.root_pgno as i32,
            tbl.columns.len() as i32,
        )
        .with_db(db_id)
        .with_comment(tbl.name.clone()),
    );
    let indexes = schema.indexes_for_table(&tbl.name);
    let mut icurs = Vec::new();
    for idx in &indexes {
        let ic = prog.alloc_cursor();
        prog.emit(
            Instruction::new(Opcode::OpenWriteIndex, ic, idx.root_pgno as i32, 0)
                .with_db(db_id)
                .with_p4(P4::KeyInfo(idx.key_sort_orders()))
                .with_comment(idx.name.clone()),
        );
        icurs.push((ic, idx));
    }

    let rid_reg = prog.alloc_register();
    let col_base = prog.alloc_registers(tbl.columns.len());
    let rec_reg = prog.alloc_register();

    if let InsertSource::Values(rows) = &ins.source
        && ins.upsert_clauses.is_empty()
    {
        for row in rows {
            prog.emit(Instruction::new(Opcode::NewRowid, tcur, rid_reg, 0));
            for (i, expr) in row.iter().take(tbl.columns.len()).enumerate() {
                if tbl.rowid_alias_col == Some(i) {
                    if compile_expr(&mut prog, expr, None, functions, rid_reg).is_err() {
                        prog.emit(Instruction::new(Opcode::Null, 0, rid_reg, 0));
                    }
                    prog.emit(Instruction::new(Opcode::Null, 0, col_base + i as i32, 0));
                } else {
                    let dst = col_base + i as i32;
                    if compile_expr(&mut prog, expr, None, functions, dst).is_err() {
                        prog.emit(Instruction::new(Opcode::Null, 0, dst, 0));
                    }
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
        let uses_ephemeral = match (&ins.source, db_set) {
            (InsertSource::Select(sel), Some(ds)) => {
                insert_select_reads_target(sel, &ins.ctes, db_id, &tbl.name, ds, 0)
            }
            _ => false,
        };
        if uses_ephemeral {
            let ecur = prog.alloc_cursor();
            prog.emit(Instruction::new(
                Opcode::OpenEphemeral,
                ecur,
                tbl.columns.len() as i32,
                0,
            ));
        }
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

fn cte_names_shadow(ctes: &[CommonTableExpr], qn: &QualifiedName) -> bool {
    qn.schema.is_none() && ctes.iter().any(|c| c.name.eq_ignore_ascii_case(&qn.name))
}

fn select_core_blocks_flattening(core: &SelectCore) -> bool {
    match core {
        SelectCore::Values(_) => true,
        SelectCore::Select {
            distinct,
            group_by,
            having,
            projection,
            windows,
            ..
        } => {
            *distinct
                || !group_by.is_empty()
                || having.is_some()
                || !windows.is_empty()
                || projection.iter().any(|rc| {
                    matches!(rc, ResultColumn::Expr { expr, .. } if expr_has_agg_for_explain(expr) || expr_contains_window(expr))
                })
        }
    }
}

fn expr_has_agg_for_explain(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall { name, args, .. } => {
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "count" | "sum" | "total" | "avg" | "min" | "max" | "group_concat" | "string_agg"
            ) && (matches!(args, FunctionArgs::Star)
                || matches!(args, FunctionArgs::Exprs(es) if es.len() <= 2 && !(name.eq_ignore_ascii_case("min") || name.eq_ignore_ascii_case("max")) || matches!(args, FunctionArgs::Exprs(es2) if es2.len() == 1)))
            {
                return true;
            }
            if let FunctionArgs::Exprs(es) = args {
                return es.iter().any(expr_has_agg_for_explain);
            }
            false
        }
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::Cast { expr: e, .. } => expr_has_agg_for_explain(e),
        Expr::Binary { left, right, .. } => {
            expr_has_agg_for_explain(left) || expr_has_agg_for_explain(right)
        }
        _ => false,
    }
}

fn in_table_reuses_index(
    qn: &QualifiedName,
    ctes: &[CommonTableExpr],
    db_set: &DatabaseSet,
) -> bool {
    if cte_names_shadow(ctes, qn) {
        return false;
    }
    let Ok(Some((db_id, tbl))) = db_set.find_table_in_set(qn) else {
        return false;
    };
    if tbl.columns.len() != 1 {
        return false;
    }
    if tbl.rowid_alias_col == Some(0) || tbl.columns[0].is_primary_key || tbl.columns[0].is_unique {
        return true;
    }
    db_set.slot(db_id).is_some_and(|sl| {
        sl.schema.indexes_for_table(&tbl.name).iter().any(|idx| {
            idx.where_clause.is_none()
                && idx
                    .columns
                    .first()
                    .is_some_and(|ic| ic.name.eq_ignore_ascii_case(&tbl.columns[0].name))
        })
    })
}

fn expr_needs_ephemeral_in_db_set(
    expr: &Expr,
    ctes: &[CommonTableExpr],
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    if depth > 32 {
        return false;
    }
    match expr {
        Expr::InTable {
            expr: lhs, table, ..
        } => {
            expr_needs_ephemeral_in_db_set(lhs, ctes, db_set, depth + 1)
                || !in_table_reuses_index(table, ctes, db_set)
        }
        Expr::InSubquery {
            expr: lhs,
            subquery,
            ..
        } => {
            expr_needs_ephemeral_in_db_set(lhs, ctes, db_set, depth + 1)
                || !in_subquery_reuses_index(subquery, ctes, db_set)
        }
        Expr::InList {
            expr: lhs, list, ..
        } => {
            expr_needs_ephemeral_in_db_set(lhs, ctes, db_set, depth + 1)
                || list
                    .iter()
                    .any(|e| expr_needs_ephemeral_in_db_set(e, ctes, db_set, depth + 1))
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            select_needs_ephemeral_in_db_set(sub, ctes, db_set, depth + 1)
        }
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::Cast { expr: e, .. }
        | Expr::IsNull { expr: e, .. } => {
            expr_needs_ephemeral_in_db_set(e, ctes, db_set, depth + 1)
        }
        Expr::Binary { left, right, .. } => {
            expr_needs_ephemeral_in_db_set(left, ctes, db_set, depth + 1)
                || expr_needs_ephemeral_in_db_set(right, ctes, db_set, depth + 1)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_needs_ephemeral_in_db_set(e, ctes, db_set, depth + 1)
                || expr_needs_ephemeral_in_db_set(low, ctes, db_set, depth + 1)
                || expr_needs_ephemeral_in_db_set(high, ctes, db_set, depth + 1)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_needs_ephemeral_in_db_set(e, ctes, db_set, depth + 1)
                || expr_needs_ephemeral_in_db_set(pattern, ctes, db_set, depth + 1)
                || escape
                    .as_deref()
                    .is_some_and(|esc| expr_needs_ephemeral_in_db_set(esc, ctes, db_set, depth + 1))
        }
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if over.is_some() {
                return true;
            }
            let args_ephem = match args {
                FunctionArgs::Star => false,
                FunctionArgs::Exprs(es) => es
                    .iter()
                    .any(|e| expr_needs_ephemeral_in_db_set(e, ctes, db_set, depth + 1)),
            };
            args_ephem
                || filter
                    .as_deref()
                    .is_some_and(|f| expr_needs_ephemeral_in_db_set(f, ctes, db_set, depth + 1))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_deref()
                .is_some_and(|o| expr_needs_ephemeral_in_db_set(o, ctes, db_set, depth + 1))
                || branches.iter().any(|(w, t)| {
                    expr_needs_ephemeral_in_db_set(w, ctes, db_set, depth + 1)
                        || expr_needs_ephemeral_in_db_set(t, ctes, db_set, depth + 1)
                })
                || else_expr
                    .as_deref()
                    .is_some_and(|el| expr_needs_ephemeral_in_db_set(el, ctes, db_set, depth + 1))
        }
        Expr::RowValue(es) => es
            .iter()
            .any(|e| expr_needs_ephemeral_in_db_set(e, ctes, db_set, depth + 1)),
        _ => false,
    }
}

fn select_uses_seek_scan_in_db_set(sel: &SelectStmt, db_set: &DatabaseSet) -> bool {
    let SelectCore::Select {
        from: Some(fc),
        selection: Some(w),
        ..
    } = &sel.body.head
    else {
        return false;
    };
    if !select_contains_multi_in_list(sel) || !expr_contains_between(w) {
        return false;
    }
    let TableOrSubquery::Table { name, .. } = &fc.head else {
        return false;
    };
    let Ok(Some((db_id, tbl))) = db_set.find_table_in_set(name) else {
        return false;
    };
    db_set.slot(db_id).is_some_and(|sl| {
        sl.schema
            .indexes_for_table(&tbl.name)
            .iter()
            .any(|idx| idx.stat1.as_ref().is_some_and(|s| s.has_stat1))
    })
}

fn select_has_repeated_from_table_in_db_set(sel: &SelectStmt) -> bool {
    let SelectCore::Select { from: Some(fc), .. } = &sel.body.head else {
        return false;
    };
    if fc.joins.is_empty() {
        return false;
    }
    let mut names: Vec<String> = Vec::with_capacity(1 + fc.joins.len());
    if let TableOrSubquery::Table { name, .. } = &fc.head {
        names.push(name.name.to_ascii_lowercase());
    }
    for j in &fc.joins {
        if let TableOrSubquery::Table { name, .. } = &j.right {
            let lower = name.name.to_ascii_lowercase();
            if names.contains(&lower) {
                return true;
            }
            names.push(lower);
        }
    }
    false
}

fn expr_contains_between(expr: &Expr) -> bool {
    match expr {
        Expr::Between { .. } => true,
        Expr::Binary { left, right, .. } => {
            expr_contains_between(left) || expr_contains_between(right)
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. } => expr_contains_between(inner),
        _ => false,
    }
}

fn in_subquery_reuses_index(
    sub: &SelectStmt,
    ctes: &[CommonTableExpr],
    db_set: &DatabaseSet,
) -> bool {
    if !sub.ctes.is_empty() || !sub.body.compounds.is_empty() || sub.limit.is_some() {
        return false;
    }
    let SelectCore::Select {
        distinct: _,
        projection,
        from: Some(fc),
        selection: None,
        group_by,
        having: None,
        windows,
    } = &sub.body.head
    else {
        return false;
    };
    if !group_by.is_empty() || !windows.is_empty() || !fc.joins.is_empty() || projection.len() != 1
    {
        return false;
    }
    let TableOrSubquery::Table { name, .. } = &fc.head else {
        return false;
    };
    if cte_names_shadow(ctes, name) {
        return false;
    }
    let Ok(Some((db_id, tbl))) = db_set.find_table_in_set(name) else {
        return false;
    };
    let ResultColumn::Expr {
        expr: Expr::Column(cref),
        ..
    } = &projection[0]
    else {
        return false;
    };
    if tbl.is_implicit_rowid_name(&cref.column) {
        return !tbl.without_rowid;
    }
    let Some((col_idx, col)) = tbl.find_column(&cref.column) else {
        return false;
    };
    if tbl.rowid_alias_col == Some(col_idx) || col.is_primary_key || col.is_unique {
        return true;
    }
    db_set.slot(db_id).is_some_and(|sl| {
        sl.schema.indexes_for_table(&tbl.name).iter().any(|idx| {
            idx.where_clause.is_none()
                && idx
                    .columns
                    .first()
                    .is_some_and(|ic| ic.name.eq_ignore_ascii_case(&col.name))
        })
    })
}

fn select_needs_ephemeral_in_db_set(
    sel: &SelectStmt,
    outer_ctes: &[CommonTableExpr],
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    if depth > 32 {
        return false;
    }
    let mut ctes = outer_ctes.to_vec();
    ctes.extend(sel.ctes.iter().cloned());
    for cte in &sel.ctes {
        if cte.materialized == Some(true)
            || !cte.query.body.compounds.is_empty()
            || select_needs_ephemeral_in_db_set(&cte.query, &ctes, db_set, depth + 1)
        {
            return true;
        }
    }
    if sel
        .body
        .compounds
        .iter()
        .any(|(op, _)| *op != CompoundOp::UnionAll)
    {
        return true;
    }
    if select_core_needs_ephemeral(&sel.body.head, &ctes, db_set, depth) {
        return true;
    }
    for (_, core) in &sel.body.compounds {
        if select_core_needs_ephemeral(core, &ctes, db_set, depth) {
            return true;
        }
    }
    sel.order_by
        .iter()
        .any(|ob| expr_needs_ephemeral_in_db_set(&ob.expr, &ctes, db_set, depth + 1))
}

fn select_core_needs_ephemeral(
    core: &SelectCore,
    ctes: &[CommonTableExpr],
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    let SelectCore::Select {
        distinct,
        projection,
        from,
        selection,
        group_by,
        having,
        windows,
    } = core
    else {
        return false;
    };
    if *distinct || !windows.is_empty() {
        return true;
    }
    if projection.iter().any(|rc| {
        matches!(rc, ResultColumn::Expr { expr, .. } if expr_needs_ephemeral_in_db_set(expr, ctes, db_set, depth + 1))
    }) {
        return true;
    }
    if selection
        .as_ref()
        .is_some_and(|w| expr_needs_ephemeral_in_db_set(w, ctes, db_set, depth + 1))
    {
        return true;
    }
    if group_by
        .iter()
        .any(|g| expr_needs_ephemeral_in_db_set(g, ctes, db_set, depth + 1))
    {
        return true;
    }
    if having
        .as_ref()
        .is_some_and(|h| expr_needs_ephemeral_in_db_set(h, ctes, db_set, depth + 1))
    {
        return true;
    }
    let Some(fc) = from else {
        return false;
    };
    let outer_has_joins = !fc.joins.is_empty();
    let outer_blocks_limit_flatten = outer_has_joins || selection.is_some() || !group_by.is_empty();
    if tos_needs_ephemeral_in_from(
        &fc.head,
        outer_has_joins,
        outer_blocks_limit_flatten,
        ctes,
        db_set,
        depth,
    ) {
        return true;
    }
    for j in &fc.joins {
        if tos_needs_ephemeral_in_from(&j.right, true, true, ctes, db_set, depth) {
            return true;
        }
        if let Some(JoinConstraint::On(ref on_e)) = j.constraint
            && expr_needs_ephemeral_in_db_set(on_e, ctes, db_set, depth + 1)
        {
            return true;
        }
    }
    false
}

fn tos_needs_ephemeral_in_from(
    tos: &TableOrSubquery,
    outer_has_joins: bool,
    outer_blocks_limit_flatten: bool,
    ctes: &[CommonTableExpr],
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    match tos {
        TableOrSubquery::Subquery { query, .. } => {
            (outer_has_joins && !query.body.compounds.is_empty())
                || (outer_blocks_limit_flatten && query.limit.is_some())
                || select_core_blocks_flattening(&query.body.head)
                || query
                    .body
                    .compounds
                    .iter()
                    .any(|(_, arm)| select_core_blocks_flattening(arm))
                || select_needs_ephemeral_in_db_set(query, ctes, db_set, depth + 1)
        }
        TableOrSubquery::Table { name, .. } => {
            if cte_names_shadow(ctes, name) {
                return false;
            }
            if db_set.resolve_catalog_table(name).ok().flatten().is_none()
                && let Some((_, view)) = db_set.find_view_in_set(name)
            {
                return (outer_has_joins && !view.select.body.compounds.is_empty())
                    || (outer_blocks_limit_flatten && view.select.limit.is_some())
                    || select_core_blocks_flattening(&view.select.body.head)
                    || view
                        .select
                        .body
                        .compounds
                        .iter()
                        .any(|(_, arm)| select_core_blocks_flattening(arm))
                    || select_needs_ephemeral_in_db_set(&view.select, &[], db_set, depth + 1);
            }
            false
        }
    }
}

fn insert_select_reads_target(
    sel: &SelectStmt,
    outer_ctes: &[CommonTableExpr],
    target_db_id: DatabaseId,
    target_tbl_name: &str,
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    if depth > 32 {
        return true;
    }
    let mut ctes = outer_ctes.to_vec();
    ctes.extend(sel.ctes.iter().cloned());
    for cte in &sel.ctes {
        if insert_select_reads_target(
            &cte.query,
            &ctes,
            target_db_id,
            target_tbl_name,
            db_set,
            depth + 1,
        ) {
            return true;
        }
    }
    if select_core_reads_target(
        &sel.body.head,
        &ctes,
        target_db_id,
        target_tbl_name,
        db_set,
        depth,
    ) {
        return true;
    }
    for (_, core) in &sel.body.compounds {
        if select_core_reads_target(core, &ctes, target_db_id, target_tbl_name, db_set, depth) {
            return true;
        }
    }
    if sel.order_by.iter().any(|ob| {
        expr_reads_target(
            &ob.expr,
            &ctes,
            target_db_id,
            target_tbl_name,
            db_set,
            depth + 1,
        )
    }) {
        return true;
    }
    if let Some(ref lim) = sel.limit {
        if expr_reads_target(
            &lim.limit,
            &ctes,
            target_db_id,
            target_tbl_name,
            db_set,
            depth + 1,
        ) {
            return true;
        }
        if lim.offset.as_ref().is_some_and(|off| {
            expr_reads_target(off, &ctes, target_db_id, target_tbl_name, db_set, depth + 1)
        }) {
            return true;
        }
    }
    false
}

fn select_core_reads_target(
    core: &SelectCore,
    ctes: &[CommonTableExpr],
    target_db_id: DatabaseId,
    target_tbl_name: &str,
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    match core {
        SelectCore::Values(rows) => rows.iter().any(|r| {
            r.iter().any(|e| {
                expr_reads_target(e, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
            })
        }),
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            windows,
            ..
        } => {
            if projection.iter().any(|rc| {
                matches!(rc, ResultColumn::Expr { expr, .. } if expr_reads_target(expr, ctes, target_db_id, target_tbl_name, db_set, depth + 1))
            }) {
                return true;
            }
            if selection.as_ref().is_some_and(|w| {
                expr_reads_target(w, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
            }) {
                return true;
            }
            if group_by.iter().any(|g| {
                expr_reads_target(g, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
            }) {
                return true;
            }
            if having.as_ref().is_some_and(|h| {
                expr_reads_target(h, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
            }) {
                return true;
            }
            if windows.iter().any(|(_, ws)| {
                window_spec_reads_target(ws, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
            }) {
                return true;
            }
            let Some(fc) = from else {
                return false;
            };
            if tos_reads_target(&fc.head, ctes, target_db_id, target_tbl_name, db_set, depth) {
                return true;
            }
            for j in &fc.joins {
                if tos_reads_target(&j.right, ctes, target_db_id, target_tbl_name, db_set, depth) {
                    return true;
                }
                if let Some(JoinConstraint::On(ref on_e)) = j.constraint
                    && expr_reads_target(
                        on_e,
                        ctes,
                        target_db_id,
                        target_tbl_name,
                        db_set,
                        depth + 1,
                    )
                {
                    return true;
                }
            }
            false
        }
    }
}

fn tos_reads_target(
    tos: &TableOrSubquery,
    ctes: &[CommonTableExpr],
    target_db_id: DatabaseId,
    target_tbl_name: &str,
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    match tos {
        TableOrSubquery::Subquery { query, .. } => insert_select_reads_target(
            query,
            ctes,
            target_db_id,
            target_tbl_name,
            db_set,
            depth + 1,
        ),
        TableOrSubquery::Table { name, .. } => {
            if cte_names_shadow(ctes, name) {
                return false;
            }
            if let Ok(Some((db_id, tbl))) = db_set.find_table_in_set(name) {
                return db_id == target_db_id && tbl.name.eq_ignore_ascii_case(target_tbl_name);
            }
            if let Some((_, view)) = db_set.find_view_in_set(name) {
                return insert_select_reads_target(
                    &view.select,
                    &[],
                    target_db_id,
                    target_tbl_name,
                    db_set,
                    depth + 1,
                );
            }
            false
        }
    }
}

fn expr_reads_target(
    expr: &Expr,
    ctes: &[CommonTableExpr],
    target_db_id: DatabaseId,
    target_tbl_name: &str,
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    if depth > 32 {
        return true;
    }
    match expr {
        Expr::InTable {
            expr: lhs, table, ..
        } => {
            if expr_reads_target(lhs, ctes, target_db_id, target_tbl_name, db_set, depth + 1) {
                return true;
            }
            tos_reads_target(
                &TableOrSubquery::Table {
                    name: table.clone(),
                    table_args: None,
                    alias: None,
                    indexed_by: None,
                },
                ctes,
                target_db_id,
                target_tbl_name,
                db_set,
                depth + 1,
            )
        }
        Expr::InSubquery {
            expr: lhs,
            subquery,
            ..
        } => {
            expr_reads_target(lhs, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                || insert_select_reads_target(
                    subquery,
                    ctes,
                    target_db_id,
                    target_tbl_name,
                    db_set,
                    depth + 1,
                )
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            insert_select_reads_target(sub, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
        }
        Expr::InList {
            expr: lhs, list, ..
        } => {
            expr_reads_target(lhs, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                || list.iter().any(|e| {
                    expr_reads_target(e, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                })
        }
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::Cast { expr: e, .. }
        | Expr::IsNull { expr: e, .. } => {
            expr_reads_target(e, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
        }
        Expr::Binary { left, right, .. } => {
            expr_reads_target(left, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                || expr_reads_target(
                    right,
                    ctes,
                    target_db_id,
                    target_tbl_name,
                    db_set,
                    depth + 1,
                )
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_reads_target(e, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                || expr_reads_target(low, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                || expr_reads_target(high, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_reads_target(e, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                || expr_reads_target(
                    pattern,
                    ctes,
                    target_db_id,
                    target_tbl_name,
                    db_set,
                    depth + 1,
                )
                || escape.as_deref().is_some_and(|esc| {
                    expr_reads_target(esc, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                })
        }
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            let args_r = match args {
                FunctionArgs::Star => false,
                FunctionArgs::Exprs(es) => es.iter().any(|e| {
                    expr_reads_target(e, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                }),
            };
            args_r
                || filter.as_deref().is_some_and(|f| {
                    expr_reads_target(f, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                })
                || matches!(over, Some(WindowRef::Spec(ws)) if window_spec_reads_target(ws, ctes, target_db_id, target_tbl_name, db_set, depth + 1))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_some_and(|o| {
                expr_reads_target(o, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
            }) || branches.iter().any(|(w, t)| {
                expr_reads_target(w, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
                    || expr_reads_target(t, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
            }) || else_expr.as_deref().is_some_and(|el| {
                expr_reads_target(el, ctes, target_db_id, target_tbl_name, db_set, depth + 1)
            })
        }
        Expr::RowValue(es) => es
            .iter()
            .any(|e| expr_reads_target(e, ctes, target_db_id, target_tbl_name, db_set, depth + 1)),
        _ => false,
    }
}

fn compile_update_explain_table(tbl: &TableSchema, db_id: DatabaseId) -> Result<VdbeProgram> {
    let mut prog = VdbeProgram::new();
    prog.read_only = false;
    prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    prog.emit(Instruction::new(Opcode::Transaction, db_id.index() as i32, 1, 0).with_db(db_id));
    let tcur = prog.alloc_cursor();
    prog.emit(
        Instruction::new(
            Opcode::OpenWrite,
            tcur,
            tbl.root_pgno as i32,
            tbl.columns.len() as i32,
        )
        .with_db(db_id)
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

fn compile_delete_explain_table(tbl: &TableSchema, db_id: DatabaseId) -> Result<VdbeProgram> {
    let mut prog = VdbeProgram::new();
    prog.read_only = false;
    prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    prog.emit(Instruction::new(Opcode::Transaction, db_id.index() as i32, 1, 0).with_db(db_id));
    let tcur = prog.alloc_cursor();
    prog.emit(
        Instruction::new(
            Opcode::OpenWrite,
            tcur,
            tbl.root_pgno as i32,
            tbl.columns.len() as i32,
        )
        .with_db(db_id)
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

fn resolve_update_explain_modified_cols(
    tbl_opt: Option<&TableSchema>,
    assigns: &[SetAssignment],
) -> Vec<String> {
    let mut out = Vec::new();
    let push_col = |raw: &str, out: &mut Vec<String>| {
        if let Some(tbl) = tbl_opt {
            if let Some(col) = tbl
                .columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(raw))
            {
                out.push(col.name.clone());
            } else if !tbl.without_rowid
                && matches!(
                    raw.to_ascii_lowercase().as_str(),
                    "rowid" | "_rowid_" | "oid"
                )
                && let Some(ipk) = tbl.rowid_alias_col
            {
                out.push(tbl.columns[ipk].name.clone());
            }
        } else {
            out.push(raw.to_owned());
        }
    };
    for sa in assigns {
        for c in &sa.columns {
            push_col(c, &mut out);
        }
    }
    out
}

fn append_matching_triggers_for_explain(
    prog: &mut VdbeProgram,
    db_set: &DatabaseSet,
    db_id: DatabaseId,
    target_name: &str,
    is_view: bool,
    event_kind: TriggerEventKind,
    mod_cols: Option<&[&str]>,
) {
    let timings: &[TriggerTiming] = if is_view {
        &[TriggerTiming::InsteadOf]
    } else {
        &[TriggerTiming::Before, TriggerTiming::After]
    };
    for &timing in timings {
        let matched = db_set.matching_triggers(db_id, target_name, timing, event_kind, mod_cols);
        for (_, _, trig) in matched {
            prog.emit(
                Instruction::new(Opcode::Init, 0, 1, 0)
                    .with_p4(P4::Str(format!("-- TRIGGER {}", trig.name))),
            );
            if let Some(ref w) = trig.when_clause {
                emit_explain_expr_literals(prog, w);
            }
            for step in &trig.body {
                match step {
                    TriggerStep::Select(sel) => emit_explain_select_literals(prog, sel),
                    TriggerStep::Insert(ins) => {
                        for cte in &ins.ctes {
                            emit_explain_select_literals(prog, &cte.query);
                        }
                        match &ins.source {
                            InsertSource::DefaultValues => {}
                            InsertSource::Values(rows) => {
                                for r in rows {
                                    for e in r {
                                        emit_explain_expr_literals(prog, e);
                                    }
                                }
                            }
                            InsertSource::Select(sel) => emit_explain_select_literals(prog, sel),
                        }
                        for uc in &ins.upsert_clauses {
                            if let Some(ref ut) = uc.target
                                && let Some(ref tw) = ut.where_clause
                            {
                                emit_explain_expr_literals(prog, tw);
                            }
                            if let UpsertAction::DoUpdate {
                                ref assignments,
                                ref where_clause,
                            } = uc.action
                            {
                                for sa in assignments {
                                    emit_explain_expr_literals(prog, &sa.expr);
                                }
                                if let Some(dw) = where_clause {
                                    emit_explain_expr_literals(prog, dw);
                                }
                            }
                        }
                    }
                    TriggerStep::Update(upd) => {
                        for cte in &upd.ctes {
                            emit_explain_select_literals(prog, &cte.query);
                        }
                        for sa in &upd.assignments {
                            emit_explain_expr_literals(prog, &sa.expr);
                        }
                        if let Some(ref w) = upd.selection {
                            emit_explain_expr_literals(prog, w);
                        }
                    }
                    TriggerStep::Delete(del) => {
                        for cte in &del.ctes {
                            emit_explain_select_literals(prog, &cte.query);
                        }
                        if let Some(ref w) = del.selection {
                            emit_explain_expr_literals(prog, w);
                        }
                    }
                }
            }
            prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
        }
    }
}

fn emit_explain_select_literals(prog: &mut VdbeProgram, sel: &SelectStmt) {
    for cte in &sel.ctes {
        emit_explain_select_literals(prog, &cte.query);
    }
    emit_explain_select_core_literals(prog, &sel.body.head);
    for (_, rhs) in &sel.body.compounds {
        emit_explain_select_core_literals(prog, rhs);
    }
}

fn emit_explain_select_core_literals(prog: &mut VdbeProgram, core: &SelectCore) {
    match core {
        SelectCore::Values(rows) => {
            for r in rows {
                for e in r {
                    emit_explain_expr_literals(prog, e);
                }
            }
        }
        SelectCore::Select {
            projection,
            selection,
            group_by,
            having,
            windows,
            ..
        } => {
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    emit_explain_expr_literals(prog, expr);
                }
            }
            if let Some(w) = selection {
                emit_explain_expr_literals(prog, w);
            }
            for g in group_by {
                emit_explain_expr_literals(prog, g);
            }
            if let Some(h) = having {
                emit_explain_expr_literals(prog, h);
            }
            for (_, ws) in windows {
                emit_explain_window_spec_literals(prog, ws);
            }
        }
    }
}

fn emit_explain_expr_literals(prog: &mut VdbeProgram, expr: &Expr) {
    match expr {
        Expr::String(s) => {
            prog.emit(Instruction::new(Opcode::String8, 0, 1, 0).with_p4(P4::Str(s.clone())));
        }
        Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => emit_explain_expr_literals(prog, inner),
        Expr::Binary { left, right, .. } => {
            emit_explain_expr_literals(prog, left);
            emit_explain_expr_literals(prog, right);
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            emit_explain_expr_literals(prog, e);
            emit_explain_expr_literals(prog, low);
            emit_explain_expr_literals(prog, high);
        }
        Expr::InList { expr: e, list, .. } => {
            emit_explain_expr_literals(prog, e);
            for item in list {
                emit_explain_expr_literals(prog, item);
            }
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            emit_explain_expr_literals(prog, e);
            emit_explain_select_literals(prog, subquery);
        }
        Expr::InTable { expr: e, .. } => emit_explain_expr_literals(prog, e),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            emit_explain_expr_literals(prog, e);
            emit_explain_expr_literals(prog, pattern);
            if let Some(esc) = escape {
                emit_explain_expr_literals(prog, esc);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                emit_explain_expr_literals(prog, op);
            }
            for (c, v) in branches {
                emit_explain_expr_literals(prog, c);
                emit_explain_expr_literals(prog, v);
            }
            if let Some(el) = else_expr {
                emit_explain_expr_literals(prog, el);
            }
        }
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    emit_explain_expr_literals(prog, e);
                }
            }
            if let Some(f) = filter {
                emit_explain_expr_literals(prog, f);
            }
            if let Some(WindowRef::Spec(ws)) = over {
                emit_explain_window_spec_literals(prog, ws);
            }
        }
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            emit_explain_select_literals(prog, subquery);
        }
        Expr::Raise {
            message: Some(m), ..
        } => emit_explain_expr_literals(prog, m),
        Expr::RowValue(es) => {
            for e in es {
                emit_explain_expr_literals(prog, e);
            }
        }
        _ => {}
    }
}

fn window_spec_reads_target(
    ws: &WindowSpec,
    ctes: &[CommonTableExpr],
    target_db_id: DatabaseId,
    target_tbl_name: &str,
    db_set: &DatabaseSet,
    depth: usize,
) -> bool {
    ws.partition_by
        .iter()
        .any(|pe| expr_reads_target(pe, ctes, target_db_id, target_tbl_name, db_set, depth + 1))
        || ws.order_by.iter().any(|ob| {
            expr_reads_target(
                &ob.expr,
                ctes,
                target_db_id,
                target_tbl_name,
                db_set,
                depth + 1,
            )
        })
}

fn emit_explain_window_spec_literals(prog: &mut VdbeProgram, ws: &WindowSpec) {
    for pe in &ws.partition_by {
        emit_explain_expr_literals(prog, pe);
    }
    for ob in &ws.order_by {
        emit_explain_expr_literals(prog, &ob.expr);
    }
    if let Some(ref wf) = ws.frame {
        for b in [&wf.start, &wf.end] {
            if let WindowFrameBound::Preceding(e) | WindowFrameBound::Following(e) = b
                && is_window_bound_constant(e)
            {
                emit_explain_expr_literals(prog, e);
            }
        }
    }
}

/// Generates 4-column `EXPLAIN QUERY PLAN <stmt>` rows: `(id, parent, notused, detail)`.
pub fn explain_query_plan(stmt: &Stmt, schema: &Schema) -> Result<Vec<Vec<Value>>> {
    explain_query_plan_core(stmt, |name| {
        if name
            .schema
            .as_deref()
            .is_none_or(|s| s.eq_ignore_ascii_case("main"))
        {
            if let Some(tbl) = schema.find_table(&name.name) {
                return Some((
                    DatabaseId::MAIN,
                    "main",
                    schema,
                    ResolvedRelation::Table(Box::new(tbl)),
                ));
            }
            if let Some(view) = schema.find_view(&name.name) {
                return Some((
                    DatabaseId::MAIN,
                    "main",
                    schema,
                    ResolvedRelation::View(Box::new(view.clone())),
                ));
            }
        }
        None
    })
}

/// Multi-database variant of [`explain_query_plan`].
pub fn explain_query_plan_in_db_set(stmt: &Stmt, db_set: &DatabaseSet) -> Result<Vec<Vec<Value>>> {
    explain_query_plan_core(stmt, |name| {
        let (db_id, rel) = db_set.find_relation_in_set(name).ok().flatten()?;
        let slot = db_set.slot(db_id)?;
        Some((db_id, slot.name.as_str(), &slot.schema, rel))
    })
}

fn plan_satisfies_order_by(
    plan: &AccessPlan,
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    selection: Option<&Expr>,
    order_by: &[OrderByTerm],
) -> bool {
    if order_by.is_empty() {
        return true;
    }
    match plan {
        AccessPlan::FullScan | AccessPlan::RowidSeek { .. } | AccessPlan::RowidRange { .. } => {
            if !tbl.without_rowid {
                order_by.len() == 1
                    && is_table_rowid_column_expr(&order_by[0].expr, tbl, schema_name, alias)
            } else if let Some(pk_idx) = tbl.without_rowid_pk_as_index() {
                index_satisfies_order_by(&pk_idx, tbl, schema_name, alias, selection, order_by)
            } else {
                false
            }
        }
        AccessPlan::IndexFullScan { index, .. }
        | AccessPlan::IndexSeek { index, .. }
        | AccessPlan::IndexRangeScan { index, .. } => {
            index_satisfies_order_by(index, tbl, schema_name, alias, selection, order_by)
        }
        AccessPlan::RowidIn { .. } | AccessPlan::MultiIndexOr { .. } => false,
    }
}

fn plan_cost_and_rows(plan: &AccessPlan, tbl: &TableSchema, schema: &Schema) -> (i16, i16) {
    match plan {
        AccessPlan::FullScan => {
            let r_out = tbl.stat1.n_row_log_est;
            (r_out, r_out)
        }
        AccessPlan::IndexFullScan { index, is_covering } => {
            let r_out = index.effective_row_log_est(tbl)[0];
            let cost = if *is_covering {
                (r_out - 5).max(0)
            } else {
                r_out
            };
            (cost, r_out)
        }
        AccessPlan::RowidSeek { .. } => (10, 0),
        AccessPlan::RowidIn { list } => {
            let r_out = estimate_in_expr_list_log(list, schema);
            (r_out.saturating_add(10), r_out)
        }
        AccessPlan::RowidRange { lower, upper } => {
            let mut r_out = tbl.stat1.n_row_log_est;
            if lower.is_some() {
                r_out = (r_out - 10).max(0);
            }
            if upper.is_some() {
                r_out = (r_out - 10).max(0);
            }
            (r_out.saturating_add(15), r_out)
        }
        AccessPlan::IndexSeek {
            index,
            prefix_exprs,
            is_covering,
        } => {
            let idx_est = index.effective_row_log_est(tbl);
            let n_eq = prefix_exprs.len();
            let r_out = if index.unique && n_eq >= index.columns.len() {
                0
            } else {
                idx_est
                    .get(n_eq.min(idx_est.len().saturating_sub(1)))
                    .copied()
                    .unwrap_or(0)
            };
            let mut cost = sqlite_log_est_add(10, r_out);
            if !*is_covering {
                cost = sqlite_log_est_add(cost, r_out.saturating_add(15));
            }
            (cost, r_out)
        }
        AccessPlan::IndexRangeScan {
            index,
            prefix_exprs,
            prefix_in_lists,
            range_col_idx,
            lower,
            upper,
            is_covering,
            skip_scan_cols,
        } => {
            let idx_est = index.effective_row_log_est(tbl);
            let n_eq = skip_scan_cols + prefix_exprs.len() + prefix_in_lists.len();
            let mut r_size = idx_est
                .get(n_eq.min(idx_est.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0);
            if range_col_idx.is_some() {
                if lower.is_some() {
                    r_size = (r_size - 10).max(0);
                }
                if upper.is_some() {
                    r_size = (r_size - 10).max(0);
                }
            }
            let mut n_seeks_log: i16 = 0;
            for (_, list) in prefix_in_lists {
                n_seeks_log = n_seeks_log.saturating_add(estimate_in_expr_list_log(list, schema));
            }
            if *skip_scan_cols > 0 {
                let n_distinct_skip =
                    (idx_est[0] - idx_est.get(*skip_scan_cols).copied().unwrap_or(0)).max(10);
                n_seeks_log = n_seeks_log.saturating_add(n_distinct_skip);
            }
            let r_out = r_size.saturating_add(n_seeks_log);
            let mut cost = sqlite_log_est_add(n_seeks_log.saturating_add(10), r_out);
            if !*is_covering {
                cost = sqlite_log_est_add(cost, r_out.saturating_add(15));
            }
            (cost, r_out)
        }
        AccessPlan::MultiIndexOr { branches } => {
            let mut total_c = 0i16;
            let mut total_r = 0i16;
            for bp in branches {
                let (bc, br) = plan_cost_and_rows(bp, tbl, schema);
                total_c = sqlite_log_est_add(total_c, bc);
                total_r = sqlite_log_est_add(total_r, br);
            }
            (total_c, total_r)
        }
    }
}

fn emit_in_table_eqp_nodes<'a, F>(
    expr_opt: Option<&Expr>,
    parent: i64,
    resolve_relation: &F,
    push_node: &mut dyn FnMut(i64, String) -> i64,
) where
    F: Fn(&QualifiedName) -> Option<(DatabaseId, &'a str, &'a Schema, ResolvedRelation)>,
{
    let Some(expr) = expr_opt else {
        return;
    };
    match expr {
        Expr::InTable {
            expr: lhs,
            table,
            negated: false,
        } => {
            emit_in_table_eqp_nodes(Some(lhs), parent, resolve_relation, push_node);
            if let Some((_, _, _, ResolvedRelation::Table(rhs_tbl))) = resolve_relation(table)
                && rhs_tbl.columns.len() == 1
                && rhs_tbl.rowid_alias_col == Some(0)
            {
                let _ = push_node(
                    parent,
                    format!(
                        "USING ROWID SEARCH ON TABLE {} FOR IN-OPERATOR",
                        rhs_tbl.name
                    ),
                );
            }
        }
        Expr::Binary { left, right, .. } => {
            emit_in_table_eqp_nodes(Some(left), parent, resolve_relation, push_node);
            emit_in_table_eqp_nodes(Some(right), parent, resolve_relation, push_node);
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. } => {
            emit_in_table_eqp_nodes(Some(inner), parent, resolve_relation, push_node);
        }
        _ => {}
    }
}

fn validate_index_hint_in_eqp(
    slot_schema: &Schema,
    tbl: &TableSchema,
    sname: &str,
    alias: Option<&str>,
    indexed_by: Option<&IndexHint>,
    selection: Option<&Expr>,
) -> Result<()> {
    if let Some(IndexHint::IndexedBy(idx_name)) = indexed_by {
        let Some(idx) = slot_schema
            .indexes_for_table(&tbl.name)
            .into_iter()
            .find(|i| !i.is_autoindex && i.name.eq_ignore_ascii_case(idx_name))
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such index: {idx_name}"),
            ));
        };
        if let Some(ref pw) = idx.where_clause {
            let usable =
                selection.is_some_and(|wh| expr_implies_partial_where(wh, pw, tbl, sname, alias));
            if !usable {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "no query solution".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn substitute_view_columns_in_expr(
    expr: &Expr,
    view_alias: &str,
    view_col_map: &[(String, Expr)],
) -> Expr {
    match expr {
        Expr::Column(cref) => {
            if cref
                .table
                .as_deref()
                .is_none_or(|t| t.eq_ignore_ascii_case(view_alias))
                && let Some((_, mapped)) = view_col_map
                    .iter()
                    .find(|(vcol, _)| vcol.eq_ignore_ascii_case(&cref.column))
            {
                return mapped.clone();
            }
            expr.clone()
        }
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(substitute_view_columns_in_expr(
                inner,
                view_alias,
                view_col_map,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(substitute_view_columns_in_expr(
                left,
                view_alias,
                view_col_map,
            )),
            op: *op,
            right: Box::new(substitute_view_columns_in_expr(
                right,
                view_alias,
                view_col_map,
            )),
        },
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(substitute_view_columns_in_expr(
                inner,
                view_alias,
                view_col_map,
            )),
            collation: collation.clone(),
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(substitute_view_columns_in_expr(
                inner,
                view_alias,
                view_col_map,
            )),
            negated: *negated,
        },
        Expr::Cast {
            expr: inner,
            type_name,
        } => Expr::Cast {
            expr: Box::new(substitute_view_columns_in_expr(
                inner,
                view_alias,
                view_col_map,
            )),
            type_name: type_name.clone(),
        },
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(substitute_view_columns_in_expr(e, view_alias, view_col_map)),
            low: Box::new(substitute_view_columns_in_expr(
                low,
                view_alias,
                view_col_map,
            )),
            high: Box::new(substitute_view_columns_in_expr(
                high,
                view_alias,
                view_col_map,
            )),
            negated: *negated,
        },
        Expr::InList {
            expr: e,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(substitute_view_columns_in_expr(e, view_alias, view_col_map)),
            list: list
                .iter()
                .map(|i| substitute_view_columns_in_expr(i, view_alias, view_col_map))
                .collect(),
            negated: *negated,
        },
        Expr::FunctionCall {
            name,
            distinct,
            args: FunctionArgs::Exprs(es),
            filter,
            over,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: FunctionArgs::Exprs(
                es.iter()
                    .map(|e| substitute_view_columns_in_expr(e, view_alias, view_col_map))
                    .collect(),
            ),
            filter: filter
                .as_ref()
                .map(|f| Box::new(substitute_view_columns_in_expr(f, view_alias, view_col_map))),
            over: over.clone(),
        },
        _ => expr.clone(),
    }
}

fn rewrite_join_expr_with_bound_tables(
    expr: &Expr,
    current_tbl: &TableSchema,
    outer_table_names: &[String],
) -> Expr {
    match expr {
        Expr::Column(cref) => {
            if let Some(ref t) = cref.table {
                if outer_table_names
                    .iter()
                    .any(|ot| ot.eq_ignore_ascii_case(t))
                {
                    // Outer join loop column acts as a bound parameter for the inner loop!
                    return Expr::Bind(BindParam::Anonymous(1));
                }
            } else if !outer_table_names.is_empty()
                && current_tbl.find_column(&cref.column).is_none()
                && !current_tbl.is_implicit_rowid_name(&cref.column)
            {
                // Unqualified column that does not belong to `current_tbl` comes from an outer loop!
                return Expr::Bind(BindParam::Anonymous(1));
            }
            expr.clone()
        }
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(rewrite_join_expr_with_bound_tables(
                inner,
                current_tbl,
                outer_table_names,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(rewrite_join_expr_with_bound_tables(
                left,
                current_tbl,
                outer_table_names,
            )),
            op: *op,
            right: Box::new(rewrite_join_expr_with_bound_tables(
                right,
                current_tbl,
                outer_table_names,
            )),
        },
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(rewrite_join_expr_with_bound_tables(
                inner,
                current_tbl,
                outer_table_names,
            )),
            collation: collation.clone(),
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(rewrite_join_expr_with_bound_tables(
                inner,
                current_tbl,
                outer_table_names,
            )),
            negated: *negated,
        },
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(rewrite_join_expr_with_bound_tables(
                e,
                current_tbl,
                outer_table_names,
            )),
            low: Box::new(rewrite_join_expr_with_bound_tables(
                low,
                current_tbl,
                outer_table_names,
            )),
            high: Box::new(rewrite_join_expr_with_bound_tables(
                high,
                current_tbl,
                outer_table_names,
            )),
            negated: *negated,
        },
        Expr::InList {
            expr: e,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(rewrite_join_expr_with_bound_tables(
                e,
                current_tbl,
                outer_table_names,
            )),
            list: list
                .iter()
                .map(|i| rewrite_join_expr_with_bound_tables(i, current_tbl, outer_table_names))
                .collect(),
            negated: *negated,
        },
        Expr::FunctionCall {
            name,
            distinct,
            args: FunctionArgs::Exprs(es),
            filter,
            over,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: FunctionArgs::Exprs(
                es.iter()
                    .map(|e| rewrite_join_expr_with_bound_tables(e, current_tbl, outer_table_names))
                    .collect(),
            ),
            filter: filter.as_ref().map(|f| {
                Box::new(rewrite_join_expr_with_bound_tables(
                    f,
                    current_tbl,
                    outer_table_names,
                ))
            }),
            over: over.clone(),
        },
        _ => expr.clone(),
    }
}

fn explain_query_plan_core<'a, F>(stmt: &Stmt, resolve_relation: F) -> Result<Vec<Vec<Value>>>
where
    F: Fn(&QualifiedName) -> Option<(DatabaseId, &'a str, &'a Schema, ResolvedRelation)>,
{
    let mut rows = Vec::new();
    let mut next_id = 2i64;
    let mut push_node = |parent: i64, detail: String| -> i64 {
        let id = next_id;
        rows.push(vec![
            Value::Integer(id),
            Value::Integer(parent),
            Value::Integer(0),
            Value::Text(detail),
        ]);
        next_id += 1;
        id
    };

    fn emit_plan_nodes(
        parent: i64,
        plan: &AccessPlan,
        disp: &str,
        suffix: &str,
        push_node: &mut dyn FnMut(i64, String) -> i64,
    ) {
        match plan {
            AccessPlan::MultiIndexOr { branches } => {
                let or_id = push_node(parent, format!("MULTI-INDEX OR{suffix}"));
                for (i, bp) in branches.iter().enumerate() {
                    let idx_node = push_node(or_id, format!("INDEX {}", i + 1));
                    let _ = push_node(idx_node, bp.explain_detail(disp));
                }
            }
            _ => {
                let mut det = plan.explain_detail(disp);
                if !suffix.is_empty() {
                    det.push_str(suffix);
                }
                let _ = push_node(parent, det);
            }
        }
    }

    fn explain_select_plan<'a, F>(
        sel: &SelectStmt,
        parent: i64,
        resolve_relation: &F,
        active_views: &mut Vec<(DatabaseId, String)>,
        push_node: &mut dyn FnMut(i64, String) -> i64,
    ) -> Result<()>
    where
        F: Fn(&QualifiedName) -> Option<(DatabaseId, &'a str, &'a Schema, ResolvedRelation)>,
    {
        let explain_view = |db_id: DatabaseId,
                            sname: &str,
                            view: &ViewSchema,
                            view_alias: Option<&str>,
                            outer_selection: Option<&Expr>,
                            parent_id: i64,
                            active_views: &mut Vec<(DatabaseId, String)>,
                            push_node: &mut dyn FnMut(i64, String) -> i64|
         -> Result<()> {
            let key = (db_id, view.name.to_ascii_lowercase());
            if active_views.contains(&key) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("view {} is circularly defined", view.name),
                ));
            }
            let mut fixed = if db_id != DatabaseId::TEMP {
                fix_permanent_view_select(&view.name, sname, &view.select)?
            } else {
                view.select.clone()
            };
            // If the view is a simple single-table SELECT and `outer_selection` is present,
            // push `outer_selection` into the view's `WHERE` clause (query flattening!).
            if let Some(outer_w) = outer_selection
                && fixed.body.compounds.is_empty()
                && let SelectCore::Select {
                    distinct: false,
                    ref projection,
                    from: Some(ref v_fc),
                    ref mut selection,
                    ref group_by,
                    having: None,
                    ref windows,
                } = fixed.body.head
                && group_by.is_empty()
                && windows.is_empty()
                && v_fc.joins.is_empty()
                && let TableOrSubquery::Table {
                    name: ref inner_tname,
                    ..
                } = v_fc.head
                && let Some((_, _, _, ResolvedRelation::Table(inner_tbl))) =
                    resolve_relation(inner_tname)
            {
                let mut col_map: Vec<(String, Expr)> = Vec::new();
                for (idx, rc) in projection.iter().enumerate() {
                    match rc {
                        ResultColumn::Star | ResultColumn::TableStar { .. } => {
                            for c in &inner_tbl.columns {
                                col_map.push((
                                    c.name.clone(),
                                    Expr::Column(ColumnRef::unqualified(c.name.clone())),
                                ));
                            }
                        }
                        ResultColumn::Expr { expr, alias, .. } => {
                            let cname = if let Some(ref cols) = view.columns
                                && idx < cols.len()
                            {
                                cols[idx].clone()
                            } else if let Some(al) = alias {
                                al.clone()
                            } else if let Expr::Column(cref) = expr {
                                cref.column.clone()
                            } else {
                                expr_column_label(expr)
                            };
                            col_map.push((cname, expr.clone()));
                        }
                    }
                }
                let mapped_w = substitute_view_columns_in_expr(
                    outer_w,
                    view_alias.unwrap_or(&view.name),
                    &col_map,
                );
                *selection = Some(match selection.take() {
                    Some(prev) => Expr::Binary {
                        left: Box::new(prev),
                        op: BinaryOp::And,
                        right: Box::new(mapped_w),
                    },
                    None => mapped_w,
                });
            }
            active_views.push(key);
            let res =
                explain_select_plan(&fixed, parent_id, resolve_relation, active_views, push_node);
            active_views.pop();
            res
        };

        let is_cte = |qn: &QualifiedName| {
            qn.schema.is_none()
                && sel
                    .ctes
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(&qn.name))
        };

        let resolve_cte_target_name = |qn: &QualifiedName| -> Option<String> {
            if qn.schema.is_some() {
                return None;
            }
            let c = sel
                .ctes
                .iter()
                .rev()
                .find(|c| c.name.eq_ignore_ascii_case(&qn.name))?;
            if c.query.body.compounds.is_empty()
                && c.query.order_by.is_empty()
                && c.query.limit.is_none()
                && let SelectCore::Select {
                    distinct: false,
                    ref projection,
                    from: Some(ref c_fc),
                    selection: None,
                    ref group_by,
                    having: None,
                    ref windows,
                } = c.query.body.head
                && group_by.is_empty()
                && windows.is_empty()
                && c_fc.joins.is_empty()
                && matches!(projection.as_slice(), [ResultColumn::Star])
                && let TableOrSubquery::Table {
                    name: ref inner_qn, ..
                } = c_fc.head
                && inner_qn.schema.is_none()
                && sel
                    .ctes
                    .iter()
                    .any(|base| base.name.eq_ignore_ascii_case(&inner_qn.name))
            {
                return Some(inner_qn.name.clone());
            }
            Some(c.name.clone())
        };

        if let SelectCore::Select {
            projection,
            from: Some(fc),
            selection,
            group_by,
            having,
            ..
        } = &sel.body.head
        {
            if fc.joins.is_empty()
                && let TableOrSubquery::Subquery { query, alias } = &fc.head
            {
                if query.body.compounds.is_empty()
                    && query.order_by.is_empty()
                    && query.limit.is_none()
                    && !crate::window::core_has_regular_aggregates(
                        &query.body.head,
                        &query.order_by,
                    )
                    && !core_has_window(&query.body.head, &query.order_by)
                {
                    let mut merged_q = (**query).clone();
                    let mut combined_ctes = sel.ctes.clone();
                    combined_ctes.extend(merged_q.ctes);
                    merged_q.ctes = combined_ctes;
                    return explain_select_plan(
                        &merged_q,
                        parent,
                        resolve_relation,
                        active_views,
                        push_node,
                    );
                } else if !query.order_by.is_empty() {
                    let sub_label = alias.as_deref().unwrap_or("(subquery-xxxxxx)");
                    let co_id = push_node(parent, format!("CO-ROUTINE {sub_label}"));
                    explain_select_plan(query, co_id, resolve_relation, active_views, push_node)?;
                    let _ = push_node(parent, format!("SCAN {sub_label}"));
                    return Ok(());
                }
            }

            if !sel.ctes.is_empty() {
                let mut ref_counts: Vec<(String, usize)> = Vec::new();
                let mut count_tos = |tos: &TableOrSubquery| {
                    if let TableOrSubquery::Table { name, .. } = tos
                        && let Some(canon) = resolve_cte_target_name(name)
                    {
                        if let Some((_, cnt)) = ref_counts
                            .iter_mut()
                            .find(|(n, _)| n.eq_ignore_ascii_case(&canon))
                        {
                            *cnt += 1;
                        } else {
                            ref_counts.push((canon, 1));
                        }
                    }
                };
                count_tos(&fc.head);
                for j in &fc.joins {
                    count_tos(&j.right);
                }
                for (canon, cnt) in &ref_counts {
                    if *cnt >= 2
                        && let Some(cte_def) =
                            sel.ctes.iter().find(|c| c.name.eq_ignore_ascii_case(canon))
                    {
                        let mat_id = push_node(parent, format!("MATERIALIZE {}", cte_def.name));
                        explain_select_plan(
                            &cte_def.query,
                            mat_id,
                            resolve_relation,
                            active_views,
                            push_node,
                        )?;
                    }
                }
            }
            let window_sort_groups = crate::window::collect_window_sort_groups(sel);
            let effective_order_by: &[OrderByTerm] =
                if let Some(first_wg) = window_sort_groups.first() {
                    first_wg.as_slice()
                } else {
                    &sel.order_by
                };
            let mut order_by_satisfied = false;
            if fc.joins.is_empty()
                && let TableOrSubquery::Table {
                    name,
                    table_args: None,
                    alias,
                    indexed_by,
                } = &fc.head
                && !is_cte(name)
                && let Some((db_id, sname, slot_schema, rel)) = resolve_relation(name)
            {
                match rel {
                    ResolvedRelation::Table(tbl) => {
                        validate_index_hint_in_eqp(
                            slot_schema,
                            &tbl,
                            sname,
                            alias.as_deref(),
                            indexed_by.as_ref(),
                            selection.as_ref(),
                        )?;
                        let plan = choose_access_plan_with_context(
                            &tbl,
                            sname,
                            alias.as_deref(),
                            selection.as_ref(),
                            slot_schema,
                            QueryPlanContext {
                                projection: Some(projection),
                                group_by,
                                having: having.as_ref(),
                                order_by: effective_order_by,
                                indexed_by: indexed_by.as_ref(),
                                is_count_star_only: is_pure_count_star_projection(projection),
                            },
                        );
                        order_by_satisfied = plan_satisfies_order_by(
                            &plan,
                            &tbl,
                            sname,
                            alias.as_deref(),
                            selection.as_ref(),
                            effective_order_by,
                        );
                        let disp = alias.as_deref().unwrap_or(&tbl.name);
                        emit_plan_nodes(parent, &plan, disp, "", push_node);
                        emit_in_table_eqp_nodes(
                            selection.as_ref(),
                            parent,
                            resolve_relation,
                            push_node,
                        );
                    }
                    ResolvedRelation::View(view) => {
                        explain_view(
                            db_id,
                            sname,
                            &view,
                            alias.as_deref(),
                            selection.as_ref(),
                            parent,
                            active_views,
                            push_node,
                        )?;
                    }
                }
            } else {
                // Multi-table join or subquery/CTE scan
                let mut join_items: Vec<(&TableOrSubquery, Option<JoinType>, Option<&Expr>)> =
                    Vec::with_capacity(1 + fc.joins.len());
                join_items.push((&fc.head, None, None));
                for j in &fc.joins {
                    let on_e = match &j.constraint {
                        Some(JoinConstraint::On(e)) => Some(e),
                        _ => None,
                    };
                    join_items.push((&j.right, Some(j.join_type), on_e));
                }

                // Check if all items are plain base tables with inner joins (eligible for cost-based join ordering)
                let has_cross_or_outer = fc.joins.iter().any(|j| {
                    matches!(
                        j.join_type,
                        JoinType::Cross | JoinType::Left | JoinType::Right | JoinType::Full
                    )
                });
                let mut resolved_tables = Vec::with_capacity(join_items.len());
                for (tos, _, _) in &join_items {
                    if let TableOrSubquery::Table {
                        name,
                        table_args: None,
                        alias,
                        indexed_by,
                    } = tos
                        && !is_cte(name)
                        && let Some((_, sname, slot_schema, ResolvedRelation::Table(tbl))) =
                            resolve_relation(name)
                    {
                        validate_index_hint_in_eqp(
                            slot_schema,
                            &tbl,
                            sname,
                            alias.as_deref(),
                            indexed_by.as_ref(),
                            selection.as_ref(),
                        )?;
                        resolved_tables.push(Some((
                            sname,
                            slot_schema,
                            *tbl,
                            alias.as_deref(),
                            indexed_by.as_ref(),
                        )));
                    } else {
                        resolved_tables.push(None);
                    }
                }

                // Combine WHERE selection with ON constraints of inner joins
                let mut combined_where: Option<Expr> = selection.clone();
                for (_, jtype, on_opt) in &join_items {
                    if jtype.is_none_or(|jt| matches!(jt, JoinType::Inner | JoinType::Cross))
                        && let Some(on_e) = on_opt
                    {
                        combined_where = Some(match combined_where {
                            Some(prev) => Expr::Binary {
                                left: Box::new(prev),
                                op: BinaryOp::And,
                                right: Box::new((*on_e).clone()),
                            },
                            None => (*on_e).clone(),
                        });
                    }
                }

                let mut order: Vec<usize> = (0..join_items.len()).collect();
                if !has_cross_or_outer
                    && order.len() == 2
                    && resolved_tables[0].is_some()
                    && resolved_tables[1].is_some()
                {
                    let (s0, sch0, ref t0, a0, ib0) = *resolved_tables[0].as_ref().unwrap();
                    let (s1, sch1, ref t1, a1, ib1) = *resolved_tables[1].as_ref().unwrap();
                    let disp0 = a0.unwrap_or(&t0.name).to_owned();
                    let disp1 = a1.unwrap_or(&t1.name).to_owned();

                    let p0_outer = choose_access_plan_with_context(
                        t0,
                        s0,
                        a0,
                        combined_where.as_ref(),
                        sch0,
                        QueryPlanContext {
                            projection: Some(projection),
                            group_by,
                            having: having.as_ref(),
                            order_by: &sel.order_by,
                            indexed_by: ib0,
                            is_count_star_only: false,
                        },
                    );
                    let bound_w_for_1 = combined_where.as_ref().map(|w| {
                        rewrite_join_expr_with_bound_tables(w, t1, &[disp0, t0.name.clone()])
                    });
                    let p1_inner = choose_access_plan_with_context(
                        t1,
                        s1,
                        a1,
                        bound_w_for_1.as_ref(),
                        sch1,
                        QueryPlanContext {
                            projection: Some(projection),
                            group_by,
                            having: having.as_ref(),
                            order_by: &sel.order_by,
                            indexed_by: ib1,
                            is_count_star_only: false,
                        },
                    );

                    let p1_outer = choose_access_plan_with_context(
                        t1,
                        s1,
                        a1,
                        combined_where.as_ref(),
                        sch1,
                        QueryPlanContext {
                            projection: Some(projection),
                            group_by,
                            having: having.as_ref(),
                            order_by: &sel.order_by,
                            indexed_by: ib1,
                            is_count_star_only: false,
                        },
                    );
                    let bound_w_for_0 = combined_where.as_ref().map(|w| {
                        rewrite_join_expr_with_bound_tables(w, t0, &[disp1, t1.name.clone()])
                    });
                    let p0_inner = choose_access_plan_with_context(
                        t0,
                        s0,
                        a0,
                        bound_w_for_0.as_ref(),
                        sch0,
                        QueryPlanContext {
                            projection: Some(projection),
                            group_by,
                            having: having.as_ref(),
                            order_by: &sel.order_by,
                            indexed_by: ib0,
                            is_count_star_only: false,
                        },
                    );

                    let (c0_out, r0_out) = plan_cost_and_rows(&p0_outer, t0, sch0);
                    let (c1_in, _) = plan_cost_and_rows(&p1_inner, t1, sch1);
                    let total_01 = sqlite_log_est_add(c0_out, r0_out.saturating_add(c1_in));

                    let (c1_out, r1_out) = plan_cost_and_rows(&p1_outer, t1, sch1);
                    let (c0_in, _) = plan_cost_and_rows(&p0_inner, t0, sch0);
                    let total_10 = sqlite_log_est_add(c1_out, r1_out.saturating_add(c0_in));

                    if total_10 < total_01 {
                        order.swap(0, 1);
                    }
                }

                let mut outer_bound_names: Vec<String> = Vec::new();
                for &idx in &order {
                    let (tos, jtype_opt, on_opt) = join_items[idx];
                    if let Some((sname, slot_schema, ref tbl, alias, indexed_by)) =
                        resolved_tables[idx]
                    {
                        let disp = alias.unwrap_or(&tbl.name);
                        let is_left_join = jtype_opt == Some(JoinType::Left)
                            && !selection.as_ref().is_some_and(|w| {
                                tbl.columns.iter().any(|c| {
                                    expr_implies_non_null_col(w, &c.name, tbl, sname, alias)
                                })
                            });
                        let pred_for_tbl = if is_left_join {
                            on_opt.cloned()
                        } else {
                            match (&combined_where, on_opt) {
                                (Some(cw), Some(oe)) => Some(Expr::Binary {
                                    left: Box::new(cw.clone()),
                                    op: BinaryOp::And,
                                    right: Box::new((*oe).clone()),
                                }),
                                (Some(cw), None) => Some(cw.clone()),
                                (None, Some(oe)) => Some((*oe).clone()),
                                (None, None) => None,
                            }
                        };
                        let bound_pred = pred_for_tbl.as_ref().map(|p| {
                            rewrite_join_expr_with_bound_tables(p, tbl, &outer_bound_names)
                        });
                        let plan = choose_access_plan_with_context(
                            tbl,
                            sname,
                            alias,
                            bound_pred.as_ref(),
                            slot_schema,
                            QueryPlanContext {
                                projection: Some(projection),
                                group_by,
                                having: having.as_ref(),
                                order_by: &sel.order_by,
                                indexed_by,
                                is_count_star_only: false,
                            },
                        );
                        let suffix = if is_left_join { " LEFT-JOIN" } else { "" };
                        emit_plan_nodes(parent, &plan, disp, suffix, push_node);
                        outer_bound_names.push(disp.to_owned());
                        outer_bound_names.push(tbl.name.clone());
                    } else {
                        match tos {
                            TableOrSubquery::Table { name, alias, .. } => {
                                if !is_cte(name)
                                    && let Some((db_id, sname, _, ResolvedRelation::View(view))) =
                                        resolve_relation(name)
                                {
                                    explain_view(
                                        db_id,
                                        sname,
                                        &view,
                                        alias.as_deref(),
                                        None,
                                        parent,
                                        active_views,
                                        push_node,
                                    )?;
                                } else {
                                    let disp_name = alias
                                        .as_deref()
                                        .map(str::to_owned)
                                        .or_else(|| resolve_cte_target_name(name))
                                        .unwrap_or_else(|| name.name.clone());
                                    let _ = push_node(parent, format!("SCAN {disp_name}"));
                                }
                            }
                            TableOrSubquery::Subquery { .. } => {
                                let _ = push_node(parent, "SCAN SUBQUERY".to_owned());
                            }
                        }
                    }
                }
            }
            if !window_sort_groups.is_empty() {
                let first_needs_sort = !order_by_satisfied;
                let total_window_sorts =
                    usize::from(first_needs_sort) + window_sort_groups.len().saturating_sub(1);
                for _ in 0..total_window_sorts {
                    let _ = push_node(parent, "USE TEMP B-TREE FOR ORDER BY".to_owned());
                }
            } else if !sel.order_by.is_empty() && !order_by_satisfied {
                let _ = push_node(parent, "USE TEMP B-TREE FOR ORDER BY".to_owned());
            }
        } else {
            let _ = push_node(parent, "SCAN CONSTANT ROW".to_owned());
        }
        Ok(())
    }

    match stmt {
        Stmt::Select(sel) => {
            let mut active_views = Vec::new();
            explain_select_plan(sel, 0, &resolve_relation, &mut active_views, &mut push_node)?;
        }
        Stmt::Insert(ins) => {
            if let InsertSource::Select(ref src_sel) = ins.source {
                let mut merged_sel = (**src_sel).clone();
                let mut combined_ctes = ins.ctes.clone();
                combined_ctes.extend(merged_sel.ctes);
                merged_sel.ctes = combined_ctes;
                let mut active_views = Vec::new();
                explain_select_plan(
                    &merged_sel,
                    0,
                    &resolve_relation,
                    &mut active_views,
                    &mut push_node,
                )?;
            } else {
                let _ = push_node(0, "EXECUTE STATEMENT".to_owned());
            }
        }
        Stmt::Update(upd) => {
            if let Some((_, sname, slot_schema, ResolvedRelation::Table(tbl))) =
                resolve_relation(&upd.table)
            {
                let sel_pred = if upd.from.is_none() {
                    upd.selection.as_ref()
                } else {
                    None
                };
                let plan = choose_access_plan_with_schema_name(
                    &tbl,
                    sname,
                    upd.alias.as_deref(),
                    sel_pred,
                    slot_schema,
                );
                let disp = upd.alias.as_deref().unwrap_or(&tbl.name);
                emit_plan_nodes(0, &plan, disp, "", &mut push_node);
                if let Some(ref fc) = upd.from {
                    let from_sel = SelectStmt {
                        ctes: upd.ctes.clone(),
                        body: SelectBody {
                            head: SelectCore::Select {
                                distinct: false,
                                projection: vec![ResultColumn::Star],
                                from: Some(fc.clone()),
                                selection: None,
                                group_by: Vec::new(),
                                having: None,
                                windows: Vec::new(),
                            },
                            compounds: Vec::new(),
                        },
                        order_by: Vec::new(),
                        limit: None,
                    };
                    let mut active_views = Vec::new();
                    explain_select_plan(
                        &from_sel,
                        0,
                        &resolve_relation,
                        &mut active_views,
                        &mut push_node,
                    )?;
                }
            }
        }
        Stmt::Delete(del) => {
            if let Some((_, sname, slot_schema, ResolvedRelation::Table(tbl))) =
                resolve_relation(&del.table)
            {
                let plan = choose_access_plan_with_schema_name(
                    &tbl,
                    sname,
                    del.alias.as_deref(),
                    del.selection.as_ref(),
                    slot_schema,
                );
                let disp = del.alias.as_deref().unwrap_or(&del.table.name);
                emit_plan_nodes(0, &plan, disp, "", &mut push_node);
            }
        }
        _ => {
            let _ = push_node(0, "EXECUTE STATEMENT".to_owned());
        }
    }

    Ok(rows)
}
