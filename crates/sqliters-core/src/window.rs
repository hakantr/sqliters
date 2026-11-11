//! Window function resolution, validation, frame calculation, and multi-stage execution engine
//! (`sqlite/src/window.c`, `sqlite/src/resolve.c`, `sqlite/src/select.c`).
#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::rc::Rc;

use sqliters_syntax::{
    ColumnRef, Expr, FromClause, FunctionArgs, JoinConstraint, OrderByTerm, ResultColumn,
    SelectCore, SelectStmt, TableOrSubquery, UnaryOp, WindowFrame, WindowFrameBound,
    WindowFrameExclude, WindowFrameUnits, WindowRef, WindowSpec,
};
use sqliters_types::{
    Collation, ErrorCode, ExecutionServices, FunctionFlags, KeySortOrder, Result, RuntimeValue,
    SqliteError, TextEncoding, Value, fallible_sort_by,
};
use sqliters_vdbe::{AggregateKind, builtin_window_arity};

use crate::compiler::expr_contains_aggregate;
use crate::engine::{
    CteStore, EvalRow, ExecContext, ScopedColumn, col_in_scoped_cols_pub, eval_expr_pub,
    eval_expr_rt_pub, infer_expr_collation_from_cols_pub,
    infer_expr_custom_collation_from_cols_pub, push_scope_pub,
};
use crate::execution_context::{CollationRegistry, ExecutionContext, FunctionRegistry};

/// Synthetic `ColumnRef::schema` marker used for precomputed window-function result slots on [`EvalRow`].
pub(crate) const WIN_SLOT_SCHEMA: &str = "\0win";

// ============================================================================
// AST Predicates & Structural Helpers
// ============================================================================

/// Returns `true` if `expr` contains a window function call belonging to the current `SELECT`
/// (does not cross `Subquery` / `Exists` / `InSubquery` boundaries).
pub fn expr_contains_window(expr: &Expr) -> bool {
    first_window_function_name_in_expr(expr).is_some()
}

/// Returns the name of the first window function (`OVER ...`) in `expr` at the current query level
/// (does not cross `Subquery` / `Exists` / `InSubquery` boundaries).
pub fn first_window_function_name_in_expr(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::FunctionCall {
            name,
            args,
            filter,
            over,
            ..
        } => {
            if over.is_some() {
                return Some(name.as_str());
            }
            if let FunctionArgs::Exprs(es) = args {
                for a in es {
                    if let Some(found) = first_window_function_name_in_expr(a) {
                        return Some(found);
                    }
                }
            }
            if let Some(fe) = filter
                && let Some(found) = first_window_function_name_in_expr(fe)
            {
                return Some(found);
            }
            None
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => first_window_function_name_in_expr(inner),
        Expr::Binary { left, right, .. } => first_window_function_name_in_expr(left)
            .or_else(|| first_window_function_name_in_expr(right)),
        Expr::Between {
            expr: e, low, high, ..
        } => first_window_function_name_in_expr(e)
            .or_else(|| first_window_function_name_in_expr(low))
            .or_else(|| first_window_function_name_in_expr(high)),
        Expr::InList { expr: e, list, .. } => first_window_function_name_in_expr(e)
            .or_else(|| list.iter().find_map(first_window_function_name_in_expr)),
        Expr::InTable { expr: e, .. } | Expr::InSubquery { expr: e, .. } => {
            first_window_function_name_in_expr(e)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => first_window_function_name_in_expr(e)
            .or_else(|| first_window_function_name_in_expr(pattern))
            .or_else(|| {
                escape
                    .as_deref()
                    .and_then(first_window_function_name_in_expr)
            }),
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => operand
            .as_deref()
            .and_then(first_window_function_name_in_expr)
            .or_else(|| {
                branches.iter().find_map(|(w, t)| {
                    first_window_function_name_in_expr(w)
                        .or_else(|| first_window_function_name_in_expr(t))
                })
            })
            .or_else(|| {
                else_expr
                    .as_deref()
                    .and_then(first_window_function_name_in_expr)
            }),
        Expr::RowValue(elems) => elems.iter().find_map(first_window_function_name_in_expr),
        Expr::Raise {
            message: Some(m), ..
        } => first_window_function_name_in_expr(m),
        _ => None,
    }
}

/// Rejects any window function call inside a DDL expression (`CREATE INDEX`, `CHECK`, `DEFAULT`,
/// `GENERATED ALWAYS AS`), returning `"misuse of window function <name>()"`.
pub fn reject_window_in_ddl_expr(expr: &Expr) -> Result<()> {
    if let Some(wname) = first_window_function_name_in_expr(expr) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("misuse of window function {wname}()"),
        ));
    }
    Ok(())
}

/// Returns `true` if `wspec` (`PARTITION BY` or `ORDER BY`) contains any regular aggregate call.
pub fn window_spec_contains_aggregate(wspec: &WindowSpec) -> bool {
    wspec.partition_by.iter().any(expr_contains_aggregate)
        || wspec
            .order_by
            .iter()
            .any(|ob| expr_contains_aggregate(&ob.expr))
}

/// Returns `true` if `wref` directly contains any regular aggregate call in an inline `WindowSpec`.
pub fn window_ref_contains_aggregate(wref: &WindowRef) -> bool {
    match wref {
        WindowRef::Name(_) => false,
        WindowRef::Spec(spec) => window_spec_contains_aggregate(spec),
    }
}

/// Returns `true` if `core` or `order_by` invokes any window function (`OVER ...`).
pub fn core_has_window(core: &SelectCore, order_by: &[OrderByTerm]) -> bool {
    let in_core = match core {
        SelectCore::Values(rows) => rows.iter().any(|r| r.iter().any(expr_contains_window)),
        SelectCore::Select { projection, .. } => projection.iter().any(|rc| match rc {
            ResultColumn::Expr { expr, .. } => expr_contains_window(expr),
            _ => false,
        }),
    };
    in_core || order_by.iter().any(|ob| expr_contains_window(&ob.expr))
}

/// Returns `true` if `core` or `order_by` is an aggregated query (`GROUP BY`, `HAVING`, or
/// regular aggregate inside `projection`, `order_by`, or any referenced `WINDOW` definition).
pub fn core_has_regular_aggregates(core: &SelectCore, order_by: &[OrderByTerm]) -> bool {
    let SelectCore::Select {
        projection,
        group_by,
        having,
        windows,
        ..
    } = core
    else {
        return false;
    };
    if !group_by.is_empty()
        || having.is_some()
        || projection.iter().any(|rc| match rc {
            ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
            _ => false,
        })
        || order_by.iter().any(|ob| expr_contains_aggregate(&ob.expr))
    {
        return true;
    }
    if windows.is_empty() {
        return false;
    }
    let Ok(resolved_defs) = chain_window_definitions(windows) else {
        return false;
    };
    let mut referenced = false;
    for rc in projection {
        if let ResultColumn::Expr { expr, .. } = rc
            && expr_references_agg_in_defs(expr, &resolved_defs)
        {
            referenced = true;
            break;
        }
    }
    if !referenced {
        for ob in order_by {
            if expr_references_agg_in_defs(&ob.expr, &resolved_defs) {
                referenced = true;
                break;
            }
        }
    }
    referenced
}

fn expr_references_agg_in_defs(expr: &Expr, defs: &[(String, ResolvedWindowSpec)]) -> bool {
    match expr {
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if let Some(wref) = over
                && let Ok(spec) = resolve_over_clause(wref, defs)
                && (spec.partition_by.iter().any(expr_contains_aggregate)
                    || spec
                        .order_by
                        .iter()
                        .any(|ob| expr_contains_aggregate(&ob.expr)))
            {
                return true;
            }
            let in_args = match args {
                FunctionArgs::Star => false,
                FunctionArgs::Exprs(es) => es.iter().any(|e| expr_references_agg_in_defs(e, defs)),
            };
            in_args
                || filter
                    .as_deref()
                    .is_some_and(|f| expr_references_agg_in_defs(f, defs))
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => expr_references_agg_in_defs(inner, defs),
        Expr::Binary { left, right, .. } => {
            expr_references_agg_in_defs(left, defs) || expr_references_agg_in_defs(right, defs)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_references_agg_in_defs(e, defs)
                || expr_references_agg_in_defs(low, defs)
                || expr_references_agg_in_defs(high, defs)
        }
        Expr::InList { expr: e, list, .. } => {
            expr_references_agg_in_defs(e, defs)
                || list.iter().any(|x| expr_references_agg_in_defs(x, defs))
        }
        Expr::InTable { expr: e, .. } | Expr::InSubquery { expr: e, .. } => {
            expr_references_agg_in_defs(e, defs)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_references_agg_in_defs(e, defs)
                || expr_references_agg_in_defs(pattern, defs)
                || escape
                    .as_deref()
                    .is_some_and(|esc| expr_references_agg_in_defs(esc, defs))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_deref()
                .is_some_and(|o| expr_references_agg_in_defs(o, defs))
                || branches.iter().any(|(w, t)| {
                    expr_references_agg_in_defs(w, defs) || expr_references_agg_in_defs(t, defs)
                })
                || else_expr
                    .as_deref()
                    .is_some_and(|el| expr_references_agg_in_defs(el, defs))
        }
        Expr::RowValue(es) => es.iter().any(|e| expr_references_agg_in_defs(e, defs)),
        _ => false,
    }
}

/// Exact structural constant check for `<expr> PRECEDING` / `<expr> FOLLOWING` frame offsets
/// (`sqlite3WindowOffsetExpr` + `sqlite3ExprIsConstant(0, pExpr)` in `sqlite/src/window.c:1169`
/// and `sqlite/src/expr.c:2577-2667`).
///
/// Any expression containing columns, function calls, subqueries, or `RAISE` is treated as
/// non-constant (`NULL` at runtime), while literals, operators, and bind parameters (`?`) remain
/// constant.
pub fn is_window_bound_constant(expr: &Expr) -> bool {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_)
        | Expr::OversizedHex(_) => true,
        Expr::Column(_)
        | Expr::FunctionCall { .. }
        | Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::InTable { .. }
        | Expr::Raise { .. } => false,
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => is_window_bound_constant(inner),
        Expr::Binary { left, right, .. } => {
            is_window_bound_constant(left) && is_window_bound_constant(right)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            is_window_bound_constant(e)
                && is_window_bound_constant(low)
                && is_window_bound_constant(high)
        }
        Expr::InList { expr: e, list, .. } => {
            is_window_bound_constant(e) && list.iter().all(is_window_bound_constant)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            is_window_bound_constant(e)
                && is_window_bound_constant(pattern)
                && escape.as_deref().is_none_or(is_window_bound_constant)
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_none_or(is_window_bound_constant)
                && branches
                    .iter()
                    .all(|(w, t)| is_window_bound_constant(w) && is_window_bound_constant(t))
                && else_expr.as_deref().is_none_or(is_window_bound_constant)
        }
        Expr::RowValue(es) => es.iter().all(is_window_bound_constant),
    }
}

