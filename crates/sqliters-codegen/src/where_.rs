//! The interface of `where.c` (`sqlite3WhereBegin` / `sqlite3WhereEnd` and the accessors of
//! `WhereInfo`) with the two degenerate cases step V1 needs ported exactly:
//!
//! 1. **No tables in the FROM clause** (`nTabList==0`): the WHERE clause is split into its
//!    AND-connected terms (`sqlite3WhereSplit`), each term is analyzed (`exprAnalyze` of
//!    `whereexpr.c`, as far as it has an effect when no table exists) and coded with
//!    `sqlite3ExprIfFalse(..., pWInfo->iBreak, SQLITE_JUMPIFNULL)`; ORDER BY is satisfied
//!    trivially and DISTINCT is `WHERE_DISTINCT_UNIQUE` / `WHERE_DISTINCT_ORDERED` as
//!    `wherePathSolver()` decides for zero loops.
//! 2. **One FROM-clause term that is a co-routine** (the multi-row VALUES clause built by
//!    `sqlite3MultiValues`), without WHERE, ORDER BY or DISTINCT: the single full-scan loop
//!    `OP_InitCoroutine` / `OP_Yield` … `OP_Goto`, and the OP_Column → OP_Copy translation
//!    of `sqlite3WhereEnd()`.
//!
//! Everything else is the query planner (`where.c`, `wherecode.c`, `whereexpr.c`) and is the
//! next step: `sqlite3_where_begin` reports it as unsupported. The signatures are C's, so the
//! callers in `select.rs` do not change when the planner arrives.

use sqliters_vdbe::opcodes::*;

use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::sqliteint::*;
use crate::tk::*;

/// `WhereLevel` of the single co-routine loop.
#[derive(Debug, Clone, Default)]
pub struct WhereLevel {
    /// Jump here to break out of the loop
    pub addr_brk: i32,
    /// Jump here to continue with the next loop cycle
    pub addr_cont: i32,
    /// Opcode, P2 that end the loop (`op`, `p2`)
    pub op: u8,
    pub p2: i32,
    /// Beginning of the body of this loop
    pub addr_body: i32,
    /// The VDBE cursor used to access the table
    pub i_tab_cur: i32,
    /// `pSubq->regResult` of the co-routine
    pub reg_result: i32,
}

/// `struct WhereInfo`: the result of `sqlite3WhereBegin()`.
#[derive(Debug, Clone, Default)]
pub struct WhereInfo {
    /// Jump here to break out of the loop
    pub i_break: i32,
    /// Jump here to continue with next record
    pub i_continue: i32,
    /// Number of ORDER BY terms satisfied by indices
    pub n_ob_sat: i32,
    /// One of the WHERE_DISTINCT_* values
    pub e_distinct: u8,
    /// Estimated number of output rows
    pub n_row_out: LogEst,
    /// pParse->nQueryLoop outside the WHERE loop
    pub saved_n_query_loop: LogEst,
    /// The loops (none, or the single co-routine loop)
    pub a: Vec<WhereLevel>,
}

impl WhereInfo {
    /// `sqlite3WhereIsOrdered(pWInfo)`
    pub fn is_ordered(&self) -> i32 {
        if self.n_ob_sat < 0 { 0 } else { self.n_ob_sat }
    }
    /// `sqlite3WhereOrderByLimitOptLabel(pWInfo)`: `bOrderedInnerLoop` is never set without
    /// an index.
    pub fn order_by_limit_opt_label(&self) -> i32 {
        self.i_continue
    }
}

// WhereTerm.wtFlags
const TERM_CODED: u16 = 0x0004; // This term is already coded
const TERM_VIRTUAL: u16 = 0x0002; // Added by the optimizer. Do not code

struct WhereTerm {
    p_expr: ExprId,
    wt_flags: u16,
}

/// `sqlite3WhereSplit(pWC, pExpr, op)`: break the expression into separate terms whenever the
/// operator `op` (TK_AND here) is seen.
fn sqlite3_where_split(p: &Parse, a: &mut Vec<WhereTerm>, e: ExprId, op: u8) {
    let e2 = sqlite3_expr_skip_collate_and_likely(p, e);
    if e2.is_null() {
        return;
    }
    if p.e(e2).op != op {
        // whereClauseInsert(): pTerm->pExpr = sqlite3ExprSkipCollateAndLikely(p)
        a.push(WhereTerm { p_expr: e2, wt_flags: 0 });
    } else {
        sqlite3_where_split(p, a, p.e(e2).p_left, op);
        sqlite3_where_split(p, a, p.e(e2).p_right, op);
    }
}