/// Checks whether an outer `WHERE` predicate `outer_where` (already rewritten in terms of the
/// subquery's result expressions) may be safely pushed into `subquery` when `subquery` uses
/// window functions (`pushDownWindowCheck` in `sqlite/src/select.c:4177-4210`).
///
/// Safe iff `subquery` is a non-compound `SELECT` where every window function shares the same
/// `PARTITION BY` list, and every column/aggregate referenced by `outer_where` appears in that
/// common `PARTITION BY` list.
pub fn can_push_where_into_window_select(subquery: &SelectStmt, outer_where: &Expr) -> bool {
    if !subquery.body.compounds.is_empty() {
        return false;
    }
    let SelectCore::Select {
        projection,
        windows,
        ..
    } = &subquery.body.head
    else {
        return false;
    };
    if !core_has_window(&subquery.body.head, &subquery.order_by) {
        return true;
    }
    let Ok(defs) = chain_window_definitions(windows) else {
        return false;
    };
    let mut specs = Vec::new();
    for rc in projection {
        if let ResultColumn::Expr { expr, .. } = rc
            && !collect_resolved_specs_in_expr(expr, &defs, &mut specs)
        {
            return false;
        }
    }
    for ob in &subquery.order_by {
        if !collect_resolved_specs_in_expr(&ob.expr, &defs, &mut specs) {
            return false;
        }
    }
    let Some(first) = specs.first() else {
        return true;
    };
    if first.partition_by.is_empty() {
        return false;
    }
    if specs
        .iter()
        .skip(1)
        .any(|s| s.partition_by != first.partition_by)
    {
        return false;
    }
    expr_only_references_partition_terms(outer_where, &first.partition_by)
}

fn order_by_terms_match(a: &OrderByTerm, b: &OrderByTerm) -> bool {
    a.expr == b.expr
        && a.desc == b.desc
        && a.nulls_last.unwrap_or(a.desc) == b.nulls_last.unwrap_or(b.desc)
}

fn order_by_slice_is_prefix(short: &[OrderByTerm], long: &[OrderByTerm]) -> bool {
    short.len() <= long.len()
        && short
            .iter()
            .zip(long.iter())
            .all(|(a, b)| order_by_terms_match(a, b))
}

/// Collects the distinct merged `(PARTITION BY ++ ORDER BY)` sort-order groups required by
/// window functions in `sel` (`selectWindowRewriteEList` in `sqlite/src/window.c`).
pub fn collect_window_sort_groups(sel: &SelectStmt) -> Vec<Vec<OrderByTerm>> {
    if !core_has_window(&sel.body.head, &sel.order_by) {
        return Vec::new();
    }
    let SelectCore::Select {
        projection,
        windows,
        ..
    } = &sel.body.head
    else {
        return Vec::new();
    };
    let defs = chain_window_definitions(windows).unwrap_or_default();
    let mut specs = Vec::new();
    for rc in projection {
        if let ResultColumn::Expr { expr, .. } = rc {
            let _ = collect_resolved_specs_in_expr(expr, &defs, &mut specs);
        }
    }
    for ob in &sel.order_by {
        let _ = collect_resolved_specs_in_expr(&ob.expr, &defs, &mut specs);
    }
    let mut groups: Vec<Vec<OrderByTerm>> = Vec::new();
    for spec in specs {
        let mut combined: Vec<OrderByTerm> =
            Vec::with_capacity(spec.partition_by.len() + spec.order_by.len());
        for p_expr in spec.partition_by {
            let term = OrderByTerm {
                expr: p_expr,
                desc: false,
                nulls_last: None,
            };
            if !combined
                .iter()
                .any(|existing| order_by_terms_match(existing, &term))
            {
                combined.push(term);
            }
        }
        for ob in spec.order_by {
            if !combined
                .iter()
                .any(|existing| order_by_terms_match(existing, &ob))
            {
                combined.push(ob);
            }
        }
        if combined.is_empty() {
            continue;
        }
        if let Some(existing_group) = groups.iter_mut().find(|g| {
            order_by_slice_is_prefix(g, &combined) || order_by_slice_is_prefix(&combined, g)
        }) {
            if combined.len() > existing_group.len() {
                *existing_group = combined;
            }
        } else {
            groups.push(combined);
        }
    }
    groups
}

fn collect_resolved_specs_in_expr(
    expr: &Expr,
    defs: &[(String, ResolvedWindowSpec)],
    out: &mut Vec<ResolvedWindowSpec>,
) -> bool {
    match expr {
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if let Some(wref) = over {
                let Ok(spec) = resolve_over_clause(wref, defs) else {
                    return false;
                };
                out.push(spec);
                return true;
            }
            if let FunctionArgs::Exprs(es) = args {
                for a in es {
                    if !collect_resolved_specs_in_expr(a, defs, out) {
                        return false;
                    }
                }
            }
            if let Some(fe) = filter
                && !collect_resolved_specs_in_expr(fe, defs, out)
            {
                return false;
            }
            true
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => collect_resolved_specs_in_expr(inner, defs, out),
        Expr::Binary { left, right, .. } => {
            collect_resolved_specs_in_expr(left, defs, out)
                && collect_resolved_specs_in_expr(right, defs, out)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            collect_resolved_specs_in_expr(e, defs, out)
                && collect_resolved_specs_in_expr(low, defs, out)
                && collect_resolved_specs_in_expr(high, defs, out)
        }
        Expr::InList { expr: e, list, .. } => {
            collect_resolved_specs_in_expr(e, defs, out)
                && list
                    .iter()
                    .all(|x| collect_resolved_specs_in_expr(x, defs, out))
        }
        Expr::InTable { expr: e, .. } | Expr::InSubquery { expr: e, .. } => {
            collect_resolved_specs_in_expr(e, defs, out)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_resolved_specs_in_expr(e, defs, out)
                && collect_resolved_specs_in_expr(pattern, defs, out)
                && escape
                    .as_deref()
                    .is_none_or(|esc| collect_resolved_specs_in_expr(esc, defs, out))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_deref()
                .is_none_or(|o| collect_resolved_specs_in_expr(o, defs, out))
                && branches.iter().all(|(w, t)| {
                    collect_resolved_specs_in_expr(w, defs, out)
                        && collect_resolved_specs_in_expr(t, defs, out)
                })
                && else_expr
                    .as_deref()
                    .is_none_or(|el| collect_resolved_specs_in_expr(el, defs, out))
        }
        Expr::RowValue(es) => es
            .iter()
            .all(|e| collect_resolved_specs_in_expr(e, defs, out)),
        _ => true,
    }
}

fn expr_only_references_partition_terms(expr: &Expr, partition_by: &[Expr]) -> bool {
    if partition_by.contains(expr) {
        return true;
    }
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::Bind(_)
        | Expr::CurrentTimeKw(_) => true,
        Expr::Column(cref) => partition_by.iter().any(|pe| match pe {
            Expr::Column(pcref) => {
                pcref.column.eq_ignore_ascii_case(&cref.column)
                    && (cref.table.is_none()
                        || pcref.table.is_none()
                        || pcref
                            .table
                            .as_deref()
                            .zip(cref.table.as_deref())
                            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)))
            }
            _ => false,
        }),
        Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::InTable { .. }
        | Expr::Raise { .. }
        | Expr::OversizedHex(_) => false,
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            expr_only_references_partition_terms(inner, partition_by)
        }
        Expr::Binary { left, right, .. } => {
            expr_only_references_partition_terms(left, partition_by)
                && expr_only_references_partition_terms(right, partition_by)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_only_references_partition_terms(e, partition_by)
                && expr_only_references_partition_terms(low, partition_by)
                && expr_only_references_partition_terms(high, partition_by)
        }
        Expr::InList { expr: e, list, .. } => {
            expr_only_references_partition_terms(e, partition_by)
                && list
                    .iter()
                    .all(|x| expr_only_references_partition_terms(x, partition_by))
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_only_references_partition_terms(e, partition_by)
                && expr_only_references_partition_terms(pattern, partition_by)
                && escape
                    .as_deref()
                    .is_none_or(|esc| expr_only_references_partition_terms(esc, partition_by))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_deref()
                .is_none_or(|o| expr_only_references_partition_terms(o, partition_by))
                && branches.iter().all(|(w, t)| {
                    expr_only_references_partition_terms(w, partition_by)
                        && expr_only_references_partition_terms(t, partition_by)
                })
                && else_expr
                    .as_deref()
                    .is_none_or(|el| expr_only_references_partition_terms(el, partition_by))
        }
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if over.is_some() || filter.is_some() || expr_contains_aggregate(expr) {
                return false;
            }
            match args {
                FunctionArgs::Star => false,
                FunctionArgs::Exprs(es) => es
                    .iter()
                    .all(|a| expr_only_references_partition_terms(a, partition_by)),
            }
        }
        Expr::RowValue(es) => es
            .iter()
            .all(|e| expr_only_references_partition_terms(e, partition_by)),
    }
}

// ============================================================================
// Window Chaining, Resolution & Function Classification
// ============================================================================

/// Fully resolved window specification with explicit or default frame bounds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedWindowSpec {
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderByTerm>,
    pub units: WindowFrameUnits,
    pub start: WindowFrameBound,
    pub end: WindowFrameBound,
    pub exclude: WindowFrameExclude,
    pub implicit_frame: bool,
}

impl ResolvedWindowSpec {
    fn from_unbased_spec(spec: &WindowSpec) -> Self {
        match &spec.frame {
            Some(f) => Self {
                partition_by: spec.partition_by.clone(),
                order_by: spec.order_by.clone(),
                units: f.units,
                start: f.start.clone(),
                end: f.end.clone(),
                exclude: f.exclude,
                implicit_frame: false,
            },
            None => Self {
                partition_by: spec.partition_by.clone(),
                order_by: spec.order_by.clone(),
                units: WindowFrameUnits::Range,
                start: WindowFrameBound::UnboundedPreceding,
                end: WindowFrameBound::CurrentRow,
                exclude: WindowFrameExclude::NoOthers,
                implicit_frame: true,
            },
        }
    }

    fn apply_frame_opt(&mut self, frame: Option<&WindowFrame>) {
        if let Some(f) = frame {
            self.units = f.units;
            self.start = f.start.clone();
            self.end = f.end.clone();
            self.exclude = f.exclude;
            self.implicit_frame = false;
        } else {
            self.units = WindowFrameUnits::Range;
            self.start = WindowFrameBound::UnboundedPreceding;
            self.end = WindowFrameBound::CurrentRow;
            self.exclude = WindowFrameExclude::NoOthers;
            self.implicit_frame = true;
        }
    }
}

fn chain_against_base(
    base_name: &str,
    child: &WindowSpec,
    earlier: &[(String, ResolvedWindowSpec)],
) -> Result<ResolvedWindowSpec> {
    let Some((_, base)) = earlier
        .iter()
        .rev()
        .find(|(nm, _)| nm.eq_ignore_ascii_case(base_name))
    else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such window: {base_name}"),
        ));
    };
    if !child.partition_by.is_empty() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("cannot override PARTITION clause of window: {base_name}"),
        ));
    }
    if !base.order_by.is_empty() && !child.order_by.is_empty() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("cannot override ORDER BY clause of window: {base_name}"),
        ));
    }
    if !base.implicit_frame {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("cannot override frame specification of window: {base_name}"),
        ));
    }
    let mut out = ResolvedWindowSpec {
        partition_by: base.partition_by.clone(),
        order_by: if !child.order_by.is_empty() {
            child.order_by.clone()
        } else {
            base.order_by.clone()
        },
        units: WindowFrameUnits::Range,
        start: WindowFrameBound::UnboundedPreceding,
        end: WindowFrameBound::CurrentRow,
        exclude: WindowFrameExclude::NoOthers,
        implicit_frame: true,
    };
    out.apply_frame_opt(child.frame.as_ref());
    Ok(out)
}

/// Resolves and chains a `SELECT`'s `WINDOW w1 AS (...), w2 AS (...)` list (`parse.y:1995-2000`,
/// `sqlite3WindowChain` in `sqlite/src/window.c:1280-1312`).
pub(crate) fn chain_window_definitions(
    defs: &[(String, WindowSpec)],
) -> Result<Vec<(String, ResolvedWindowSpec)>> {
    let mut resolved: Vec<(String, ResolvedWindowSpec)> = Vec::with_capacity(defs.len());
    for (idx, (name, spec)) in defs.iter().enumerate() {
        let rspec = if idx >= 1
            && let Some(ref base_name) = spec.base_window
        {
            chain_against_base(base_name, spec, &resolved)?
        } else {
            ResolvedWindowSpec::from_unbased_spec(spec)
        };
        resolved.push((name.clone(), rspec));
    }
    Ok(resolved)
}

/// Resolves an `OVER w` or `OVER (...)` reference against chained `WINDOW` definitions
/// (`sqlite3WindowUpdate` in `sqlite/src/window.c:668-681`).
pub(crate) fn resolve_over_clause(
    over: &WindowRef,
    resolved_defs: &[(String, ResolvedWindowSpec)],
) -> Result<ResolvedWindowSpec> {
    match over {
        WindowRef::Name(wname) => {
            let Some((_, found)) = resolved_defs
                .iter()
                .rev()
                .find(|(nm, _)| nm.eq_ignore_ascii_case(wname))
            else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such window: {wname}"),
                ));
            };
            Ok(found.clone())
        }
        WindowRef::Spec(spec) => {
            if let Some(ref base_name) = spec.base_window {
                chain_against_base(base_name, spec, resolved_defs)
            } else {
                Ok(ResolvedWindowSpec::from_unbased_spec(spec))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinWinFn {
    RowNumber,
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
    Ntile,
    Lag,
    Lead,
    FirstValue,
    LastValue,
    NthValue,
}

impl BuiltinWinFn {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "ROW_NUMBER" => Some(Self::RowNumber),
            "RANK" => Some(Self::Rank),
            "DENSE_RANK" => Some(Self::DenseRank),
            "PERCENT_RANK" => Some(Self::PercentRank),
            "CUME_DIST" => Some(Self::CumeDist),
            "NTILE" => Some(Self::Ntile),
            "LAG" => Some(Self::Lag),
            "LEAD" => Some(Self::Lead),
            "FIRST_VALUE" => Some(Self::FirstValue),
            "LAST_VALUE" => Some(Self::LastValue),
            "NTH_VALUE" => Some(Self::NthValue),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WindowFnKind {
    Builtin(BuiltinWinFn),
    Aggregate {
        name: String,
        arity: usize,
        is_star: bool,
        distinct: bool,
        flags: FunctionFlags,
    },
}

/// Validates function name, arity, `DISTINCT`, and window capability for `name(...) OVER ...`
/// (`resolve.c:1185-1285`).
pub(crate) fn classify_and_validate_window_fn(
    name: &str,
    is_star: bool,
    arg_count: usize,
    distinct: bool,
    functions: &FunctionRegistry,
) -> Result<WindowFnKind> {
    if distinct {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "DISTINCT is not supported for window functions",
        ));
    }

    // 1. Check connection-registered aggregate/window functions first (supports overriding built-ins, e.g. `window5-3.0`)
    let lookup_arity = if is_star { 0 } else { arg_count };
    if let Some(def) = functions.find_aggregate(name, lookup_arity) {
        if is_star && lookup_arity != 0 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("wrong number of arguments to function {name}()"),
            ));
        }
        if !def.supports_window {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("{name}() may not be used as a window function"),
            ));
        }
        return Ok(WindowFnKind::Aggregate {
            name: def.name.clone(),
            arity: lookup_arity,
            is_star,
            distinct,
            flags: def.flags,
        });
    }
    if !is_star && functions.find_scalar(name, arg_count).is_some() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("{name}() may not be used as a window function"),
        ));
    }

    // 2. Check the 11 built-in window-only functions
    let upper = name.to_ascii_uppercase();
    if let Some((min_a, max_a)) = builtin_window_arity(&upper) {
        if is_star || arg_count < min_a || arg_count > max_a {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("wrong number of arguments to function {name}()"),
            ));
        }
        let bfn = BuiltinWinFn::from_name(name).expect("verified builtin window function");
        return Ok(WindowFnKind::Builtin(bfn));
    }

    // 3. Check built-in aggregate functions usable as window functions
    match upper.as_str() {
        "COUNT" => {
            if !is_star && arg_count > 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            let count_star = is_star || arg_count == 0;
            return Ok(WindowFnKind::Aggregate {
                name: name.to_owned(),
                arity: if count_star { 0 } else { 1 },
                is_star: count_star,
                distinct,
                flags: FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
            });
        }
        "SUM" | "TOTAL" | "AVG" => {
            if is_star || arg_count != 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            return Ok(WindowFnKind::Aggregate {
                name: name.to_owned(),
                arity: 1,
                is_star: false,
                distinct,
                flags: FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
            });
        }
        "MIN" | "MAX" => {
            if is_star || arg_count == 0 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            if arg_count > 1 {
                // Multi-arg `min(a, b)` / `max(a, b)` is a scalar function, not a window function!
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("{name}() may not be used as a window function"),
                ));
            }
            return Ok(WindowFnKind::Aggregate {
                name: name.to_owned(),
                arity: 1,
                is_star: false,
                distinct,
                flags: FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
            });
        }
        "GROUP_CONCAT" => {
            if is_star || !(1..=2).contains(&arg_count) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            return Ok(WindowFnKind::Aggregate {
                name: name.to_owned(),
                arity: arg_count,
                is_star: false,
                distinct,
                flags: FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
            });
        }
        "STRING_AGG" => {
            if is_star || arg_count != 2 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            return Ok(WindowFnKind::Aggregate {
                name: name.to_owned(),
                arity: 2,
                is_star: false,
                distinct,
                flags: FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
            });
        }
        "JSON_GROUP_ARRAY" | "JSONB_GROUP_ARRAY" => {
            if is_star || arg_count != 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            let kind = AggregateKind::from_name(name, false).expect("verified json group array");
            return Ok(WindowFnKind::Aggregate {
                name: name.to_owned(),
                arity: 1,
                is_star: false,
                distinct,
                flags: kind.function_flags(),
            });
        }
        "JSON_GROUP_OBJECT" | "JSONB_GROUP_OBJECT" => {
            if is_star || arg_count != 2 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            let kind = AggregateKind::from_name(name, false).expect("verified json group object");
            return Ok(WindowFnKind::Aggregate {
                name: name.to_owned(),
                arity: 2,
                is_star: false,
                distinct,
                flags: kind.function_flags(),
            });
        }
        _ => {}
    }

    if functions.has_aggregate_name(name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("wrong number of arguments to function {name}()"),
        ));
    }
    // Otherwise, validate as a scalar call: if unknown -> "no such function: <name>",
    // if wrong arity -> "wrong number of arguments to function <name>()",
    // if valid scalar -> "<name>() may not be used as a window function".
    functions.validate_call(name, is_star, arg_count, false, false, false)?;
    Err(SqliteError::new(
        ErrorCode::Error,
        format!("{name}() may not be used as a window function"),
    ))
}

/// Validates `RANGE` offset `ORDER BY` arity, `FILTER` legality, and coerces frames for
/// frame-ignoring built-in window functions (`sqlite3WindowUpdate` in `sqlite/src/window.c:682-730`).
pub(crate) fn finalize_window_spec_for_fn(
    mut spec: ResolvedWindowSpec,
    fn_kind: &WindowFnKind,
    has_filter: bool,
) -> Result<ResolvedWindowSpec> {
    let has_offset_bound = matches!(
        spec.start,
        WindowFrameBound::Preceding(_) | WindowFrameBound::Following(_)
    ) || matches!(
        spec.end,
        WindowFrameBound::Preceding(_) | WindowFrameBound::Following(_)
    );
    if spec.units == WindowFrameUnits::Range && has_offset_bound && spec.order_by.len() != 1 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "RANGE with offset PRECEDING/FOLLOWING requires one ORDER BY expression",
        ));
    }

    if let WindowFnKind::Builtin(bfn) = fn_kind {
        if has_filter {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "FILTER clause may only be used with aggregate window functions",
            ));
        }
        match bfn {
            BuiltinWinFn::RowNumber
            | BuiltinWinFn::Ntile
            | BuiltinWinFn::Lead
            | BuiltinWinFn::Lag => {
                spec.units = WindowFrameUnits::Rows;
                spec.start = WindowFrameBound::UnboundedPreceding;
                spec.end = WindowFrameBound::CurrentRow;
                spec.exclude = WindowFrameExclude::NoOthers;
            }
            BuiltinWinFn::DenseRank
            | BuiltinWinFn::Rank
            | BuiltinWinFn::PercentRank
            | BuiltinWinFn::CumeDist => {
                spec.units = WindowFrameUnits::Range;
                spec.start = WindowFrameBound::UnboundedPreceding;
                spec.end = WindowFrameBound::CurrentRow;
                spec.exclude = WindowFrameExclude::NoOthers;
            }
            BuiltinWinFn::FirstValue | BuiltinWinFn::LastValue | BuiltinWinFn::NthValue => {}
        }
    }

    Ok(spec)
}

/// Checks `expr` for illegal references to `SELECT` aliases whose expressions contain window
/// functions (`"misuse of aliased window function <alias>"`, `resolve.c:678-682`).
pub(crate) fn check_aliased_window_misuse(
    expr: &Expr,
    window_aliases: &[String],
    sample_cols: &[ScopedColumn],
    allow_top_level_win_alias: bool,
    functions: &FunctionRegistry,
) -> Result<()> {
    if window_aliases.is_empty() {
        return Ok(());
    }
    check_aliased_win_inner(
        expr,
        window_aliases,
        sample_cols,
        allow_top_level_win_alias,
        functions,
    )
}