/// `allowedOp(op)`: return TRUE if the given operator is one of the operators that is allowed
/// for an indexable WHERE clause term. The allowed operators are "=", "<", ">", "<=", ">=",
/// "IN", "IS", and "IS NULL"
fn allowed_op(op: u8) -> bool {
    op == TK_IN || (op >= TK_EQ && op <= TK_GE) || op == TK_ISNULL || op == TK_IS
}

/// True if the expression tree contains a column reference (any term of a WHERE clause over
/// zero tables that mentions a column would need the real `exprAnalyze`).
fn contains_column(p: &Parse, e: ExprId) -> bool {
    if e.is_null() {
        return false;
    }
    let x = p.e(e);
    if matches!(x.op, TK_COLUMN | TK_AGG_COLUMN | TK_TRIGGER | TK_IF_NULL_ROW) {
        return true;
    }
    if x.has(EP_TokenOnly | EP_Leaf) {
        return false;
    }
    if contains_column(p, x.p_left) || contains_column(p, x.p_right) {
        return true;
    }
    if x.use_x_select() {
        // a correlated sub-select
        x.has(EP_VarSelect)
    } else if x.x_list().nn() {
        p.l(x.x_list()).a.iter().any(|it| contains_column(p, it.p_expr))
    } else {
        false
    }
}

/// `exprAnalyze(pSrc, pWC, idxTerm)` for a WHERE clause over zero tables: the effects that
/// are visible in the generated code.
///
/// * `IN`: `sqlite3ExprCheckIN()` reports a row-value size mismatch.
/// * `X IS NULL` where X cannot be NULL becomes the constant FALSE.
/// * An OR term that `sqlite3ExprSimplifiedAndOr()` can simplify is replaced by its
///   simplification (without COLLATE / likely() wrappers).
/// * A vector equality `(a,b)=(c,d)` / `IS` is replaced by one term per field; the original
///   term is marked as coded.
///
/// The other cases of `exprAnalyze` (BETWEEN, LIKE prefix, NOT NULL, the OR-clause analysis,
/// vector IN, auxiliary virtual-table operators) only add *virtual* terms, which exist for
/// the benefit of index selection and are never coded.
fn expr_analyze(p: &mut Parse, a: &mut Vec<WhereTerm>, idx_term: usize) {
    let mut e = a[idx_term].p_expr;
    loop {
        let op = p.e(e).op;
        if op == TK_IN && sqlite3_expr_check_in(p, e) {
            return;
        }
        if allowed_op(op) {
            let left = sqlite3_expr_skip_collate(p, p.e(e).p_left);
            if op == TK_ISNULL && !p.e(e).has(EP_OuterON) && !sqlite3_expr_can_be_null(p, left) {
                let x = p.em(e);
                x.op = TK_TRUEFALSE; // See tag-20230504-1
                x.z_token = Some("false".to_owned());
                x.flags |= EP_IsFalse;
            }
        } else if op == TK_OR && !p.e(e).has(EP_Collate) {
            // Analyze a term that is composed of two or more subterms connected by an OR
            // operator.
            let alt = sqlite3_expr_simplified_and_or(p, e);
            if alt != e {
                e = sqlite3_expr_skip_collate_and_likely(p, alt);
                a[idx_term].p_expr = e;
                continue;
            }
        }
        break;
    }

    // If there is a vector == or IS term - e.g. "(a, b) == (?, ?)" - create new terms for
    // each component comparison - "a = ?" and "b = ?". The new terms completely replace the
    // original vector comparison, which is no longer used.
    //
    // This is only required if at least one side of the comparison operation is not a
    // sub-select.
    let op = p.e(e).op;
    if op == TK_EQ || op == TK_IS {
        let (left, right) = (p.e(e).p_left, p.e(e).p_right);
        let n_left = sqlite3_expr_vector_size(p, left);
        if n_left > 1
            && sqlite3_expr_vector_size(p, right) == n_left
            && ((p.e(left).flags & EP_xIsSelect) == 0 || (p.e(right).flags & EP_xIsSelect) == 0)
        {
            for i in 0..n_left {
                let l = sqlite3_expr_for_vector_field(p, left, i, n_left);
                let r = sqlite3_expr_for_vector_field(p, right, i, n_left);
                let new = sqlite3_p_expr(p, op, l, r);
                // transferJoinMarkings(): no join here
                a.push(WhereTerm { p_expr: new, wt_flags: 0 /* TERM_DYNAMIC|TERM_SLICE */ });
                let idx_new = a.len() - 1;
                expr_analyze(p, a, idx_new);
            }
            a[idx_term].wt_flags |= TERM_CODED | TERM_VIRTUAL; // Disable the original
        }
    }
}