fn check_aliased_win_inner(
    expr: &Expr,
    window_aliases: &[String],
    sample_cols: &[ScopedColumn],
    allow_here: bool,
    functions: &FunctionRegistry,
) -> Result<()> {
    match expr {
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => {
            if !allow_here
                && !col_in_scoped_cols_pub(column, sample_cols)
                && let Some(al) = window_aliases
                    .iter()
                    .find(|a| a.eq_ignore_ascii_case(column))
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("misuse of aliased window function {al}"),
                ));
            }
            Ok(())
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            check_aliased_win_inner(inner, window_aliases, sample_cols, allow_here, functions)
        }
        Expr::Binary { left, right, .. } => {
            check_aliased_win_inner(left, window_aliases, sample_cols, allow_here, functions)?;
            check_aliased_win_inner(right, window_aliases, sample_cols, allow_here, functions)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            check_aliased_win_inner(e, window_aliases, sample_cols, allow_here, functions)?;
            check_aliased_win_inner(low, window_aliases, sample_cols, allow_here, functions)?;
            check_aliased_win_inner(high, window_aliases, sample_cols, allow_here, functions)
        }
        Expr::InList { expr: e, list, .. } => {
            check_aliased_win_inner(e, window_aliases, sample_cols, allow_here, functions)?;
            for item in list {
                check_aliased_win_inner(item, window_aliases, sample_cols, allow_here, functions)?;
            }
            Ok(())
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            check_aliased_win_inner(e, window_aliases, sample_cols, allow_here, functions)?;
            check_aliased_win_in_subquery(subquery, window_aliases, functions)
        }
        Expr::InTable { expr: e, .. } => {
            check_aliased_win_inner(e, window_aliases, sample_cols, allow_here, functions)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            check_aliased_win_inner(e, window_aliases, sample_cols, allow_here, functions)?;
            check_aliased_win_inner(pattern, window_aliases, sample_cols, allow_here, functions)?;
            if let Some(esc) = escape {
                check_aliased_win_inner(esc, window_aliases, sample_cols, allow_here, functions)?;
            }
            Ok(())
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                check_aliased_win_inner(op, window_aliases, sample_cols, allow_here, functions)?;
            }
            for (w, t) in branches {
                check_aliased_win_inner(w, window_aliases, sample_cols, allow_here, functions)?;
                check_aliased_win_inner(t, window_aliases, sample_cols, allow_here, functions)?;
            }
            if let Some(el) = else_expr {
                check_aliased_win_inner(el, window_aliases, sample_cols, allow_here, functions)?;
            }
            Ok(())
        }
        Expr::FunctionCall {
            name,
            args,
            filter,
            over,
            ..
        } => {
            let arg_count = match args {
                FunctionArgs::Star => 0,
                FunctionArgs::Exprs(es) => es.len(),
            };
            let is_star = matches!(args, FunctionArgs::Star);
            let is_agg = if over.is_some()
                || (!is_star && functions.find_scalar(name, arg_count).is_some())
            {
                false
            } else if functions.find_aggregate(name, arg_count).is_some() {
                true
            } else {
                let upper = name.to_ascii_uppercase();
                if (upper == "MIN" || upper == "MAX") && arg_count != 1 {
                    false
                } else if upper == "COUNT" && arg_count == 0 {
                    true
                } else {
                    AggregateKind::from_name(name, is_star).is_some()
                }
            };
            // Inside an aggregate or window function call, `NC_AllowWin` is cleared!
            let child_allow = allow_here && over.is_none() && !is_agg;
            if let FunctionArgs::Exprs(es) = args {
                for a in es {
                    check_aliased_win_inner(
                        a,
                        window_aliases,
                        sample_cols,
                        child_allow,
                        functions,
                    )?;
                }
            }
            if let Some(fe) = filter {
                check_aliased_win_inner(fe, window_aliases, sample_cols, false, functions)?;
            }
            if let Some(WindowRef::Spec(spec)) = over {
                for pe in &spec.partition_by {
                    check_aliased_win_inner(pe, window_aliases, sample_cols, false, functions)?;
                }
                for ob in &spec.order_by {
                    check_aliased_win_inner(
                        &ob.expr,
                        window_aliases,
                        sample_cols,
                        false,
                        functions,
                    )?;
                }
            }
            Ok(())
        }
        Expr::RowValue(es) => {
            for e in es {
                check_aliased_win_inner(e, window_aliases, sample_cols, allow_here, functions)?;
            }
            Ok(())
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            check_aliased_win_in_subquery(sub, window_aliases, functions)
        }
        _ => Ok(()),
    }
}

fn check_aliased_win_in_subquery(
    sub: &SelectStmt,
    window_aliases: &[String],
    functions: &FunctionRegistry,
) -> Result<()> {
    for cte in &sub.ctes {
        check_aliased_win_in_subquery(&cte.query, window_aliases, functions)?;
    }
    check_aliased_win_in_core(&sub.body.head, window_aliases, functions)?;
    for (_, rhs) in &sub.body.compounds {
        check_aliased_win_in_core(rhs, window_aliases, functions)?;
    }
    for ob in &sub.order_by {
        check_aliased_win_inner(&ob.expr, window_aliases, &[], false, functions)?;
    }
    if let Some(ref lim) = sub.limit {
        check_aliased_win_inner(&lim.limit, window_aliases, &[], false, functions)?;
        if let Some(ref off) = lim.offset {
            check_aliased_win_inner(off, window_aliases, &[], false, functions)?;
        }
    }
    Ok(())
}

fn check_aliased_win_in_core(
    core: &SelectCore,
    window_aliases: &[String],
    functions: &FunctionRegistry,
) -> Result<()> {
    match core {
        SelectCore::Values(rows) => {
            for r in rows {
                for e in r {
                    check_aliased_win_inner(e, window_aliases, &[], false, functions)?;
                }
            }
            Ok(())
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
            // Only Check `from: None` or expressions; if `from` has tables, only unshadowed aliases fail,
            // and `subst_select_aliases_in_expr_with_ctes` already filtered shadowed aliases!
            if let Some(fc) = from {
                check_aliased_win_in_from(fc, window_aliases, functions)?;
            }
            let remaining: Vec<String> = window_aliases
                .iter()
                .filter(|al| {
                    !projection.iter().any(|rc| {
                        matches!(
                            rc,
                            ResultColumn::Expr {
                                alias: Some(ia),
                                ..
                            } if ia.eq_ignore_ascii_case(al)
                        )
                    })
                })
                .cloned()
                .collect();
            if remaining.is_empty() {
                return Ok(());
            }
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    check_aliased_win_inner(expr, &remaining, &[], false, functions)?;
                }
            }
            if let Some(w) = selection {
                check_aliased_win_inner(w, &remaining, &[], false, functions)?;
            }
            for g in group_by {
                check_aliased_win_inner(g, &remaining, &[], false, functions)?;
            }
            if let Some(h) = having {
                check_aliased_win_inner(h, &remaining, &[], false, functions)?;
            }
            for (_, ws) in windows {
                for pe in &ws.partition_by {
                    check_aliased_win_inner(pe, &remaining, &[], false, functions)?;
                }
                for ob in &ws.order_by {
                    check_aliased_win_inner(&ob.expr, &remaining, &[], false, functions)?;
                }
            }
            Ok(())
        }
    }
}

fn check_aliased_win_in_from(
    fc: &FromClause,
    window_aliases: &[String],
    functions: &FunctionRegistry,
) -> Result<()> {
    if let TableOrSubquery::Subquery { query, .. } = &fc.head {
        check_aliased_win_in_subquery(query, window_aliases, functions)?;
    }
    for j in &fc.joins {
        if let TableOrSubquery::Subquery { query, .. } = &j.right {
            check_aliased_win_in_subquery(query, window_aliases, functions)?;
        }
        if let Some(JoinConstraint::On(ref on_e)) = j.constraint {
            check_aliased_win_inner(on_e, window_aliases, &[], false, functions)?;
        }
    }
    Ok(())
}

// ============================================================================
// Window Call Collection, Rewriting & Multi-Stage Execution Engine
// ============================================================================

#[derive(Debug, Clone)]
pub(crate) struct CollectedWindowCall {
    pub slot_idx: usize,
    pub fn_kind: WindowFnKind,
    pub args: Vec<Expr>,
    pub filter: Option<Expr>,
    pub spec: ResolvedWindowSpec,
}

/// Collects all window function calls (`over: Some(_)`) at the current `SELECT` level in `expr`,
/// validating each call and replacing it in the returned expression with a synthetic
/// `WIN_SLOT_SCHEMA` column reference (`"\0win".<slot_idx>`).
///
/// Does NOT cross `Subquery` / `Exists` / `InSubquery` boundaries (those belong to nested queries).
pub(crate) fn collect_and_rewrite_window_calls(
    expr: &Expr,
    resolved_defs: &[(String, ResolvedWindowSpec)],
    functions: &FunctionRegistry,
    alias_subst: Option<&dyn Fn(&Expr) -> Expr>,
    out_calls: &mut Vec<CollectedWindowCall>,
) -> Result<Expr> {
    match expr {
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
            over: Some(over_ref),
        } => {
            let (is_star, raw_args) = match args {
                FunctionArgs::Star => (true, &[][..]),
                FunctionArgs::Exprs(es) => (false, es.as_slice()),
            };
            // Nested window calls inside args / filter / OVER are forbidden!
            for ae in raw_args {
                if let Some(nested) = first_window_function_name_in_expr(ae) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of window function {nested}()"),
                    ));
                }
            }
            if let Some(fe) = filter
                && let Some(nested) = first_window_function_name_in_expr(fe)
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("misuse of window function {nested}()"),
                ));
            }
            let fn_kind = classify_and_validate_window_fn(
                name,
                is_star,
                raw_args.len(),
                *distinct,
                functions,
            )?;
            let mut spec = resolve_over_clause(over_ref, resolved_defs)?;
            for pe in &spec.partition_by {
                if let Some(nested) = first_window_function_name_in_expr(pe) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of window function {nested}()"),
                    ));
                }
            }
            for ob in &spec.order_by {
                if let Some(nested) = first_window_function_name_in_expr(&ob.expr) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of window function {nested}()"),
                    ));
                }
            }

            let rewritten_args: Vec<Expr> = match alias_subst {
                Some(sub_fn) => raw_args.iter().map(sub_fn).collect(),
                None => raw_args.to_vec(),
            };
            let rewritten_filter = match (filter.as_deref(), alias_subst) {
                (Some(f), Some(sub_fn)) => Some(sub_fn(f)),
                (Some(f), None) => Some(f.clone()),
                (None, _) => None,
            };
            if let Some(sub_fn) = alias_subst {
                spec.partition_by = spec.partition_by.iter().map(sub_fn).collect();
                spec.order_by = spec
                    .order_by
                    .iter()
                    .map(|ob| OrderByTerm {
                        expr: sub_fn(&ob.expr),
                        desc: ob.desc,
                        nulls_last: ob.nulls_last,
                    })
                    .collect();
            }

            let spec = finalize_window_spec_for_fn(spec, &fn_kind, rewritten_filter.is_some())?;
            let slot_idx = out_calls.len();
            out_calls.push(CollectedWindowCall {
                slot_idx,
                fn_kind,
                args: rewritten_args,
                filter: rewritten_filter,
                spec,
            });
            Ok(Expr::Column(ColumnRef {
                schema: Some(WIN_SLOT_SCHEMA.to_owned()),
                table: None,
                column: slot_idx.to_string(),
            }))
        }
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
            over: None,
        } => {
            // Regular aggregate calls cannot contain window functions (`windowerr-2.1`: `SELECT sum(sum(a) OVER ())`)
            let arg_count = match args {
                FunctionArgs::Star => 0,
                FunctionArgs::Exprs(es) => es.len(),
            };
            let is_star = matches!(args, FunctionArgs::Star);
            let is_agg = if !is_star && functions.find_scalar(name, arg_count).is_some() {
                false
            } else if functions.find_aggregate(name, arg_count).is_some() {
                true
            } else {
                let upper = name.to_ascii_uppercase();
                if (upper == "MIN" || upper == "MAX") && arg_count != 1 {
                    false
                } else if upper == "COUNT" && arg_count == 0 {
                    true
                } else {
                    AggregateKind::from_name(name, is_star).is_some()
                }
            };
            if is_agg {
                if let FunctionArgs::Exprs(es) = args {
                    for ae in es {
                        if let Some(nested) = first_window_function_name_in_expr(ae) {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!("misuse of window function {nested}()"),
                            ));
                        }
                    }
                }
                if let Some(fe) = filter
                    && let Some(nested) = first_window_function_name_in_expr(fe)
                {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of window function {nested}()"),
                    ));
                }
                return Ok(expr.clone());
            }
            let new_args = match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => {
                    let mut out = Vec::with_capacity(es.len());
                    for ae in es {
                        out.push(collect_and_rewrite_window_calls(
                            ae,
                            resolved_defs,
                            functions,
                            alias_subst,
                            out_calls,
                        )?);
                    }
                    FunctionArgs::Exprs(out)
                }
            };
            let new_filter = match filter {
                Some(fe) => Some(Box::new(collect_and_rewrite_window_calls(
                    fe,
                    resolved_defs,
                    functions,
                    alias_subst,
                    out_calls,
                )?)),
                None => None,
            };
            Ok(Expr::FunctionCall {
                name: name.clone(),
                distinct: *distinct,
                args: new_args,
                filter: new_filter,
                over: None,
            })
        }
        Expr::Unary { op, expr: inner } => Ok(Expr::Unary {
            op: *op,
            expr: Box::new(collect_and_rewrite_window_calls(
                inner,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
        }),
        Expr::Collate {
            expr: inner,
            collation,
        } => Ok(Expr::Collate {
            expr: Box::new(collect_and_rewrite_window_calls(
                inner,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            collation: collation.clone(),
        }),
        Expr::IsNull {
            expr: inner,
            negated,
        } => Ok(Expr::IsNull {
            expr: Box::new(collect_and_rewrite_window_calls(
                inner,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            negated: *negated,
        }),
        Expr::Cast {
            expr: inner,
            type_name,
        } => Ok(Expr::Cast {
            expr: Box::new(collect_and_rewrite_window_calls(
                inner,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            type_name: type_name.clone(),
        }),
        Expr::Binary { left, op, right } => Ok(Expr::Binary {
            left: Box::new(collect_and_rewrite_window_calls(
                left,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            op: *op,
            right: Box::new(collect_and_rewrite_window_calls(
                right,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
        }),
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
        } => Ok(Expr::Between {
            expr: Box::new(collect_and_rewrite_window_calls(
                e,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            low: Box::new(collect_and_rewrite_window_calls(
                low,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            high: Box::new(collect_and_rewrite_window_calls(
                high,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            negated: *negated,
        }),
        Expr::InList {
            expr: e,
            list,
            negated,
        } => {
            let new_e = collect_and_rewrite_window_calls(
                e,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?;
            let mut new_list = Vec::with_capacity(list.len());
            for item in list {
                new_list.push(collect_and_rewrite_window_calls(
                    item,
                    resolved_defs,
                    functions,
                    alias_subst,
                    out_calls,
                )?);
            }
            Ok(Expr::InList {
                expr: Box::new(new_e),
                list: new_list,
                negated: *negated,
            })
        }
        Expr::InTable {
            expr: e,
            table,
            negated,
        } => Ok(Expr::InTable {
            expr: Box::new(collect_and_rewrite_window_calls(
                e,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            table: table.clone(),
            negated: *negated,
        }),
        Expr::InSubquery {
            expr: e,
            subquery,
            negated,
        } => Ok(Expr::InSubquery {
            expr: Box::new(collect_and_rewrite_window_calls(
                e,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            subquery: subquery.clone(),
            negated: *negated,
        }),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            op,
            negated,
        } => Ok(Expr::Like {
            expr: Box::new(collect_and_rewrite_window_calls(
                e,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            pattern: Box::new(collect_and_rewrite_window_calls(
                pattern,
                resolved_defs,
                functions,
                alias_subst,
                out_calls,
            )?),
            escape: match escape {
                Some(esc) => Some(Box::new(collect_and_rewrite_window_calls(
                    esc,
                    resolved_defs,
                    functions,
                    alias_subst,
                    out_calls,
                )?)),
                None => None,
            },
            op: *op,
            negated: *negated,
        }),
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            let new_op = match operand {
                Some(o) => Some(Box::new(collect_and_rewrite_window_calls(
                    o,
                    resolved_defs,
                    functions,
                    alias_subst,
                    out_calls,
                )?)),
                None => None,
            };
            let mut new_branches = Vec::with_capacity(branches.len());
            for (w, t) in branches {
                new_branches.push((
                    collect_and_rewrite_window_calls(
                        w,
                        resolved_defs,
                        functions,
                        alias_subst,
                        out_calls,
                    )?,
                    collect_and_rewrite_window_calls(
                        t,
                        resolved_defs,
                        functions,
                        alias_subst,
                        out_calls,
                    )?,
                ));
            }
            let new_else = match else_expr {
                Some(el) => Some(Box::new(collect_and_rewrite_window_calls(
                    el,
                    resolved_defs,
                    functions,
                    alias_subst,
                    out_calls,
                )?)),
                None => None,
            };
            Ok(Expr::Case {
                operand: new_op,
                branches: new_branches,
                else_expr: new_else,
            })
        }
        Expr::RowValue(elems) => {
            let mut out = Vec::with_capacity(elems.len());
            for el in elems {
                out.push(collect_and_rewrite_window_calls(
                    el,
                    resolved_defs,
                    functions,
                    alias_subst,
                    out_calls,
                )?);
            }
            Ok(Expr::RowValue(out))
        }
        _ => Ok(expr.clone()),
    }
}

/// One row (or post-`GROUP BY` group) flowing through the multi-stage window pipeline.
pub(crate) struct WindowExecItem {
    pub row: EvalRow,
    pub group_rows: Option<Vec<EvalRow>>,
    pub win_vals: Vec<RuntimeValue>,
}

struct WindowStage {
    partition_by: Vec<Expr>,
    order_by: Vec<OrderByTerm>,
    units: WindowFrameUnits,
    start: WindowFrameBound,
    end: WindowFrameBound,
    exclude: WindowFrameExclude,
    calls: Vec<CollectedWindowCall>,
}

fn specs_share_stage(a: &ResolvedWindowSpec, b: &ResolvedWindowSpec) -> bool {
    a.partition_by == b.partition_by
        && a.order_by == b.order_by
        && a.units == b.units
        && a.start == b.start
        && a.end == b.end
        && a.exclude == b.exclude
}

/// Groups collected window calls into stages matching `sqlite3WindowLink` (`window.c:1338-1386`)
/// and executes them from innermost (`stages.last()`) to outermost (`stages[0]`), populating
/// `item.row.win_vals` on every item.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_window_pipeline(
    ctx: &mut ExecContext<'_>,
    items: &mut [WindowExecItem],
    calls: &[CollectedWindowCall],
    sample_cols: &[ScopedColumn],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<()> {
    let total_calls = calls.len();
    if total_calls == 0 || items.is_empty() {
        return Ok(());
    }
    for item in items.iter_mut() {
        item.win_vals = vec![RuntimeValue::null(); total_calls];
    }

    let mut stages: Vec<WindowStage> = Vec::new();
    for call in calls {
        if let Some(st) = stages.iter_mut().find(|st| {
            let st_spec = ResolvedWindowSpec {
                partition_by: st.partition_by.clone(),
                order_by: st.order_by.clone(),
                units: st.units,
                start: st.start.clone(),
                end: st.end.clone(),
                exclude: st.exclude,
                implicit_frame: false,
            };
            specs_share_stage(&st_spec, &call.spec)
        }) {
            st.calls.push(call.clone());
        } else {
            stages.push(WindowStage {
                partition_by: call.spec.partition_by.clone(),
                order_by: call.spec.order_by.clone(),
                units: call.spec.units,
                start: call.spec.start.clone(),
                end: call.spec.end.clone(),
                exclude: call.spec.exclude,
                calls: vec![call.clone()],
            });
        }
    }

    // Execute innermost stage first (`stages.last()`) up to outermost (`stages[0]`)
    for (rev_idx, stage) in stages.iter().rev().enumerate() {
        if rev_idx > 0 {
            for item in items.iter_mut() {
                for wv in item.win_vals.iter_mut() {
                    wv.subtype = 0;
                }
            }
        }
        execute_single_window_stage(ctx, items, stage, sample_cols, params, outer_scopes, ctes)?;
    }

    for item in items.iter_mut() {
        item.row.win_vals = Rc::from(item.win_vals.as_slice());
    }
    Ok(())
}

#[derive(Clone)]
struct SortTermMeta {
    order: KeySortOrder,
    custom_collation: Option<String>,
}

impl SortTermMeta {
    fn from_expr_asc(expr: &Expr, sample_cols: &[ScopedColumn]) -> Self {
        let custom_collation = extract_custom_collation(expr)
            .or_else(|| infer_expr_custom_collation_from_cols_pub(expr, sample_cols));
        let coll = infer_expr_collation_from_cols_pub(expr, sample_cols);
        Self {
            order: KeySortOrder {
                desc: false,
                nulls_last: false,
                collation: coll,
                custom_collation: None,
            },
            custom_collation,
        }
    }

    fn from_order_by_term(ob: &OrderByTerm, sample_cols: &[ScopedColumn]) -> Self {
        let custom_collation = extract_custom_collation(&ob.expr)
            .or_else(|| infer_expr_custom_collation_from_cols_pub(&ob.expr, sample_cols));
        let coll = infer_expr_collation_from_cols_pub(&ob.expr, sample_cols);
        Self {
            order: KeySortOrder {
                desc: ob.desc,
                nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                collation: coll,
                custom_collation: None,
            },
            custom_collation,
        }
    }

    fn validate_collation(&self, collations: &CollationRegistry) -> Result<()> {
        if let Some(ref cname) = self.custom_collation
            && !collations.has_collation(cname)
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {cname}"),
            ));
        }
        Ok(())
    }

    fn compare_vals(
        &self,
        a: &Value,
        b: &Value,
        collations: &CollationRegistry,
        enc: TextEncoding,
    ) -> Result<Ordering> {
        match (a.is_null(), b.is_null()) {
            (true, true) => Ok(Ordering::Equal),
            (true, false) => Ok(if self.order.nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            }),
            (false, true) => Ok(if self.order.nulls_last {
                Ordering::Less
            } else {
                Ordering::Greater
            }),
            (false, false) => {
                let raw = if let (Some(cname), Value::Text(ls), Value::Text(rs)) =
                    (self.custom_collation.as_deref(), a, b)
                {
                    collations.compare(cname, ls, rs, enc)?
                } else {
                    a.cmp_sqlite_with_encoding(b, self.order.collation, enc)
                };
                Ok(if self.order.desc { raw.reverse() } else { raw })
            }
        }
    }
}

fn extract_custom_collation(mut expr: &Expr) -> Option<String> {
    while let Expr::Unary {
        op: UnaryOp::Plus,
        expr: inner,
    } = expr
    {
        expr = inner;
    }
    if let Expr::Collate { collation, .. } = expr
        && Collation::from_name(collation).is_none()
    {
        return Some(collation.clone());
    }
    None
}

fn extract_effective_collation_name(expr: &Expr, sample_cols: &[ScopedColumn]) -> String {
    if let Some(custom) = extract_custom_collation(expr)
        .or_else(|| infer_expr_custom_collation_from_cols_pub(expr, sample_cols))
    {
        return custom;
    }
    match infer_expr_collation_from_cols_pub(expr, sample_cols) {
        Collation::Binary => "BINARY".to_owned(),
        Collation::NoCase => "NOCASE".to_owned(),
        Collation::RTrim => "RTRIM".to_owned(),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_single_window_stage(
    ctx: &mut ExecContext<'_>,
    items: &mut [WindowExecItem],
    stage: &WindowStage,
    sample_cols: &[ScopedColumn],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<()> {
    let part_metas: Vec<SortTermMeta> = stage
        .partition_by
        .iter()
        .map(|pe| SortTermMeta::from_expr_asc(pe, sample_cols))
        .collect();
    let order_metas: Vec<SortTermMeta> = stage
        .order_by
        .iter()
        .map(|ob| SortTermMeta::from_order_by_term(ob, sample_cols))
        .collect();
    for m in part_metas.iter().chain(order_metas.iter()) {
        m.validate_collation(ctx.collations)?;
    }

    // Evaluate `PARTITION BY` and `ORDER BY` keys per item
    let n_total = items.len();
    let mut keys: Vec<(Vec<Value>, Vec<Value>)> = Vec::with_capacity(n_total);
    for item in items.iter_mut() {
        ctx.step_tick()?;
        item.row.win_vals = Rc::from(item.win_vals.as_slice());
        let scopes = push_scope_pub(outer_scopes, &item.row);
        let agg_grp = item.group_rows.as_deref().map(|gr| (gr, outer_scopes));

        let mut p_vals = Vec::with_capacity(stage.partition_by.len());
        for pe in &stage.partition_by {
            p_vals.push(eval_expr_pub(ctx, pe, params, &scopes, ctes, agg_grp)?);
        }
        let mut o_vals = Vec::with_capacity(stage.order_by.len());
        for ob in &stage.order_by {
            o_vals.push(eval_expr_pub(
                ctx, &ob.expr, params, &scopes, ctes, agg_grp,
            )?);
        }
        keys.push((p_vals, o_vals));
    }

    // Stably sort `(items, keys)` ONLY when `PARTITION BY` or `ORDER BY` is non-empty!
    let enc = ctx.encoding();
    if !stage.partition_by.is_empty() || !stage.order_by.is_empty() {
        let mut perm: Vec<usize> = (0..n_total).collect();
        let collations = ctx.collations;
        fallible_sort_by(&mut perm, |&ia, &ib| {
            let (pa, oa) = &keys[ia];
            let (pb, ob) = &keys[ib];
            for (k, pm) in part_metas.iter().enumerate() {
                let ord = pm.compare_vals(&pa[k], &pb[k], collations, enc)?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            for (k, om) in order_metas.iter().enumerate() {
                let ord = om.compare_vals(&oa[k], &ob[k], collations, enc)?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            Ok(Ordering::Equal)
        })?;
        apply_permutation_in_place(items, &perm);
        apply_permutation_in_place(&mut keys, &perm);
    }

    // Split into contiguous partitions
    let mut part_start = 0usize;
    while part_start < n_total {
        let mut part_end = part_start + 1;
        if !stage.partition_by.is_empty() {
            while part_end < n_total {
                let mut same_part = true;
                for (k, pm) in part_metas.iter().enumerate() {
                    if pm.compare_vals(
                        &keys[part_start].0[k],
                        &keys[part_end].0[k],
                        ctx.collations,
                        enc,
                    )? != Ordering::Equal
                    {
                        same_part = false;
                        break;
                    }
                }
                if !same_part {
                    break;
                }
                part_end += 1;
            }
        } else {
            part_end = n_total;
        }

        execute_stage_on_partition(
            ctx,
            &mut items[part_start..part_end],
            &keys[part_start..part_end],
            stage,
            &order_metas,
            sample_cols,
            params,
            outer_scopes,
            ctes,
        )?;
        part_start = part_end;
    }

    Ok(())
}

fn apply_permutation_in_place<T>(slice: &mut [T], perm: &[usize]) {
    let n = slice.len();
    let mut visited = vec![false; n];
    for i in 0..n {
        if visited[i] || perm[i] == i {
            visited[i] = true;
            continue;
        }
        let mut cur = i;
        loop {
            let next = perm[cur];
            visited[cur] = true;
            if next == i {
                break;
            }
            slice.swap(cur, next);
            cur = next;
        }
    }
}

#[derive(Clone)]
enum EvalFrameBound {
    UnboundedPreceding,
    PrecedingInt(i64),
    PrecedingNum(Value),
    CurrentRow,
    FollowingInt(i64),
    FollowingNum(Value),
    UnboundedFollowing,
}

fn must_be_int_val(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Real(r) => {
            if r.is_finite()
                && *r >= (i64::MIN as f64)
                && *r < 9223372036854775808.0
                && (*r as i64 as f64) == *r
            {
                Some(*r as i64)
            } else {
                None
            }
        }
        Value::Text(s) => match sqliters_types::parse_numeric_text(s) {
            Some(Value::Integer(i)) => Some(i),
            Some(Value::Real(r))
                if r.is_finite()
                    && r >= (i64::MIN as f64)
                    && r < 9223372036854775808.0
                    && (r as i64 as f64) == r =>
            {
                Some(r as i64)
            }
            _ => None,
        },
        Value::Null | Value::Blob(_) => None,
    }
}

fn must_be_non_negative_number(v: &Value) -> Option<Value> {
    match v {
        Value::Integer(i) if *i >= 0 => Some(Value::Integer(*i)),
        Value::Real(r) if *r >= 0.0 => Some(Value::Real(*r)),
        Value::Text(s) => match sqliters_types::parse_numeric_text(s) {
            Some(Value::Integer(i)) if i >= 0 => Some(Value::Integer(i)),
            Some(Value::Real(r)) if r >= 0.0 => Some(Value::Real(r)),
            _ => None,
        },
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_bound_on_first_row(
    ctx: &mut ExecContext<'_>,
    bound: &WindowFrameBound,
    units: WindowFrameUnits,
    is_start: bool,
    first_item: &WindowExecItem,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<EvalFrameBound> {
    let eval_raw = |ctx: &mut ExecContext<'_>, expr: &Expr| -> Result<Value> {
        if !is_window_bound_constant(expr) {
            return Ok(Value::Null);
        }
        let scopes = push_scope_pub(outer_scopes, &first_item.row);
        let agg_grp = first_item
            .group_rows
            .as_deref()
            .map(|gr| (gr, outer_scopes));
        eval_expr_pub(ctx, expr, params, &scopes, ctes, agg_grp)
    };

    match bound {
        WindowFrameBound::UnboundedPreceding => Ok(EvalFrameBound::UnboundedPreceding),
        WindowFrameBound::CurrentRow => Ok(EvalFrameBound::CurrentRow),
        WindowFrameBound::UnboundedFollowing => Ok(EvalFrameBound::UnboundedFollowing),
        WindowFrameBound::Preceding(expr) => {
            let v = eval_raw(ctx, expr)?;
            if units == WindowFrameUnits::Range {
                let num = must_be_non_negative_number(&v).ok_or_else(|| {
                    SqliteError::new(
                        ErrorCode::Error,
                        if is_start {
                            "frame starting offset must be a non-negative number"
                        } else {
                            "frame ending offset must be a non-negative number"
                        },
                    )
                })?;
                Ok(EvalFrameBound::PrecedingNum(num))
            } else {
                let k = must_be_int_val(&v).filter(|&k| k >= 0).ok_or_else(|| {
                    SqliteError::new(
                        ErrorCode::Error,
                        if is_start {
                            "frame starting offset must be a non-negative integer"
                        } else {
                            "frame ending offset must be a non-negative integer"
                        },
                    )
                })?;
                Ok(EvalFrameBound::PrecedingInt(k))
            }
        }
        WindowFrameBound::Following(expr) => {
            let v = eval_raw(ctx, expr)?;
            if units == WindowFrameUnits::Range {
                let num = must_be_non_negative_number(&v).ok_or_else(|| {
                    SqliteError::new(
                        ErrorCode::Error,
                        if is_start {
                            "frame starting offset must be a non-negative number"
                        } else {
                            "frame ending offset must be a non-negative number"
                        },
                    )
                })?;
                Ok(EvalFrameBound::FollowingNum(num))
            } else {
                let k = must_be_int_val(&v).filter(|&k| k >= 0).ok_or_else(|| {
                    SqliteError::new(
                        ErrorCode::Error,
                        if is_start {
                            "frame starting offset must be a non-negative integer"
                        } else {
                            "frame ending offset must be a non-negative integer"
                        },
                    )
                })?;
                Ok(EvalFrameBound::FollowingInt(k))
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RangeTestOp {
    Ge,
    Gt,
    Le,
    Lt,
}

fn vdbe_nulleq_cmp(
    op: RangeTestOp,
    lhs: &Value,
    rhs: &Value,
    meta: &SortTermMeta,
    collations: &CollationRegistry,
    enc: TextEncoding,
) -> Result<bool> {
    let ord = match (lhs.is_null(), rhs.is_null()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => {
            if let (Some(cname), Value::Text(ls), Value::Text(rs)) =
                (meta.custom_collation.as_deref(), lhs, rhs)
            {
                collations.compare(cname, ls, rs, enc)?
            } else {
                lhs.cmp_sqlite_with_encoding(rhs, meta.order.collation, enc)
            }
        }
    };
    Ok(match op {
        RangeTestOp::Ge => ord.is_ge(),
        RangeTestOp::Gt => ord.is_gt(),
        RangeTestOp::Le => ord.is_le(),
        RangeTestOp::Lt => ord.is_lt(),
    })
}

fn vdbe_numeric_add_or_sub(val: &Value, offset: &Value, sub: bool) -> Value {
    if val.is_null() || offset.is_null() {
        return Value::Null;
    }
    if let (Value::Integer(a), Value::Integer(b)) = (val, offset) {
        let checked = if sub {
            a.checked_sub(*b)
        } else {
            a.checked_add(*b)
        };
        if let Some(res) = checked {
            return Value::Integer(res);
        }
    }
    let af = val.to_real_lossy();
    let bf = offset.to_real_lossy();
    let r = if sub { af - bf } else { af + bf };
    if r.is_nan() {
        Value::Null
    } else {
        Value::Real(r)
    }
}

/// Exact translation of `windowCodeRangeTest` (`sqlite/src/window.c:2107-2231`).
fn window_code_range_test(
    mut op: RangeTestOp,
    val1: &Value,
    offset: &Value,
    val2: &Value,
    meta: &SortTermMeta,
    collations: &CollationRegistry,
    enc: TextEncoding,
) -> Result<bool> {
    let desc = meta.order.desc;
    let bignull = meta.order.desc != meta.order.nulls_last;
    let mut arith_sub = false;
    if desc {
        op = match op {
            RangeTestOp::Ge => RangeTestOp::Le,
            RangeTestOp::Gt => RangeTestOp::Lt,
            RangeTestOp::Le => RangeTestOp::Ge,
            RangeTestOp::Lt => RangeTestOp::Gt,
        };
        arith_sub = true;
    }

    if bignull {
        if val1.is_null() {
            return Ok(match op {
                RangeTestOp::Ge => true,
                RangeTestOp::Gt => !val2.is_null(),
                RangeTestOp::Le => val2.is_null(),
                RangeTestOp::Lt => false,
            });
        } else if val2.is_null() {
            return Ok(matches!(op, RangeTestOp::Le | RangeTestOp::Lt));
        }
    }

    let reg1 = if matches!(val1, Value::Text(_) | Value::Blob(_)) {
        val1.clone()
    } else {
        if ((op == RangeTestOp::Ge && !arith_sub) || (op == RangeTestOp::Le && arith_sub))
            && vdbe_nulleq_cmp(op, val1, val2, meta, collations, enc)?
        {
            return Ok(true);
        }
        vdbe_numeric_add_or_sub(val1, offset, arith_sub)
    };

    vdbe_nulleq_cmp(op, &reg1, val2, meta, collations, enc)
}

fn vdbe_rowid_offset_target(base_rowid_1based: i64, off_val: &Value, is_lead: bool) -> Option<i64> {
    if off_val.is_null() {
        return None;
    }
    let off_num = match off_val {
        Value::Integer(i) => Value::Integer(*i),
        Value::Real(r) => Value::Real(*r),
        Value::Text(s) => sqliters_types::parse_numeric_text(s).unwrap_or(Value::Integer(0)),
        Value::Blob(_) => Value::Integer(0),
        Value::Null => return None,
    };
    let target = vdbe_numeric_add_or_sub(&Value::Integer(base_rowid_1based), &off_num, !is_lead);
    must_be_int_val(&target)
}

#[allow(clippy::too_many_arguments)]
fn execute_stage_on_partition(
    ctx: &mut ExecContext<'_>,
    part_items: &mut [WindowExecItem],
    part_keys: &[(Vec<Value>, Vec<Value>)],
    stage: &WindowStage,
    order_metas: &[SortTermMeta],
    sample_cols: &[ScopedColumn],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<()> {
    let n = part_items.len();
    if n == 0 {
        return Ok(());
    }
    let enc = ctx.encoding();

    // 1. Evaluate frame start/end offsets on row 0 of the partition (`window.c:2945-2952`)
    let eval_start = eval_bound_on_first_row(
        ctx,
        &stage.start,
        stage.units,
        true,
        &part_items[0],
        params,
        outer_scopes,
        ctes,
    )?;
    let eval_end = eval_bound_on_first_row(
        ctx,
        &stage.end,
        stage.units,
        false,
        &part_items[0],
        params,
        outer_scopes,
        ctes,
    )?;

    // 2. Build peer groups `(g_start, g_end)` and `row_peer_group[0..n]`
    let mut peer_groups: Vec<(usize, usize)> = Vec::new();
    let mut row_peer_group: Vec<usize> = vec![0; n];
    if stage.order_by.is_empty() {
        peer_groups.push((0, n));
    } else {
        let mut g_start = 0usize;
        while g_start < n {
            let g_id = peer_groups.len();
            row_peer_group[g_start] = g_id;
            let mut g_end = g_start + 1;
            while g_end < n {
                let mut same_peer = true;
                for (k, om) in order_metas.iter().enumerate() {
                    if om.compare_vals(
                        &part_keys[g_start].1[k],
                        &part_keys[g_end].1[k],
                        ctx.collations,
                        enc,
                    )? != Ordering::Equal
                    {
                        same_peer = false;
                        break;
                    }
                }
                if !same_peer {
                    break;
                }
                row_peer_group[g_end] = g_id;
                g_end += 1;
            }
            peer_groups.push((g_start, g_end));
            g_start = g_end;
        }
    }
    let num_groups = peer_groups.len();

    // 3. Compute half-open `[start_idx, end_idx)` frame intervals (`0 <= start <= end <= n`)
    let mut frames: Vec<(usize, usize)> = Vec::with_capacity(n);
    match stage.units {
        WindowFrameUnits::Rows => {
            for i in 0..n {
                let s = match eval_start {
                    EvalFrameBound::UnboundedPreceding => 0,
                    EvalFrameBound::PrecedingInt(k) => {
                        (i as i128 - i128::from(k)).clamp(0, n as i128) as usize
                    }
                    EvalFrameBound::CurrentRow => i,
                    EvalFrameBound::FollowingInt(k) => {
                        (i as i128 + i128::from(k)).clamp(0, n as i128) as usize
                    }
                    _ => unreachable!(),
                };
                let e = match eval_end {
                    EvalFrameBound::PrecedingInt(k) => {
                        (i as i128 - i128::from(k) + 1).clamp(0, n as i128) as usize
                    }
                    EvalFrameBound::CurrentRow => i + 1,
                    EvalFrameBound::FollowingInt(k) => {
                        (i as i128 + i128::from(k) + 1).clamp(0, n as i128) as usize
                    }
                    EvalFrameBound::UnboundedFollowing => n,
                    _ => unreachable!(),
                };
                frames.push((s, e.max(s)));
            }
        }
        WindowFrameUnits::Groups => {
            for &g in row_peer_group.iter().take(n) {
                let sg = match eval_start {
                    EvalFrameBound::UnboundedPreceding => 0,
                    EvalFrameBound::PrecedingInt(k) => {
                        (g as i128 - i128::from(k)).clamp(0, num_groups as i128) as usize
                    }
                    EvalFrameBound::CurrentRow => g,
                    EvalFrameBound::FollowingInt(k) => {
                        (g as i128 + i128::from(k)).clamp(0, num_groups as i128) as usize
                    }
                    _ => unreachable!(),
                };
                let eg = match eval_end {
                    EvalFrameBound::PrecedingInt(k) => {
                        (g as i128 - i128::from(k) + 1).clamp(0, num_groups as i128) as usize
                    }
                    EvalFrameBound::CurrentRow => g + 1,
                    EvalFrameBound::FollowingInt(k) => {
                        (g as i128 + i128::from(k) + 1).clamp(0, num_groups as i128) as usize
                    }
                    EvalFrameBound::UnboundedFollowing => num_groups,
                    _ => unreachable!(),
                };
                if eg <= sg {
                    let anchor = if sg < num_groups {
                        peer_groups[sg].0
                    } else {
                        n
                    };
                    frames.push((anchor, anchor));
                } else {
                    frames.push((peer_groups[sg].0, peer_groups[eg - 1].1));
                }
            }
        }
        WindowFrameUnits::Range => {
            let mut cur_sg = 0usize;
            let mut cur_eg = 0usize;
            for g in 0..num_groups {
                let curr_row_idx = peer_groups[g].0;
                let sg = match &eval_start {
                    EvalFrameBound::UnboundedPreceding => 0,
                    EvalFrameBound::CurrentRow => g,
                    EvalFrameBound::PrecedingNum(off) => {
                        let om = &order_metas[0];
                        let curr_v = &part_keys[curr_row_idx].1[0];
                        while cur_sg < num_groups {
                            let cand_v = &part_keys[peer_groups[cur_sg].0].1[0];
                            if window_code_range_test(
                                RangeTestOp::Ge,
                                cand_v,
                                off,
                                curr_v,
                                om,
                                ctx.collations,
                                enc,
                            )? {
                                break;
                            }
                            cur_sg += 1;
                        }
                        cur_sg
                    }
                    EvalFrameBound::FollowingNum(off) => {
                        let om = &order_metas[0];
                        let curr_v = &part_keys[curr_row_idx].1[0];
                        while cur_sg < num_groups {
                            let cand_v = &part_keys[peer_groups[cur_sg].0].1[0];
                            if window_code_range_test(
                                RangeTestOp::Le,
                                curr_v,
                                off,
                                cand_v,
                                om,
                                ctx.collations,
                                enc,
                            )? {
                                break;
                            }
                            cur_sg += 1;
                        }
                        cur_sg
                    }
                    _ => unreachable!(),
                };
                let eg = match &eval_end {
                    EvalFrameBound::CurrentRow => g + 1,
                    EvalFrameBound::UnboundedFollowing => num_groups,
                    EvalFrameBound::PrecedingNum(off) => {
                        let om = &order_metas[0];
                        let curr_v = &part_keys[curr_row_idx].1[0];
                        while cur_eg < num_groups {
                            let cand_v = &part_keys[peer_groups[cur_eg].0].1[0];
                            if window_code_range_test(
                                RangeTestOp::Gt,
                                cand_v,
                                off,
                                curr_v,
                                om,
                                ctx.collations,
                                enc,
                            )? {
                                break;
                            }
                            cur_eg += 1;
                        }
                        cur_eg
                    }
                    EvalFrameBound::FollowingNum(off) => {
                        let om = &order_metas[0];
                        let curr_v = &part_keys[curr_row_idx].1[0];
                        while cur_eg < num_groups {
                            let cand_v = &part_keys[peer_groups[cur_eg].0].1[0];
                            if !window_code_range_test(
                                RangeTestOp::Ge,
                                curr_v,
                                off,
                                cand_v,
                                om,
                                ctx.collations,
                                enc,
                            )? {
                                break;
                            }
                            cur_eg += 1;
                        }
                        cur_eg
                    }
                    _ => unreachable!(),
                };
                let frame_pair = if eg <= sg {
                    let anchor = if sg < num_groups {
                        peer_groups[sg].0
                    } else {
                        n
                    };
                    (anchor, anchor)
                } else {
                    (peer_groups[sg].0, peer_groups[eg - 1].1)
                };
                for _ in peer_groups[g].0..peer_groups[g].1 {
                    frames.push(frame_pair);
                }
            }
        }
    }

    // 4. Evaluate each window call in this stage across `part_items[0..n]`
    for call in &stage.calls {
        evaluate_call_on_partition(
            ctx,
            part_items,
            &peer_groups,
            &row_peer_group,
            &frames,
            stage.exclude,
            &stage.start,
            call,
            sample_cols,
            params,
            outer_scopes,
            ctes,
        )?;
    }

    Ok(())
}

#[inline]
fn row_is_excluded(
    j: usize,
    curr_i: usize,
    exclude: WindowFrameExclude,
    row_peer_group: &[usize],
) -> bool {
    match exclude {
        WindowFrameExclude::NoOthers => false,
        WindowFrameExclude::CurrentRow => j == curr_i,
        WindowFrameExclude::Group => row_peer_group[j] == row_peer_group[curr_i],
        WindowFrameExclude::Ties => j != curr_i && row_peer_group[j] == row_peer_group[curr_i],
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_call_on_partition(
    ctx: &mut ExecContext<'_>,
    part_items: &mut [WindowExecItem],
    peer_groups: &[(usize, usize)],
    row_peer_group: &[usize],
    frames: &[(usize, usize)],
    exclude: WindowFrameExclude,
    start_bound: &WindowFrameBound,
    call: &CollectedWindowCall,
    sample_cols: &[ScopedColumn],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<()> {
    let n = part_items.len();
    let slot = call.slot_idx;

    match &call.fn_kind {
        WindowFnKind::Builtin(bfn) => match bfn {
            BuiltinWinFn::RowNumber => {
                for (i, item) in part_items.iter_mut().enumerate() {
                    ctx.step_tick()?;
                    item.win_vals[slot] = RuntimeValue::plain(Value::Integer((i + 1) as i64));
                }
            }
            BuiltinWinFn::Rank => {
                for (i, item) in part_items.iter_mut().enumerate() {
                    ctx.step_tick()?;
                    let g_start = peer_groups[row_peer_group[i]].0;
                    item.win_vals[slot] = RuntimeValue::plain(Value::Integer((g_start + 1) as i64));
                }
            }
            BuiltinWinFn::DenseRank => {
                for (i, item) in part_items.iter_mut().enumerate() {
                    ctx.step_tick()?;
                    let dr = (row_peer_group[i] + 1) as i64;
                    item.win_vals[slot] = RuntimeValue::plain(Value::Integer(dr));
                }
            }
            BuiltinWinFn::PercentRank => {
                for (i, item) in part_items.iter_mut().enumerate() {
                    ctx.step_tick()?;
                    let pr = if n <= 1 {
                        0.0
                    } else {
                        (peer_groups[row_peer_group[i]].0 as f64) / ((n - 1) as f64)
                    };
                    item.win_vals[slot] = RuntimeValue::plain(Value::Real(pr));
                }
            }
            BuiltinWinFn::CumeDist => {
                for (i, item) in part_items.iter_mut().enumerate() {
                    ctx.step_tick()?;
                    let g_end = peer_groups[row_peer_group[i]].1;
                    let cd = (g_end as f64) / (n as f64);
                    item.win_vals[slot] = RuntimeValue::plain(Value::Real(cd));
                }
            }
            BuiltinWinFn::Ntile => {
                let first_scopes = push_scope_pub(outer_scopes, &part_items[0].row);
                let first_agg = part_items[0]
                    .group_rows
                    .as_deref()
                    .map(|gr| (gr, outer_scopes));
                let n_val =
                    eval_expr_pub(ctx, &call.args[0], params, &first_scopes, ctes, first_agg)?;
                let n_param = n_val.to_integer_lossy();
                if n_param <= 0 {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "argument of ntile must be a positive integer",
                    ));
                }
                let n_total = n as i64;
                let n_size = n_total / n_param;
                let n_large = n_total - n_param * n_size;
                let i_small = n_large * (n_size + 1);
                for (i, item) in part_items.iter_mut().enumerate() {
                    ctx.step_tick()?;
                    let i_row = i as i64;
                    let bucket = if i_row < i_small {
                        1 + i_row / (n_size + 1)
                    } else {
                        1 + n_large + (i_row - i_small) / n_size
                    };
                    item.win_vals[slot] = RuntimeValue::plain(Value::Integer(bucket));
                }
            }
            BuiltinWinFn::Lag | BuiltinWinFn::Lead => {
                let is_lead = *bfn == BuiltinWinFn::Lead;
                let mut arg0_vals: Vec<Value> = Vec::with_capacity(n);
                for item in part_items.iter() {
                    ctx.step_tick()?;
                    let scopes = push_scope_pub(outer_scopes, &item.row);
                    let agg_grp = item.group_rows.as_deref().map(|gr| (gr, outer_scopes));
                    arg0_vals.push(eval_expr_pub(
                        ctx,
                        &call.args[0],
                        params,
                        &scopes,
                        ctes,
                        agg_grp,
                    )?);
                }
                let mut results: Vec<Value> = Vec::with_capacity(n);
                for (i, item) in part_items.iter().enumerate() {
                    ctx.step_tick()?;
                    let scopes = push_scope_pub(outer_scopes, &item.row);
                    let agg_grp = item.group_rows.as_deref().map(|gr| (gr, outer_scopes));
                    let def_val = if call.args.len() >= 3 {
                        eval_expr_pub(ctx, &call.args[2], params, &scopes, ctes, agg_grp)?
                    } else {
                        Value::Null
                    };
                    let base_rowid = (i + 1) as i64;
                    let target_opt = if call.args.len() >= 2 {
                        let off_v =
                            eval_expr_pub(ctx, &call.args[1], params, &scopes, ctes, agg_grp)?;
                        vdbe_rowid_offset_target(base_rowid, &off_v, is_lead)
                    } else {
                        Some(base_rowid + if is_lead { 1 } else { -1 })
                    };
                    let val = match target_opt {
                        Some(t) if t >= 1 && (t as usize) <= n => {
                            arg0_vals[(t as usize) - 1].clone()
                        }
                        _ => def_val,
                    };
                    results.push(val);
                }
                for (i, v) in results.into_iter().enumerate() {
                    part_items[i].win_vals[slot] = RuntimeValue::plain(v);
                }
            }
            BuiltinWinFn::FirstValue | BuiltinWinFn::LastValue => {
                let is_first = *bfn == BuiltinWinFn::FirstValue;
                let mut arg0_vals: Vec<Value> = Vec::with_capacity(n);
                for item in part_items.iter() {
                    ctx.step_tick()?;
                    let scopes = push_scope_pub(outer_scopes, &item.row);
                    let agg_grp = item.group_rows.as_deref().map(|gr| (gr, outer_scopes));
                    arg0_vals.push(eval_expr_pub(
                        ctx,
                        &call.args[0],
                        params,
                        &scopes,
                        ctes,
                        agg_grp,
                    )?);
                }
                for (i, item) in part_items.iter_mut().enumerate() {
                    ctx.step_tick()?;
                    let (s, e) = frames[i];
                    let chosen = if is_first {
                        (s..e).find(|&j| !row_is_excluded(j, i, exclude, row_peer_group))
                    } else {
                        (s..e)
                            .rev()
                            .find(|&j| !row_is_excluded(j, i, exclude, row_peer_group))
                    };
                    let val = chosen.map_or(Value::Null, |j| arg0_vals[j].clone());
                    item.win_vals[slot] = RuntimeValue::plain(val);
                }
            }
            BuiltinWinFn::NthValue => {
                let mut arg0_vals: Vec<Value> = Vec::with_capacity(n);
                let mut arg1_vals: Vec<Value> = Vec::with_capacity(n);
                for item in part_items.iter() {
                    ctx.step_tick()?;
                    let scopes = push_scope_pub(outer_scopes, &item.row);
                    let agg_grp = item.group_rows.as_deref().map(|gr| (gr, outer_scopes));
                    arg0_vals.push(eval_expr_pub(
                        ctx,
                        &call.args[0],
                        params,
                        &scopes,
                        ctes,
                        agg_grp,
                    )?);
                    arg1_vals.push(eval_expr_pub(
                        ctx,
                        &call.args[1],
                        params,
                        &scopes,
                        ctes,
                        agg_grp,
                    )?);
                }
                for (i, item) in part_items.iter_mut().enumerate() {
                    ctx.step_tick()?;
                    let (s, e) = frames[i];
                    let has_unexcluded =
                        (s..e).any(|j| !row_is_excluded(j, i, exclude, row_peer_group));
                    if exclude == WindowFrameExclude::NoOthers || has_unexcluded {
                        let k = must_be_int_val(&arg1_vals[i])
                            .filter(|&k| k >= 1)
                            .ok_or_else(|| {
                                SqliteError::new(
                                    ErrorCode::Error,
                                    "second argument to nth_value must be a positive integer",
                                )
                            })?;
                        let chosen = (s..e)
                            .filter(|&j| !row_is_excluded(j, i, exclude, row_peer_group))
                            .nth((k - 1) as usize);
                        item.win_vals[slot] = RuntimeValue::plain(
                            chosen.map_or(Value::Null, |j| arg0_vals[j].clone()),
                        );
                    } else {
                        item.win_vals[slot] = RuntimeValue::null();
                    }
                }
            }
        },
        WindowFnKind::Aggregate {
            name,
            arity,
            is_star,
            distinct,
            flags,
        } => {
            let preserve_arg_subtype = flags.contains(FunctionFlags::SUBTYPE);
            let coll_name = match call.args.first() {
                Some(first_arg) => extract_effective_collation_name(first_arg, sample_cols),
                None => "BINARY".to_owned(),
            };

            let mut row_args: Vec<Vec<RuntimeValue>> = Vec::with_capacity(n);
            let mut filter_ok: Vec<bool> = Vec::with_capacity(n);
            for item in part_items.iter() {
                ctx.step_tick()?;
                let scopes = push_scope_pub(outer_scopes, &item.row);
                let agg_grp = item.group_rows.as_deref().map(|gr| (gr, outer_scopes));
                let pass = if let Some(ref fe) = call.filter {
                    eval_expr_pub(ctx, fe, params, &scopes, ctes, agg_grp)?.truthiness()
                        == Some(true)
                } else {
                    true
                };
                filter_ok.push(pass);
                if *is_star {
                    row_args.push(Vec::new());
                } else {
                    let mut av = Vec::with_capacity(call.args.len());
                    for ae in &call.args {
                        let mut rv = eval_expr_rt_pub(ctx, ae, params, &scopes, ctes, agg_grp)?;
                        if !preserve_arg_subtype {
                            rv.subtype = 0;
                        }
                        av.push(rv);
                    }
                    row_args.push(av);
                }
            }

            ctx.publish();
            let enc = ctx.encoding();
            let mut svc = ExecutionContext {
                interrupt_flag: ctx.interrupt_flag,
                runtime: ctx.runtime,
                encoding: enc,
                functions: ctx.functions,
                collations: ctx.collations,
                progress: ctx.progress,
                strict_subtype: ctx.strict_subtype,
            };

            let probe_id = svc.agg_open_collated(name, *arity, *distinct, &coll_name)?;
            let can_slide = exclude == WindowFrameExclude::NoOthers
                && (svc.agg_supports_inverse(probe_id)
                    || matches!(start_bound, WindowFrameBound::UnboundedPreceding));

            if can_slide {
                let mut cur_s = 0usize;
                let mut cur_e = 0usize;
                let run_res = (|| -> Result<()> {
                    for (i, item) in part_items.iter_mut().enumerate() {
                        svc.check_interrupted()?;
                        svc.tick_progress()?;
                        let (s, e) = frames[i];
                        if cur_e < s {
                            for j in cur_e..s {
                                if filter_ok[j] {
                                    svc.agg_step(probe_id, &row_args[j])?;
                                }
                            }
                            for j in cur_s..s {
                                if filter_ok[j] {
                                    svc.agg_inverse(probe_id, &row_args[j])?;
                                }
                            }
                            cur_s = s;
                            cur_e = s;
                        } else if cur_s < s {
                            for j in cur_s..s {
                                if filter_ok[j] {
                                    svc.agg_inverse(probe_id, &row_args[j])?;
                                }
                            }
                            cur_s = s;
                        }
                        if cur_e < e {
                            for j in cur_e..e {
                                if filter_ok[j] {
                                    svc.agg_step(probe_id, &row_args[j])?;
                                }
                            }
                            cur_e = e;
                        }
                        item.win_vals[slot] = svc.agg_value(probe_id)?;
                    }
                    Ok(())
                })();
                let fin_res = svc.agg_finalize(probe_id);
                run_res?;
                let _ = fin_res?;
            } else {
                let _ = svc.agg_finalize(probe_id);
                for (i, item) in part_items.iter_mut().enumerate() {
                    svc.check_interrupted()?;
                    svc.tick_progress()?;
                    let (s, e) = frames[i];
                    let frame_id = svc.agg_open_collated(name, *arity, *distinct, &coll_name)?;
                    let step_res = (|| -> Result<RuntimeValue> {
                        for j in s..e {
                            if !row_is_excluded(j, i, exclude, row_peer_group) && filter_ok[j] {
                                svc.agg_step(frame_id, &row_args[j])?;
                            }
                        }
                        svc.agg_finalize(frame_id)
                    })();
                    match step_res {
                        Ok(rv) => item.win_vals[slot] = rv,
                        Err(err) => {
                            let _ = svc.agg_finalize(frame_id);
                            return Err(err);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