/// `sqlite3WhereBegin(pParse, pTabList, pWhere, pOrderBy, pResultSet, pSelect, wctrlFlags,
/// iAuxArg)`: generate the beginning of the loop used for WHERE clause processing. The
/// return value is the WhereInfo to hand to `sqlite3WhereEnd()`; `None` on an error (or when
/// the FROM clause needs the query planner, which is reported as unsupported).
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_where_begin(
    p: &mut Parse,
    tab_list: SrcListId,
    p_where: ExprId,
    order_by: ExprListId,
    result_set: ExprListId,
    select: SelectId,
    wctrl_flags: u16,
    _i_aux_arg: i32,
) -> Option<WhereInfo> {
    let mut order_by = order_by;
    let mut wctrl_flags = wctrl_flags;
    // An ORDER BY/GROUP BY clause of more than 63 terms cannot be optimized
    if order_by.nn() && p.l(order_by).n_expr() >= BMS {
        order_by = ExprListId::NULL;
        wctrl_flags &= !(WHERE_WANT_DISTINCT as u16);
    }
    let n_tab_list = p.src(tab_list).a.len();
    if n_tab_list as i32 > BMS {
        p.error_msg(format!("at most {BMS} tables in a join"));
        return None;
    }
    let mut w = WhereInfo { saved_n_query_loop: p.n_query_loop, ..WhereInfo::default() };
    w.i_break = p.v.make_label();
    w.i_continue = w.i_break;

    if n_tab_list == 0 {
        // Split the WHERE clause into separate subexpressions where each subexpression is
        // separated by an AND operator.
        let mut terms: Vec<WhereTerm> = Vec::new();
        sqlite3_where_split(p, &mut terms, p_where, TK_AND);

        // Special case: No FROM clause
        if order_by.nn() {
            w.n_ob_sat = p.l(order_by).n_expr();
        }
        if (wctrl_flags & WHERE_WANT_DISTINCT as u16) != 0 && p.db.optimization_enabled(SQLITE_DistinctOpt) {
            w.e_distinct = WHERE_DISTINCT_UNIQUE as u8;
        }

        // Analyze all of the subexpressions.
        for t in &terms {
            if contains_column(p, t.p_expr) {
                p.not_supported("WHERE term with a column reference in a FROM-less SELECT");
                return None;
            }
        }
        // sqlite3WhereExprAnalyze(): for(i=pWC->nTerm-1; i>=0; i--) exprAnalyze(...)
        let n = terms.len();
        for i in (0..n).rev() {
            expr_analyze(p, &mut terms, i);
        }
        // (sqlite3WhereAddLimit(): only for a single virtual table in the FROM clause)
        if p.n_err != 0 {
            p.n_query_loop = w.saved_n_query_loop;
            return None;
        }

        // Deal with WHERE clause terms that do not reference any tables or only reference
        // tables to the left of the first: they are evaluated before the loops begin.
        // (nBase: every term here is a base term; the terms disabled above are virtual)
        for t in terms.iter_mut() {
            if t.wt_flags & TERM_VIRTUAL != 0 {
                continue;
            }
            let x = t.p_expr;
            sqlite3_expr_if_false(p, x, w.i_break, SQLITE_JUMPIFNULL);
            t.wt_flags |= TERM_CODED;
        }

        let mut distinct_by = (wctrl_flags & WHERE_DISTINCTBY as u16) != 0;
        let mut w_order_by = order_by;
        if wctrl_flags & WHERE_WANT_DISTINCT as u16 != 0 {
            if p.db.optimization_disabled(SQLITE_DistinctOpt) {
                // Disable the DISTINCT optimization if SQLITE_DistinctOpt is set via
                // sqlite3_test_ctrl(SQLITE_TESTCTRL_OPTIMIZATIONS,...)
                wctrl_flags &= !(WHERE_WANT_DISTINCT as u16);
            } else if order_by.is_null() {
                // (isDistinctRedundant() needs exactly one table)
                // Try to ORDER BY the result set to make distinct processing easier
                distinct_by = true;
                w_order_by = result_set;
            }
        }

        // wherePathSolver(pWInfo, 0) and, if there is an ORDER BY,
        // wherePathSolver(pWInfo, pWInfo->nRowOut+1), for zero loops:
        //   aFrom[0].nRow = MIN(pParse->nQueryLoop, 48)
        //   aFrom[0].isOrdered = nOrderBy   (second pass only)
        w.n_row_out = p.n_query_loop.min(48);
        if w_order_by.nn() {
            // first pass: pWInfo->nOBSat = pFrom->isOrdered = 0; second pass: = nOrderBy
            w.n_ob_sat = p.l(w_order_by).n_expr();
            if distinct_by {
                // if( pFrom->isOrdered==pWInfo->pOrderBy->nExpr )
                w.e_distinct = WHERE_DISTINCT_ORDERED as u8;
            }
        }
        if (wctrl_flags & WHERE_WANT_DISTINCT as u16) != 0 {
            w.n_row_out -= 30;
        }
        if w_order_by.is_null() {
            w.n_ob_sat = 0;
        }
        let _ = select;
        p.n_query_loop += w.n_row_out;
        return Some(w);
    }

    // One term: the co-routine of a multi-row VALUES clause.
    let ok = n_tab_list == 1 && {
        let it = &p.src(tab_list).a[0];
        it.via_coroutine && it.is_subquery
    };
    // (WHERE_USE_LIMIT only feeds the cost estimates)
    if !ok || p_where.nn() || order_by.nn() || (wctrl_flags & !(WHERE_USE_LIMIT as u16)) != 0 {
        p.not_supported("sqlite3WhereBegin: query planner (where.c) for a FROM clause");
        p.n_query_loop = w.saved_n_query_loop;
        return None;
    }
    let (i_cursor, reg_return, addr_fill_sub, reg_result) = {
        let it = &p.src(tab_list).a[0];
        let q = it.p_subq.as_ref().expect("co-routine subquery");
        (it.i_cursor, q.reg_return, q.addr_fill_sub, q.reg_result)
    };
    // whereLoopAddAll() / wherePathSolver(): one full-scan loop over the co-routine
    w.n_row_out = p.n_query_loop.min(48) + 200;
    p.n_query_loop += w.n_row_out;

    // Open all tables in the pTabList and any indices selected for searching those tables:
    // nothing to open for an ephemeral (subquery) table.
    let mut level = WhereLevel { i_tab_cur: i_cursor, reg_result, ..WhereLevel::default() };
    level.addr_brk = p.v.make_label();

    // Generate the code to do the search. sqlite3WhereCodeOneLoopStart():
    level.addr_body = p.v.current_addr();
    level.addr_cont = p.v.make_label();
    // Special case of a FROM clause subquery implemented as a co-routine
    p.v.add_op3(OP_InitCoroutine, reg_return, 0, addr_fill_sub);
    level.p2 = p.v.add_op2(OP_Yield, reg_return, level.addr_brk);
    level.op = OP_Goto;
    w.i_continue = level.addr_cont;
    w.a.push(level);
    Some(w)
}

/// `sqlite3WhereEnd(pWInfo)`: generate the end of the WHERE loop. See comments on
/// sqlite3WhereBegin() for additional information.
pub fn sqlite3_where_end(p: &mut Parse, w: WhereInfo) {
    // Generate loop termination code.
    for level in w.a.iter().rev() {
        p.v.resolve_label(level.addr_cont);
        if level.op != OP_Noop {
            p.v.add_op3(level.op, 0, level.p2, 0);
            p.v.change_p5(0);
        }
        p.v.resolve_label(level.addr_brk);
    }
    // The "break" point is here, just past the end of the outer loop. Set it.
    p.v.resolve_label(w.i_break);
    for level in &w.a {
        // For a co-routine, change all OP_Column references to the table of the co-routine
        // into OP_Copy of the result contained in a register. OP_Rowid becomes OP_Null.
        // translateColumnToCopy(pParse, pLevel->addrBody, pLevel->iTabCur, regResult, 0)
        let i_end = p.v.current_addr();
        for a in level.addr_body..i_end {
            let op = p.v.get_op_mut(a);
            if op.p1 != level.i_tab_cur {
                continue;
            }
            if op.opcode == OP_Column {
                op.opcode = OP_Copy;
                op.p1 = op.p2 + level.reg_result;
                op.p2 = op.p3;
                op.p3 = 0;
                op.p5 = 2; // Cause the MEM_Subtype flag to be cleared
            } else if op.opcode == OP_Rowid {
                op.opcode = OP_Sequence;
                op.p1 = 0;
            }
        }
    }
    // Final cleanup
    p.n_query_loop = w.saved_n_query_loop;
}
