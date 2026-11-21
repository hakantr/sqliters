//! `where.c`: this module contains C code that generates VDBE code used to process the WHERE
//! clause of SQL statements. This module is responsible for generating the code that loops
//! through a table looking for applicable rows. Indices are selected and used to speed the
//! search when doing so is applicable. Because this module is responsible for selecting
//! indices, you might also think of this module as the "query optimizer".
//!
//! Virtual tables (`whereLoopAddVirtual` and the `sqlite3_index_info` plumbing) and the
//! STAT4 estimators (`whereKeyStats`, `whereEqualScanEst`, `whereInScanEst`,
//! `whereRangeSkipScanEst`: the reference build has no `SQLITE_ENABLE_STAT4`) are not part
//! of this port; a virtual table in a FROM clause is reported as unsupported before the
//! planner is reached.

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{KeyInfo, P4, KEYINFO_ORDER_BIGNULL, KEYINFO_ORDER_DESC};

use crate::build::*;
use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::schema::*;
use crate::select::{sqlite3_log_est, sqlite3_log_est_add};
use crate::sqliteint::*;
use crate::tk::*;
use crate::walker::*;
use crate::wherecode::*;
use crate::whereexpr::*;
use crate::whereint::*;
use crate::vtab::{IndexConstraint, IndexConstraintUsage, IndexInfo, IndexInfoHidden, IndexOrderBy};
use sqliters_types::Value;

/// `sqlite3WhereOrderByLimitOptLabel(pWInfo)`: in the ORDER BY LIMIT optimization, if the
/// inner-most loop is known to emit rows in increasing order, then we can optimize the sort
/// by jumping to the next iteration of an outer loop when the LIMIT is reached.
///
/// Return the address (label) of the instruction to jump to when the sorter is full:
/// the "continue" label of the WHERE clause if the optimization does not apply, otherwise
/// the "next" label of the inner loop.
pub fn sqlite3_where_order_by_limit_opt_label(w: &WhereInfo) -> i32 {
    if !w.b_ordered_inner_loop {
        // The ORDER BY LIMIT optimization does not apply. Jump to the continuation of the
        // inner-most loop.
        return w.i_continue;
    }
    let inner = &w.a[w.n_level as usize - 1];
    if inner.p_rj.is_some() { w.i_continue } else { inner.addr_nxt }
}

/// `sqlite3WhereMinMaxOptEarlyOut(v, pWInfo)`: while generating code for the min/max
/// optimization, after handling the aggregate-step call for a single row, the WHERE loop
/// may exit early: all later rows of the inner loop cannot change the result.
pub fn sqlite3_where_min_max_opt_early_out(p: &mut Parse, w: &WhereInfo) {
    if !w.b_ordered_inner_loop {
        return;
    }
    if w.n_ob_sat == 0 {
        return;
    }
    for i in (0..w.n_level as usize).rev() {
        let inner = &w.a[i];
        if (w.loops[inner.p_w_loop].ws_flags & WHERE_COLUMN_IN as u32) != 0 {
            p.v.goto(inner.addr_nxt);
            return;
        }
    }
    p.v.goto(w.i_break);
}

/// `sqlite3WhereOkOnePass(pWInfo, aiCur)`: return ONEPASS_OFF (0) if an UPDATE or DELETE
/// statement is unable to operate in ONEPASS mode, the cursors otherwise.
pub fn sqlite3_where_ok_one_pass(w: &WhereInfo, ai_cur: &mut [i32; 2]) -> i32 {
    *ai_cur = w.ai_cur_one_pass;
    w.e_one_pass as i32
}

/// `whereOrInsert(pSet, prereq, rRun, nOut)`: try to insert a new prerequisite/cost entry
/// into the WhereOrSet pSet.
///
/// The new entry might overwrite an existing entry, or it might be appended, or it might be
/// discarded. Do whatever is the right thing so that pSet keeps the N_OR_COST best entries
/// seen so far.
fn where_or_insert(set: &mut WhereOrSet, prereq: Bitmask, r_run: LogEst, n_out: LogEst) -> bool {
    let mut k: Option<usize> = None;
    for i in 0..set.n as usize {
        let c = &set.a[i];
        if r_run <= c.r_run && (prereq & c.prereq) == prereq {
            k = Some(i);
            break;
        }
        if c.r_run <= r_run && (c.prereq & prereq) == c.prereq {
            return false;
        }
    }
    let k = match k {
        Some(k) => k,
        None => {
            if (set.n as usize) < N_OR_COST {
                let k = set.n as usize;
                set.n += 1;
                set.a[k].n_out = n_out;
                k
            } else {
                let mut k = 0;
                for i in 1..set.n as usize {
                    if set.a[k].r_run > set.a[i].r_run {
                        k = i;
                    }
                }
                if set.a[k].r_run <= r_run {
                    return false;
                }
                k
            }
        }
    };
    // whereOrInsert_done:
    let c = &mut set.a[k];
    c.prereq = prereq;
    c.r_run = r_run;
    if c.n_out > n_out {
        c.n_out = n_out;
    }
    true
}

/// `sqlite3WhereGetMask(pMaskSet, iCursor)`: return the bitmask for the given cursor number.
/// Return 0 if iCursor is not in the set.
pub fn sqlite3_where_get_mask(m: &WhereMaskSet, i_cursor: i32) -> Bitmask {
    if m.ix[0] == i_cursor {
        return 1;
    }
    for i in 1..m.n as usize {
        if m.ix[i] == i_cursor {
            return 1u64 << i;
        }
    }
    0
}

/// `createMask(pMaskSet, iCursor)`: create a new mask for cursor iCursor.
///
/// There is one cursor per table in the FROM clause. The number of tables in the FROM clause
/// is limited by a test early in the sqlite3WhereBegin() routine. So we know that the
/// pMaskSet->ix[] array will never overflow.
fn create_mask(m: &mut WhereMaskSet, i_cursor: i32) {
    m.ix[m.n as usize] = i_cursor;
    m.n += 1;
}

/// `whereRightSubexprIsColumn(p)`: if the right-hand branch of the expression is a TK_COLUMN,
/// then return a pointer to the right-hand branch. Otherwise, return NULL.
fn where_right_subexpr_is_column(p: &Parse, e: ExprId) -> ExprId {
    let r = sqlite3_expr_skip_collate_and_likely(p, p.e(e).p_right);
    if r.nn() && p.e(r).op == TK_COLUMN && !p.e(r).has(EP_FixedCol) {
        return r;
    }
    ExprId::NULL
}

/// `indexInAffinityOk(pParse, pTerm, idxaff)`: term pTerm is guaranteed to be a WO_IN term.
/// It may be a component term of a vector IN expression of the form:
///
///       (x,y) IN (SELECT ...)
///
/// Or it may be a regular IN expression. This function checks if the affinity of the index
/// column (idxaff) is compatible with the IN and, if so, returns the name of the collating
/// sequence of the comparison.
fn index_in_affinity_ok(p: &mut Parse, term: &WhereTerm, idxaff: u8) -> Option<String> {
    let mut x = term.p_expr;
    if sqlite3_expr_is_vector(p, p.e(x).p_left) {
        let i_field = (term.i_field - 1) as usize;
        let l = p.l(p.e(p.e(x).p_left).x_list()).a[i_field].p_expr;
        let r = p.l(p.s(p.e(x).x_select()).p_e_list).a[i_field].p_expr;
        // Expr inexpr; inexpr.flags = 0; inexpr.op = TK_EQ; inexpr.pLeft = ...; inexpr.pRight = ...
        x = p.new_expr(Expr { op: TK_EQ, p_left: l, p_right: r, ..Expr::default() });
    }
    if sqlite3_index_affinity_ok(p, x, idxaff) {
        let ret = sqlite3_expr_compare_coll_seq(p, x);
        return Some(ret.map_or_else(|| "BINARY".to_owned(), |c| c.name.clone()));
    }
    None
}

/// `whereScanNext(pScan)`: advance to the next WhereTerm that matches according to the
/// criteria established when the pScan object was initialized by whereScanInit(). Return
/// NULL if there are no more matching WhereTerms.
fn where_scan_next(p: &mut Parse, w: &WhereInfo, scan: &mut WhereScan) -> Option<TermRef> {
    let mut k = scan.k as usize; // Where to start scanning
    let mut wc_opt = scan.p_wc;
    loop {
        let i_column = scan.ai_column[scan.i_equiv as usize - 1];
        let i_cur = scan.ai_cur[scan.i_equiv as usize - 1];
        while let Some(wc) = wc_opt {
            let n_term = w.a_wc[wc].a.len();
            while k < n_term {
                let term = &w.a_wc[wc].a[k];
                if term.left_cursor == i_cur
                    && term.left_column == i_column as i32
                    && (i_column as i32 != XN_EXPR
                        || sqlite3_expr_compare_skip(p, p.e(term.p_expr).p_left, scan.p_idx_expr, i_cur) == 0)
                    && (scan.i_equiv <= 1 || !p.e(term.p_expr).has(EP_OuterON))
                {
                    if (term.e_operator & WO_EQUIV) != 0 && (scan.n_equiv as usize) < scan.ai_cur.len() {
                        let x = where_right_subexpr_is_column(p, term.p_expr);
                        if x.nn() {
                            let (xt, xc) = (p.e(x).i_table, p.e(x).i_column);
                            let mut j = 0;
                            while j < scan.n_equiv as usize {
                                if scan.ai_cur[j] == xt && scan.ai_column[j] == xc {
                                    break;
                                }
                                j += 1;
                            }
                            if j == scan.n_equiv as usize {
                                scan.ai_cur[j] = xt;
                                scan.ai_column[j] = xc;
                                scan.n_equiv += 1;
                            }
                        }
                    }
                    if (term.e_operator as u32 & scan.op_mask) != 0 {
                        // Verify the affinity and collating sequence match
                        if let Some(z_scan_coll) = scan.z_coll_name.clone()
                            && (term.e_operator & WO_ISNULL) == 0
                        {
                            let x = term.p_expr;
                            let z_coll_name = if (term.e_operator & WO_IN) != 0 {
                                let t = term.clone();
                                match index_in_affinity_ok(p, &t, scan.idxaff) {
                                    Some(z) => z,
                                    None => {
                                        k += 1;
                                        continue;
                                    }
                                }
                            } else {
                                if !sqlite3_index_affinity_ok(p, x, scan.idxaff) {
                                    k += 1;
                                    continue;
                                }
                                let coll = sqlite3_expr_compare_coll_seq(p, x);
                                coll.map_or_else(|| "BINARY".to_owned(), |c| c.name.clone())
                            };
                            if !z_coll_name.eq_ignore_ascii_case(&z_scan_coll) {
                                k += 1;
                                continue;
                            }
                        }
                        let term = &w.a_wc[wc].a[k];
                        if (term.e_operator & (WO_EQ | WO_IS)) != 0 {
                            let x = p.e(term.p_expr).p_right;
                            if x.nn()
                                && p.e(x).op == TK_COLUMN
                                && p.e(x).i_table == scan.ai_cur[0]
                                && p.e(x).i_column == scan.ai_column[0]
                            {
                                k += 1;
                                continue;
                            }
                        }
                        scan.p_wc = Some(wc);
                        scan.k = k as i32 + 1;
                        return Some(TermRef::new(wc, k));
                    }
                }
                k += 1;
            }
            wc_opt = w.a_wc[wc].p_outer;
            k = 0;
        }
        if scan.i_equiv >= scan.n_equiv {
            break;
        }
        wc_opt = Some(scan.p_orig_wc);
        k = 0;
        scan.i_equiv += 1;
    }
    None
}

/// `whereScanInit(pScan, pWC, iCur, iColumn, opMask, pIdx)`: initialize a WHERE clause
/// scanner object. Return a pointer to the first match. Return NULL if there are no matches.
///
/// The scanner will be searching the WHERE clause pWC. It will look for terms of the form
/// "X <op> <expr>" where X is column iColumn of table iCur. Or if pIdx!=0 then X is column
/// iColumn of index pIdx. pIdx might be for an index on expressions. If pIdx!=0 then it must
/// be one of the indexes of table iCur.
///
/// The <op> must be one of the operators described by opMask.
///
/// If the search is for X and the WHERE clause contains terms of the form X=Y then this
/// routine might also return terms of the form "Y <op> <expr>". The number of levels of
/// transitivity is limited, but is allowed to be more than one.
///
/// If X is not the INTEGER PRIMARY KEY then X must be compatible with index pIdx.
fn where_scan_init(
    p: &mut Parse,
    w: &WhereInfo,
    scan: &mut WhereScan,
    wc: WcId,
    i_cur: i32,
    i_column: i32,
    op_mask: u32,
    idx: IndexId,
) -> Option<TermRef> {
    *scan = WhereScan { p_orig_wc: wc, p_wc: Some(wc), op_mask, ..WhereScan::default() };
    scan.ai_cur[0] = i_cur;
    scan.n_equiv = 1;
    scan.i_equiv = 1;
    let mut i_column = i_column;
    if idx.nn() {
        let j = i_column as usize;
        let tab = p.idx(idx).p_table;
        i_column = p.idx(idx).ai_column[j] as i32;
        if i_column == p.tab(tab).i_p_key as i32 {
            i_column = XN_ROWID;
        } else if i_column >= 0 {
            scan.idxaff = p.tab(tab).a_col[i_column as usize].affinity;
            scan.z_coll_name = Some(p.idx(idx).az_coll[j].clone());
        } else if i_column == XN_EXPR {
            scan.p_idx_expr = p.l(p.idx(idx).a_col_expr).a[j].p_expr;
            scan.z_coll_name = Some(p.idx(idx).az_coll[j].clone());
            scan.ai_column[0] = XN_EXPR as i16;
            // whereScanInitIndexExpr(pScan)
            scan.idxaff = sqlite3_expr_affinity(p, scan.p_idx_expr);
            return where_scan_next(p, w, scan);
        }
    } else if i_column == XN_EXPR {
        return None;
    }
    scan.ai_column[0] = i_column as i16;
    where_scan_next(p, w, scan)
}

/// `sqlite3WhereFindTerm(pWC, iCur, iColumn, notReady, op, pIdx)`: search for a term in the
/// WHERE clause that is of the form "X <op> <expr>" where X is a reference to the iColumn of
/// table iCur or if pIdx!=0 then X is the iColumn-th column of index pIdx. pIdx might be an
/// index on expressions in which case iColumn is ignored...
///
/// The term returned might by Y=<expr> if there is another constraint in the WHERE clause
/// that specifies that X=Y. Any such constraints will be identified by the WO_EQUIV bit in
/// the pTerm->eOperator field. The aiCur[]/iaColumn[] arrays hold X and all its equivalents.
/// There are 11 slots in aiCur[]/aiColumn[] so that means we can look for X plus up to 10
/// other equivalent values. Hence a search for X will return <expr> if X=A1 and A1=A2 and
/// A2=A3 and ... and A9=A10 and A10=<expr>.
///
/// If there are multiple terms in the WHERE clause of the form "X <op> <expr>" then try for
/// the one with no dependencies on <expr> - in other words where <expr> is a constant
/// expression of some kind. Only return entries of the form "X <op> Y" where Y is a column
/// in another table if no terms of the form "X <op> <const-expr>" exist. If no terms with a
/// constant RHS exist, try to return a term that does not use WO_EQUIV.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_where_find_term(
    p: &mut Parse,
    w: &WhereInfo,
    wc: WcId,
    i_cur: i32,
    i_column: i32,
    not_ready: Bitmask,
    op: u32,
    idx: IndexId,
) -> Option<TermRef> {
    let mut result: Option<TermRef> = None;
    let mut scan = WhereScan::default();
    let mut t = where_scan_init(p, w, &mut scan, wc, i_cur, i_column, op, idx);
    let op = op & (WO_EQ | WO_IS) as u32;
    while let Some(tr) = t {
        let term = w.term(tr);
        if (term.prereq_right & not_ready) == 0 {
            if term.prereq_right == 0 && (term.e_operator as u32 & op) != 0 {
                return Some(tr);
            }
            if result.is_none() {
                result = Some(tr);
            }
        }
        t = where_scan_next(p, w, &mut scan);
    }
    result
}

/// `findIndexCol(pParse, pList, iBase, pIdx, iCol)`: this function searches pList for an
/// entry that matches the iCol-th column of index pIdx.
///
/// If such an expression is found, its index in pList->a[] is returned. If no expression is
/// found, -1 is returned.
fn find_index_col(p: &mut Parse, list: ExprListId, i_base: i32, idx: IndexId, i_col: usize) -> i32 {
    let z_coll = p.idx(idx).az_coll[i_col].clone();
    let col = p.idx(idx).ai_column[i_col];
    for i in 0..p.l(list).a.len() {
        let le = p.l(list).a[i].p_expr;
        let e = sqlite3_expr_skip_collate_and_likely(p, le);
        if e.nn() && (p.e(e).op == TK_COLUMN || p.e(e).op == TK_AGG_COLUMN) && p.e(e).i_column == col && p.e(e).i_table == i_base {
            let coll = sqlite3_expr_nn_coll_seq(p, le);
            if coll.name.eq_ignore_ascii_case(&z_coll) {
                return i as i32;
            }
        }
    }
    -1
}

/// `indexColumnNotNull(pIdx, iCol)`: return TRUE if the iCol-th column of index pIdx is NOT
/// NULL
fn index_column_not_null(p: &Parse, idx: IndexId, i_col: usize) -> bool {
    let j = p.idx(idx).ai_column[i_col];
    if j >= 0 {
        p.tab(p.idx(idx).p_table).a_col[j as usize].not_null != 0
    } else {
        // -1: the rowid; otherwise assume an indexed expression can always yield a NULL
        j == -1
    }
}

/// `isDistinctRedundant(pParse, pTabList, pWC, pDistinct)`: return true if the DISTINCT
/// expression-list passed as the third argument is redundant.
///
/// A DISTINCT list is redundant if any subset of the columns in the DISTINCT list are
/// collectively unique and individually non-null.
fn is_distinct_redundant(p: &mut Parse, w: &WhereInfo, tab_list: SrcListId, wc: WcId, distinct: ExprListId) -> bool {
    // If there is more than one table or sub-select in the FROM clause of this query, then it
    // will not be possible to show that the DISTINCT clause is redundant
    if p.src(tab_list).a.len() != 1 {
        return false;
    }
    let i_base = p.src(tab_list).a[0].i_cursor;
    let tab = p.src(tab_list).a[0].p_s_tab;

    // If any of the expressions is an IPK column on table iBase, then return true. Note: The
    // (p->iTable==iBase) part of this test may be false if the current SELECT is a
    // correlated sub-query.
    for i in 0..p.l(distinct).a.len() {
        let e = sqlite3_expr_skip_collate_and_likely(p, p.l(distinct).a[i].p_expr);
        if e.is_null() {
            continue;
        }
        if p.e(e).op != TK_COLUMN && p.e(e).op != TK_AGG_COLUMN {
            continue;
        }
        if p.e(e).i_table == i_base && p.e(e).i_column < 0 {
            return true;
        }
    }

    // Loop through all indices on the table, checking each to see if it makes the DISTINCT
    // qualifier redundant. It does so if:
    //
    //   1. The index is itself UNIQUE, and
    //
    //   2. All of the columns in the index are either part of the pDistinct list, or else
    //      the WHERE clause contains a term of the form "col=X", where X is a constant
    //      value. The collation sequences of the comparison and select-list expressions
    //      must match those of the index.
    //
    //   3. All of those index columns for which the WHERE clause does not contain a "col=X"
    //      term are subject to a NOT NULL constraint.
    let idxs = p.tab(tab).p_index.clone();
    for idx in idxs {
        if !p.idx(idx).is_unique() {
            continue;
        }
        if p.idx(idx).p_part_idx_where.nn() {
            continue;
        }
        let n_key = p.idx(idx).n_key_col as usize;
        let mut i = 0;
        while i < n_key {
            if sqlite3_where_find_term(p, w, wc, i_base, i as i32, !0, WO_EQ as u32, idx).is_none() {
                if find_index_col(p, distinct, i_base, idx, i) < 0 {
                    break;
                }
                if !index_column_not_null(p, idx, i) {
                    break;
                }
            }
            i += 1;
        }
        if i == n_key {
            // This index implies that the DISTINCT qualifier is redundant.
            return true;
        }
    }
    false
}

/// `estLog(N)`: estimate the logarithm of the input value to base 2.
fn est_log(n: LogEst) -> LogEst {
    if n <= 10 { 0 } else { sqlite3_log_est(n as u64) - 33 }
}

/// `translateColumnToCopy(pParse, iStart, iTabCur, iRegister, iAutoidxCur)`: convert
/// OP_Column opcodes to OP_Copy in previously generated code.
///
/// This routine runs over generated VDBE code and translates OP_Column opcodes into OP_Copy
/// when the table is being accessed via co-routine instead of via table lookup.
///
/// If the iAutoidxCur is not zero, then any OP_Rowid instructions on cursor iTabCur are
/// translated into a OP_Sequence against iAutoidxCur. Otherwise OP_Rowid becomes
/// OP_Sequence 0 (a rowid of a co-routine is never visible: NoVisibleRowid).
pub fn translate_column_to_copy(p: &mut Parse, i_start: i32, i_tab_cur: i32, i_register: i32, i_autoidx_cur: i32) {
    let i_end = p.v.current_addr();
    for a in i_start..i_end {
        let op = p.v.get_op_mut(a);
        if op.p1 != i_tab_cur {
            continue;
        }
        if op.opcode == OP_Column {
            op.opcode = OP_Copy;
            op.p1 = op.p2 + i_register;
            op.p2 = op.p3;
            op.p3 = 0;
            op.p5 = 2; // Cause the MEM_Subtype flag to be cleared
        } else if op.opcode == OP_Rowid {
            op.opcode = OP_Sequence;
            op.p1 = i_autoidx_cur;
        }
    }
}

/// `constraintCompatibleWithOuterJoin(pTerm, pSrc)`: an ON or USING clause term of a LEFT or
/// RIGHT join can only be used to drive an index of the table the join brings in.
fn constraint_compatible_with_outer_join(p: &Parse, term: &WhereTerm, src: &SrcItem) -> bool {
    let x = p.e(term.p_expr);
    if !x.has(EP_OuterON | EP_InnerON) || x.w != src.i_cursor {
        return false;
    }
    if (src.jointype as i32 & (JT_LEFT | JT_RIGHT)) != 0 && x.has(EP_InnerON) {
        return false;
    }
    true
}

/// `columnIsGoodIndexCandidate(pTab, iCol)`: return true if column iCol of table pTab seem
/// like it might be a good column to use as part of a query-time index.
///
/// Current algorithm (subject to improvement!):
///
///   1.   If iCol is already the left-most column of some other index, then return false.
///
///   2.   If iCol is part of an existing index that has an aiRowLogEst of more than 20,
///        then return false.
///
///   3.   If no disqualifying conditions above are found, return true.
fn column_is_good_index_candidate(p: &Parse, tab: TableId, i_col: i32) -> bool {
    for &ix in &p.tab(tab).p_index {
        let idx = p.idx(ix);
        for j in 0..idx.n_key_col as usize {
            if idx.ai_column[j] as i32 == i_col {
                if j == 0 {
                    return false;
                }
                if idx.has_stat1 && idx.ai_row_log_est[j + 1] > 20 {
                    return false;
                }
                break;
            }
        }
    }
    true
}

/// `termCanDriveIndex(pTerm, pSrc, notReady)`: return TRUE if the WHERE clause term pTerm is
/// of a form where it could be used with an index to access pSrc, assuming an appropriate
/// index existed.
fn term_can_drive_index(p: &mut Parse, term: &WhereTerm, src_list: SrcListId, i_src: usize, not_ready: Bitmask) -> bool {
    let src = p.src(src_list).a[i_src].clone();
    if term.left_cursor != src.i_cursor {
        return false;
    }
    if (term.e_operator & (WO_EQ | WO_IS)) == 0 {
        return false;
    }
    if (src.jointype as i32 & (JT_LEFT | JT_LTORJ | JT_RIGHT)) != 0 && !constraint_compatible_with_outer_join(p, term, &src) {
        return false; // See https://sqlite.org/forum/forumpost/51e6959f61
    }
    if (term.prereq_right & not_ready) != 0 {
        return false;
    }
    let left_col = term.left_column;
    if left_col < 0 {
        return false;
    }
    let aff = p.tab(src.p_s_tab).a_col[left_col as usize].affinity;
    if !sqlite3_index_affinity_ok(p, term.p_expr, aff) {
        return false;
    }
    column_is_good_index_candidate(p, src.p_s_tab, left_col)
}

/// `sqlite3GenerateIndexKey(pParse, pIdx, iDataCur, regOut, prefixOnly, 0, 0, 0)` (delete.c):
/// generate code that will assemble an index key and stores it in register regOut. The key
/// with be for index pIdx which is an index on pTab. iCur is the index of a cursor open on
/// the pTab table and pointing to the entry that needs indexing. If pTab is a WITHOUT ROWID
/// table, then iCur must be the cursor of the PRIMARY KEY index.
///
/// Return a register number which is the first in a block of registers that holds the
/// elements of the index key. The block of registers has already been deallocated by the
/// time this routine returns.
pub fn sqlite3_generate_index_key(p: &mut Parse, idx: IndexId, i_data_cur: i32, reg_out: i32, prefix_only: bool) -> i32 {
    let n_col = {
        let i = p.idx(idx);
        if prefix_only && i.uniq_not_null { i.n_key_col as i32 } else { i.n_column as i32 }
    };
    let reg_base = sqlite3_get_temp_range(p, n_col);
    for j in 0..n_col {
        sqlite3_expr_code_load_index_column(p, idx, i_data_cur, j, reg_base + j);
        if p.idx(idx).ai_column[j as usize] >= 0 {
            // If the column affinity is REAL but the number is an integer, then it might be
            // stored in the table as an integer (using a compact representation) then
            // converted to REAL by an OP_RealAffinity opcode. But we are getting ready to
            // store this value back into an index, where it should be converted by to INTEGER
            // again. So omit the OP_RealAffinity opcode if it is present
            p.v.delete_prior_opcode(OP_RealAffinity);
        }
    }
    if reg_out != 0 {
        p.v.add_op3(OP_MakeRecord, reg_base, n_col, reg_out);
    }
    sqlite3_release_temp_range(p, reg_base, n_col);
    reg_base
}

/// `constructAutomaticIndex(pParse, pWC, notReady, pLevel)`: generate code to construct the
/// Index object for an automatic index and to set up the WhereLevel object pLevel so that
/// the code generator makes use of the automatic index.
pub fn construct_automatic_index(p: &mut Parse, w: &mut WhereInfo, wc: WcId, not_ready: Bitmask, i_level: usize) {
    let mut sent_warning = false; // True if a warning has been issued
    let mut use_bloom_filter = true; // True to also add a Bloom filter
    let mut partial = ExprId::NULL; // Partial Index Expression
    let mut i_continue = 0; // Jump here to skip excluded rows
    let mut addr_counter = 0; // Address where integer counter is initialized

    // Generate code to skip over the creation and initialization of the transient index on
    // 2nd and subsequent iterations of the loop.
    let addr_init = p.v.add_op0(OP_Once);

    // Count the number of columns that will be added to the index and used to match WHERE
    // clause constraints
    let mut n_key_col: i32 = 0;
    let tab_list = w.p_tab_list;
    let i_from = w.a[i_level].i_from as usize;
    let table = p.src(tab_list).a[i_from].p_s_tab;
    let i_loop = w.a[i_level].p_w_loop;
    let mut idx_cols: Bitmask = 0;
    let n_term = w.a_wc[wc].a.len();
    for k in 0..n_term {
        let term = w.a_wc[wc].a[k].clone();
        let e = term.p_expr;
        // Make the automatic index a partial index if there are terms in the WHERE clause
        // (or the ON clause of a LEFT join) that constrain which rows of the target table
        // can participate.
        if (term.wt_flags & TERM_VIRTUAL) == 0 && sqlite3_expr_is_single_table_constraint(p, e, tab_list, i_from, false) {
            let dup = sqlite3_expr_dup(p, e, 0);
            partial = sqlite3_expr_and(p, partial, dup);
        }
        if term_can_drive_index(p, &term, tab_list, i_from, not_ready) {
            let i_col = term.left_column;
            let c_mask: Bitmask = if i_col >= BMS { 1u64 << (BMS - 1) } else { 1u64 << i_col };
            if !sent_warning {
                // sqlite3_log(SQLITE_WARNING_AUTOINDEX, "automatic index on %s(%s)", ...)
                sent_warning = true;
            }
            if (idx_cols & c_mask) == 0 {
                let l = &mut w.loops[i_loop];
                l.a_l_term.truncate(n_key_col as usize);
                l.a_l_term.push(Some(TermRef::new(wc, k)));
                n_key_col += 1;
                idx_cols |= c_mask;
            }
        }
    }
    {
        let l = &mut w.loops[i_loop];
        l.a_l_term.truncate(n_key_col as usize);
        l.n_eq = n_key_col as u16;
        l.ws_flags = (WHERE_COLUMN_EQ | WHERE_IDX_ONLY | WHERE_INDEXED | WHERE_AUTO_INDEX) as u32;
    }

    // Count the number of additional columns needed to create a covering index. A "covering
    // index" is an index that contains all columns that are needed by the query. With a
    // covering index, the original table never needs to be accessed. Automatic indices must
    // be covering because the index will not be updated if the original table changes and
    // the index and table cannot both be used if they go out of sync.
    let col_used = p.src(tab_list).a[i_from].col_used;
    let mut extra_cols: Bitmask = if p.tab(table).is_view() { !idx_cols } else { col_used & (!idx_cols | (1u64 << (BMS - 1))) };
    let n_col = p.tab(table).a_col.len() as i32;
    let has_rowid = p.tab(table).has_rowid();
    if !has_rowid {
        // For WITHOUT ROWID tables, ensure that all PRIMARY KEY columns are either in the
        // idxCols mask or in the extraCols mask
        for i in 0..n_col {
            if (p.tab(table).a_col[i as usize].col_flags & COLFLAG_PRIMKEY as u16) == 0 {
                continue;
            }
            if i >= BMS - 1 {
                extra_cols |= 1u64 << (BMS - 1);
                break;
            }
            if idx_cols & (1u64 << i) != 0 {
                continue;
            }
            extra_cols |= 1u64 << i;
        }
    }
    let mx_bit_col = (BMS - 1).min(n_col);
    for i in 0..mx_bit_col {
        if extra_cols & (1u64 << i) != 0 {
            n_key_col += 1;
        }
    }
    if col_used & (1u64 << (BMS - 1)) != 0 {
        n_key_col += n_col - BMS + 1;
    }

    // Construct the Index object to describe this index
    // pIdx = sqlite3AllocateIndexObject(pParse->db, nKeyCol+HasRowid(pTable), 0, &zNotUsed)
    let n_alloc = n_key_col + has_rowid as i32;
    let mut idx = Index {
        z_name: "auto-index".to_owned(),
        p_table: table,
        n_column: n_alloc as u16,
        n_key_col: (n_alloc - 1) as u16,
        ..Index::default()
    };
    let mut idx_cols: Bitmask = 0;
    for k in 0..n_term {
        let term = w.a_wc[wc].a[k].clone();
        if term_can_drive_index(p, &term, tab_list, i_from, not_ready) {
            let i_col = term.left_column;
            let c_mask: Bitmask = if i_col >= BMS { 1u64 << (BMS - 1) } else { 1u64 << i_col };
            if (idx_cols & c_mask) == 0 {
                let x = term.p_expr;
                idx_cols |= c_mask;
                idx.ai_column.push(term.left_column as i16);
                let coll = sqlite3_expr_compare_coll_seq(p, x);
                if !sqlite3_is_binary(coll.as_deref()) {
                    // Disallow the use of a Bloom filter if any non-BINARY collating sequence
                    // is used by any of the indexed fields. See tag-202607231411.
                    use_bloom_filter = false;
                }
                // pIdx->azColl[n] = pColl ? pColl->zName : sqlite3StrBINARY;
                idx.a_coll_static.push(coll.is_none());
                idx.az_coll.push(coll.map_or_else(|| "BINARY".to_owned(), |c| c.name.clone()));
            }
        }
    }
    let n_eq_cols = idx.ai_column.len();
    let _ = n_eq_cols;

    // Add additional columns needed to make the automatic index into a covering index
    for i in 0..mx_bit_col {
        if extra_cols & (1u64 << i) != 0 {
            idx.ai_column.push(i as i16);
            idx.az_coll.push("BINARY".to_owned());
            idx.a_coll_static.push(true);
        }
    }
    if col_used & (1u64 << (BMS - 1)) != 0 {
        for i in BMS - 1..n_col {
            idx.ai_column.push(i as i16);
            idx.az_coll.push("BINARY".to_owned());
            idx.a_coll_static.push(true);
        }
    }
    let n = idx.ai_column.len() as i32; // == nKeyCol
    if has_rowid {
        idx.ai_column.push(XN_ROWID as i16);
        idx.az_coll.push("BINARY".to_owned());
        idx.a_coll_static.push(true);
    }
    idx.a_sort_order = vec![0; idx.ai_column.len()];
    let idx_id = p.new_idx(idx);
    w.loops[i_loop].p_index = idx_id;

    // Create the automatic index
    // (explainAutomaticIndex(): SQLITE_ENABLE_STMT_SCANSTATUS only)
    let i_idx_cur = p.n_tab;
    p.n_tab += 1;
    w.a[i_level].i_idx_cur = i_idx_cur;
    p.v.add_op2(OP_OpenAutoindex, i_idx_cur, n_key_col + 1);
    sqlite3_vdbe_set_p4_key_info(p, idx_id);
    if p.db.optimization_enabled(SQLITE_BloomFilter) && use_bloom_filter {
        sqlite3_where_explain_bloom_filter(p, w, i_level);
        p.n_mem += 1;
        w.a[i_level].reg_filter = p.n_mem;
        p.v.add_op2(OP_Blob, 10000, w.a[i_level].reg_filter);
    }

    // Fill the automatic index with content
    let i_tab_cur = w.a[i_level].i_tab_cur;
    let via_coroutine = p.src(tab_list).a[i_from].via_coroutine;
    let addr_top;
    if via_coroutine {
        let (reg_yield, addr_fill_sub) = {
            let q = p.src(tab_list).a[i_from].p_subq.as_ref().expect("subquery");
            (q.reg_return, q.addr_fill_sub)
        };
        addr_counter = p.v.add_op2(OP_Integer, 0, 0);
        p.v.add_op3(OP_InitCoroutine, reg_yield, 0, addr_fill_sub);
        addr_top = p.v.add_op1(OP_Yield, reg_yield);
    } else {
        addr_top = p.v.add_op2(OP_Rewind, i_tab_cur, w.a[i_level].addr_halt);
    }
    if partial.nn() {
        i_continue = p.v.make_label();
        sqlite3_expr_if_false(p, partial, i_continue, SQLITE_JUMPIFNULL);
        w.loops[i_loop].ws_flags |= WHERE_PARTIALIDX as u32;
    }
    let reg_record = sqlite3_get_temp_reg(p);
    let reg_base = sqlite3_generate_index_key(p, idx_id, i_tab_cur, reg_record, false);
    if w.a[i_level].reg_filter != 0 {
        let n_eq = w.loops[i_loop].n_eq as i32;
        p.v.add_op4_int(OP_FilterAdd, w.a[i_level].reg_filter, 0, reg_base, n_eq);
    }
    p.v.add_op2(OP_IdxInsert, i_idx_cur, reg_record);
    p.v.change_p5(OPFLAG_USESEEKRESULT as u16);
    if partial.nn() {
        p.v.resolve_label(i_continue);
    }
    if via_coroutine {
        p.v.change_p2(addr_counter, reg_base + n);
        let reg_result = p.src(tab_list).a[i_from].p_subq.as_ref().expect("subquery").reg_result;
        translate_column_to_copy(p, addr_top, i_tab_cur, reg_result, i_idx_cur);
        p.v.goto(addr_top);
        p.srcm(tab_list).a[i_from].via_coroutine = false;
        p.v.jump_here(addr_top);
    } else {
        p.v.add_op2(OP_Next, i_tab_cur, addr_top + 1);
        p.v.change_p5(SQLITE_STMTSTATUS_AUTOINDEX as u16);
        if (p.src(tab_list).a[i_from].jointype as i32 & JT_LEFT) != 0 {
            p.v.jump_here(addr_top);
        }
    }
    sqlite3_release_temp_reg(p, reg_record);

    // Jump here when skipping the initialization
    p.v.jump_here(addr_init);
}

/// `SQLITE_STMTSTATUS_AUTOINDEX`
const SQLITE_STMTSTATUS_AUTOINDEX: i32 = 3;

/// `sqlite3LogEstToInt(x)`: convert a LogEst into an integer.
pub fn sqlite3_log_est_to_int(mut x: LogEst) -> u64 {
    let mut n = (x % 10) as u64;
    x /= 10;
    if n >= 5 {
        n -= 2;
    } else if n >= 1 {
        n -= 1;
    }
    if x > 60 {
        return i64::MAX as u64;
    }
    if x >= 3 { (n + 8) << (x - 3) } else { (n + 8) >> (3 - x) }
}

/// `sqlite3ConstructBloomFilter(pWInfo, iLevel, pLevel, notReady)`: generate bytecode that
/// will initialize a Bloom filter that is appropriate for pLevel.
///
/// If there are inner loops within pLevel that have the WHERE_BLOOMFILTER flag set, initialize
/// a Bloomfilter for them as well. Except, do not initialize a Bloom filter for a loop that is
/// the right operand of a LEFT JOIN or of some other outer join, or one that has prerequisites
/// that are not ready.
pub fn sqlite3_construct_bloom_filter(p: &mut Parse, w: &mut WhereInfo, mut i_level: usize, not_ready: Bitmask) {
    let saved_idx_epr = std::mem::take(&mut p.p_idx_epr);
    let saved_idx_part_expr = std::mem::take(&mut p.p_idx_part_expr);

    let addr_once = p.v.add_op0(OP_Once);
    loop {
        sqlite3_where_explain_bloom_filter(p, w, i_level);
        let addr_cont = p.v.make_label();
        let i_cur = w.a[i_level].i_tab_cur;
        p.n_mem += 1;
        w.a[i_level].reg_filter = p.n_mem;
        let reg_filter = w.a[i_level].reg_filter;
        let i_loop = w.a[i_level].p_w_loop;

        // The Bloom filter is a Blob held in a register. Initialize it to zero-filled blob
        // of at least 80K bits, but maybe more if the estimated size of the table is larger.
        // We could actually measure the size of the table at run-time using OP_Count with
        // P3==1 and use that value to initialize the blob. But that makes testing complicated.
        // By basing the blob size on the value in the sqlite_stat1 table, testing is much
        // easier.
        let tab_list = w.p_tab_list;
        let i_src = w.a[i_level].i_from as usize;
        let tab = p.src(tab_list).a[i_src].p_s_tab;
        let mut sz = sqlite3_log_est_to_int(p.tab(tab).n_row_log_est);
        if sz < 10000 {
            sz = 10000;
        } else if sz > 10_000_000 {
            sz = 10_000_000;
        }
        p.v.add_op2(OP_Blob, sz as i32, reg_filter);

        let addr_top = p.v.add_op1(OP_Rewind, i_cur);
        let n_term = w.a_wc[0].a.len();
        for k in 0..n_term {
            let (e, wt_flags) = {
                let t = &w.a_wc[0].a[k];
                (t.p_expr, t.wt_flags)
            };
            if (wt_flags & TERM_VIRTUAL) == 0 && sqlite3_expr_is_single_table_constraint(p, e, tab_list, i_src, false) {
                sqlite3_expr_if_false(p, e, addr_cont, SQLITE_JUMPIFNULL);
            }
        }
        if w.loops[i_loop].ws_flags & WHERE_IPK as u32 != 0 {
            let r1 = sqlite3_get_temp_reg(p);
            p.v.add_op2(OP_Rowid, i_cur, r1);
            p.v.add_op4_int(OP_FilterAdd, reg_filter, 0, r1, 1);
            sqlite3_release_temp_reg(p, r1);
        } else {
            let idx = w.loops[i_loop].p_index;
            let n = w.loops[i_loop].n_eq as i32;
            let r1 = sqlite3_get_temp_range(p, n);
            for jj in 0..n {
                sqlite3_expr_code_load_index_column(p, idx, i_cur, jj, r1 + jj);
            }
            p.v.add_op4_int(OP_FilterAdd, reg_filter, 0, r1, n);
            sqlite3_release_temp_range(p, r1, n);
        }
        p.v.resolve_label(addr_cont);
        p.v.add_op2(OP_Next, w.a[i_level].i_tab_cur, addr_top + 1);
        p.v.jump_here(addr_top);
        w.loops[i_loop].ws_flags &= !(WHERE_BLOOMFILTER as u32);
        loop {
            i_level += 1;
            if i_level >= w.n_level as usize {
                break;
            }
            let i_from = w.a[i_level].i_from as usize;
            if p.src(w.p_tab_list).a[i_from].jointype as i32 & (JT_LEFT | JT_LTORJ) != 0 {
                continue;
            }
            let l = &w.loops[w.a[i_level].p_w_loop];
            if l.prereq & not_ready != 0 {
                continue;
            }
            if (l.ws_flags & (WHERE_BLOOMFILTER | WHERE_COLUMN_IN) as u32) == WHERE_BLOOMFILTER as u32 {
                // This is a candidate for bloom-filter pull-down (early evaluation). The test
                // that WHERE_COLUMN_IN is omitted is important, as we are not able to do early
                // evaluation of bloom filters that make use of the IN operator
                break;
            }
        }
        if i_level >= w.n_level as usize {
            break;
        }
    }
    p.v.jump_here(addr_once);
    p.p_idx_epr = saved_idx_epr;
    p.p_idx_part_expr = saved_idx_part_expr;
}

/// `whereRangeAdjust(pTerm, nNew)`: if it is not NULL, pTerm is a term that provides an upper
/// or lower bound on a range scan. Without considering pTerm, it is estimated that the scan
/// will visit nNew rows. This function returns the number estimated to be visited after
/// taking pTerm into account.
///
/// If the user explicitly specified a likelihood() value for this term, then the return
/// value is the likelihood multiplied by the number of input rows. Otherwise, this function
/// assumes that an "IS NOT NULL" term has good selectivity and that a range constraint
/// reduces the search space by a factor of 4.
fn where_range_adjust(term: Option<&WhereTerm>, n_new: LogEst) -> LogEst {
    let mut n_ret = n_new;
    if let Some(t) = term {
        if t.truth_prob <= 0 {
            n_ret += t.truth_prob;
        } else if (t.wt_flags & TERM_VNULL) == 0 {
            n_ret -= 20;
        }
    }
    n_ret
}

/// `whereRangeScanEst(pParse, pBuilder, pLower, pUpper, pLoop)` without STAT4: this function
/// is used to estimate the number of rows that will be visited by scanning an index for a
/// range of values. The range may have an upper bound, a lower bound, or both.
///
/// In the absence of sqlite_stat4 ANALYZE data, or if such data cannot be used, a single
/// range inequality reduces the search space by a factor of 4, and a pair of constraints
/// (x>? AND x<?) reduces the expected number of rows visited by a factor of 64.
fn where_range_scan_est(lower: Option<&WhereTerm>, upper: Option<&WhereTerm>, l: &mut WhereLoop) {
    let mut n_out = l.n_out as i32;
    let mut n_new = where_range_adjust(lower, n_out as LogEst);
    n_new = where_range_adjust(upper, n_new);

    // TUNING: If there is both an upper and lower limit and neither limit has an
    // application-defined likelihood(), assume the range is reduced by an additional 75%.
    // This means that, by default, an open-ended range query (e.g. col > ?) is assumed to
    // match 1/4 of the rows in the index. While a closed range (e.g. col BETWEEN ? AND ?) is
    // estimated to match 1/64 of the index.
    if let (Some(lo), Some(up)) = (lower, upper)
        && lo.truth_prob > 0
        && up.truth_prob > 0
    {
        n_new -= 20;
    }

    n_out -= lower.is_some() as i32 + upper.is_some() as i32;
    if n_new < 10 {
        n_new = 10;
    }
    if (n_new as i32) < n_out {
        n_out = n_new as i32;
    }
    l.n_out = n_out as LogEst;
}

/// `whereLoopCheaperProperSubset(pX, pY)`: return TRUE if X is a proper subset of Y but is of
/// equal or less cost. In other words, return true if all of the following are true:
///
///   (1a) X and Y use the same index.
///   (1b) X has fewer == terms than Y
///   (1c) Neither X nor Y use skip-scan
///   (1d) X does not have a a greater cost than Y
///
/// Or (case 2):
///
///   (2a) X has the same or lower cost, or returns the same or fewer rows, than Y.
///   (2b) X uses fewer WHERE clause terms than Y
///   (2c) Every WHERE clause term used by X is also used by Y
///   (2d) X skips at least as many columns as Y
///   (2e) If X is a covering index, than Y is too
fn where_loop_cheaper_proper_subset(x: &WhereLoop, y: &WhereLoop) -> bool {
    if x.r_run > y.r_run && x.n_out > y.n_out {
        return false; // (1d) and (2a)
    }
    if x.n_eq < y.n_eq // (1b)
        && x.p_index == y.p_index // (1a)
        && x.n_skip == 0 && y.n_skip == 0
    // (1c)
    {
        return true; // Case 1 is true
    }
    if x.n_l_term() as i32 - x.n_skip as i32 >= y.n_l_term() as i32 - y.n_skip as i32 {
        return false; // (2b)
    }
    if y.n_skip > x.n_skip {
        return false; // (2d)
    }
    for i in (0..x.n_l_term()).rev() {
        let Some(xt) = x.a_l_term[i] else { continue };
        if !y.a_l_term.iter().any(|t| *t == Some(xt)) {
            return false; // (2c)
        }
    }
    if (x.ws_flags & WHERE_IDX_ONLY as u32) != 0 && (y.ws_flags & WHERE_IDX_ONLY as u32) == 0 {
        return false; // (2e)
    }
    true // Case 2 is true
}

/// `whereLoopAdjustCost(p, pTemplate)`: try to adjust the cost and number of output rows of
/// WhereLoop pTemplate upwards or downwards so that:
///
///   (1) pTemplate costs less than any other WhereLoops that are a proper subset of pTemplate
///
///   (2) pTemplate costs more than any other WhereLoops for which pTemplate is a proper
///       subset.
///
/// To say "WhereLoop X is a proper subset of Y" means that X uses fewer WHERE clause terms
/// than Y and that every WHERE clause term used by X is also used by Y.
fn where_loop_adjust_cost(loops: &[WhereLoop], template: &mut WhereLoop) {
    if (template.ws_flags & WHERE_INDEXED as u32) == 0 {
        return;
    }
    for l in loops {
        if l.i_tab != template.i_tab {
            continue;
        }
        if (l.ws_flags & WHERE_INDEXED as u32) == 0 {
            continue;
        }
        if where_loop_cheaper_proper_subset(l, template) {
            // Adjust pTemplate cost downward so that it is cheaper than its subset p.
            template.r_run = l.r_run.min(template.r_run);
            template.n_out = (l.n_out - 1).min(template.n_out);
        } else if where_loop_cheaper_proper_subset(template, l) {
            // Adjust pTemplate cost upward so that it is costlier than p since pTemplate is a
            // proper subset of p
            template.r_run = l.r_run.max(template.r_run);
            template.n_out = (l.n_out + 1).max(template.n_out);
        }
    }
}

/// `whereLoopFindLesser(ppPrev, pTemplate)`: search the list of WhereLoops in *ppPrev looking
/// for one that can be replaced by pTemplate.
///
/// Return NULL (`Err(())`) if pTemplate does not belong on the WhereLoop list at all because
/// it is no better than at least one WhereLoop already on the list.
///
/// If pX is a WhereLoop that pTemplate can replace, then return its position. If pTemplate
/// cannot replace any existing element of the list but needs to be added to the WhereLoop
/// list, then return the end of the list (`loops.len()`).
fn where_loop_find_lesser(loops: &[WhereLoop], start: usize, template: &WhereLoop) -> Result<usize, ()> {
    let mut i = start;
    while i < loops.len() {
        let l = &loops[i];
        if l.i_tab != template.i_tab || l.i_sort_idx != template.i_sort_idx {
            // If either the iTab or iSortIdx values for two WhereLoop are different then
            // those WhereLoops need to be considered separately. Neither is a candidate to
            // replace the other.
            i += 1;
            continue;
        }
        // whereLoopAddBtree() always generates and inserts the automatic index case first.
        // Hence compatible candidate WhereLoops never have a larger rSetup.

        // Any loop using an application-defined index (or PRIMARY KEY or UNIQUE constraint)
        // with one or more == constraints is better than an automatic index. Unless it is a
        // skip-scan.
        if (l.ws_flags & WHERE_AUTO_INDEX as u32) != 0
            && template.n_skip == 0
            && (template.ws_flags & WHERE_INDEXED as u32) != 0
            && (template.ws_flags & WHERE_COLUMN_EQ as u32) != 0
            && (l.prereq & template.prereq) == template.prereq
        {
            break;
        }

        // If existing WhereLoop p is better than pTemplate, pTemplate can be discarded.
        // WhereLoop p is better if:
        //   (1)  p has no more dependencies than pTemplate, and
        //   (2)  p has an equal or lower cost than pTemplate
        if (l.prereq & template.prereq) == l.prereq // (1)
            && l.r_setup <= template.r_setup // (2a)
            && l.r_run <= template.r_run // (2b)
            && l.n_out <= template.n_out
        // (2c)
        {
            return Err(()); // Discard pTemplate
        }

        // If pTemplate is always better than p, then cause p to be overwritten with
        // pTemplate. pTemplate is better than p if:
        //   (1)  pTemplate has no more dependencies than p, and
        //   (2)  pTemplate has an equal or lower cost than p.
        if (l.prereq & template.prereq) == template.prereq // (1)
            && l.r_run >= template.r_run // (2a)
            && l.n_out >= template.n_out
        // (2b)
        {
            break; // Cause p to be overwritten by pTemplate
        }
        i += 1;
    }
    Ok(i)
}

/// One line of the WhereLoop dump (verification aid): the values of a loop offered to
/// `whereLoopInsert()` and what became of it.
fn dump_loop(p: &mut Parse, w: &WhereInfo, what: &str, l: &WhereLoop) {
    if p.where_dump.is_none() {
        return;
    }
    let _ = w;
    if l.ws_flags & WHERE_VIRTUALTABLE as u32 != 0 {
        // (V4b: the u.vtab fields of a virtual-table loop)
        let line = format!(
            "LOOP {} tab={} idx=- ws={:x} vtab idxNum={} idxStr={} isOrdered={} omit={:x} hex={} hIn={:x} omOff={} nSkip={} nLTerm={} iSortIdx={} rSetup={} rRun={} nOut={} prereq={:x}",
            what,
            l.i_tab,
            l.ws_flags,
            l.vt_idx_num,
            l.vt_idx_str.as_deref().unwrap_or("(null)"),
            l.vt_is_ordered,
            l.vt_omit_mask,
            u8::from(l.vt_b_idx_num_hex),
            l.vt_m_handle_in,
            u8::from(l.vt_b_omit_offset),
            l.n_skip,
            l.n_l_term(),
            l.i_sort_idx,
            l.r_setup,
            l.r_run,
            l.n_out,
            l.prereq
        );
        if let Some(d) = p.where_dump.as_mut() {
            d.push(line);
        }
        return;
    }
    let name = if l.p_index.nn() && !p.idx(l.p_index).z_name.is_empty() { p.idx(l.p_index).z_name.clone() } else { "-".to_owned() };
    let line = format!(
        "LOOP {} tab={} idx={} ws={:x} nEq={} nBtm={} nTop={} nSkip={} nLTerm={} iSortIdx={} rSetup={} rRun={} nOut={} prereq={:x}",
        what,
        l.i_tab,
        name,
        l.ws_flags,
        l.n_eq,
        l.n_btm,
        l.n_top,
        l.n_skip,
        l.n_l_term(),
        l.i_sort_idx,
        l.r_setup,
        l.r_run,
        l.n_out,
        l.prereq
    );
    if let Some(d) = p.where_dump.as_mut() {
        d.push(line);
    }
}

/// `whereLoopInsert(pBuilder, pTemplate)`: insert or replace a WhereLoop entry using the
/// template supplied.
///
/// An existing WhereLoop entry might be overwritten if the new template is better and has
/// fewer dependencies. Or the template will be ignored and no insert will occur if an
/// existing WhereLoop is faster and has fewer dependencies. Or a new WhereLoop will be
/// inserted if the template is unique.
///
/// When accumulating multiple loops (when pBuilder->pOrSet is NULL) we still might overwrite
/// similar loops with the new template if the new template is better. Loops may be
/// overwritten if the following conditions are met:
///
///    (1)  They have the same iTab.
///    (2)  They have the same iSortIdx.
///    (3)  The template has same or fewer dependencies than the current loop
///    (4)  The template has the same or lower cost than the current loop
fn where_loop_insert(p: &mut Parse, w: &mut WhereInfo, b: &mut WhereLoopBuilder) -> i32 {
    // Stop the search once we hit the query planner search limit
    if b.i_plan_limit == 0 {
        if let Some(s) = b.p_or_set.as_mut() {
            s.n = 0;
        }
        return SQLITE_DONE;
    }
    b.i_plan_limit -= 1;

    where_loop_adjust_cost(&w.loops, &mut b.p_new);

    // If pBuilder->pOrSet is defined, then only keep track of the costs and prereqs.
    if let Some(set) = b.p_or_set.as_mut() {
        if b.p_new.n_l_term() != 0 {
            let x = where_or_insert(set, b.p_new.prereq, b.p_new.r_run, b.p_new.n_out);
            let t = b.p_new.clone();
            dump_loop(p, w, if x { "or" } else { "or-X" }, &t);
        }
        return SQLITE_OK;
    }

    // Look for an existing WhereLoop to replace with pTemplate
    let pos = match where_loop_find_lesser(&w.loops, 0, &b.p_new) {
        Err(()) => {
            // There already exists a WhereLoop on the list that is better than pTemplate, so
            // just ignore pTemplate
            let t = b.p_new.clone();
            dump_loop(p, w, "skip", &t);
            return SQLITE_OK;
        }
        Ok(pos) => pos,
    };

    // If we reach this point it means that either p[] should be overwritten with pTemplate[]
    // if p[] exists, or if p==NULL then allocate a new WhereLoop and insert it before pNext.
    if pos >= w.loops.len() {
        // Allocate a new WhereLoop to add to the end of the list
        let t = b.p_new.clone();
        dump_loop(p, w, "add", &t);
        w.loops.push(WhereLoop::default());
    } else {
        let old = w.loops[pos].clone();
        dump_loop(p, w, "replace", &old);
        let t = b.p_new.clone();
        dump_loop(p, w, "with", &t);
        // We will be overwriting WhereLoop p[]. But before we do, first go through the rest
        // of the list and delete any other entries besides p[] that are also supplanted by
        // pTemplate
        let mut tail = pos + 1;
        while tail < w.loops.len() {
            match where_loop_find_lesser(&w.loops, tail, &b.p_new) {
                Err(()) => break,
                Ok(k) => {
                    if k >= w.loops.len() {
                        break;
                    }
                    let del = w.loops.remove(k);
                    dump_loop(p, w, "delete", &del);
                    tail = k;
                }
            }
        }
    }
    // whereLoopXfer(db, p, pTemplate)
    w.loops[pos] = b.p_new.clone();
    if (b.p_new.ws_flags & WHERE_VIRTUALTABLE as u32) != 0 {
        b.p_new.vt_need_free = false;
    } else if (b.p_new.ws_flags & WHERE_AUTO_INDEX as u32) != 0 {
        b.p_new.p_index = IndexId::NULL;
    }
    if (w.loops[pos].ws_flags & WHERE_VIRTUALTABLE as u32) == 0 {
        let idx = w.loops[pos].p_index;
        if idx.nn() && p.idx(idx).idx_type == SQLITE_IDXTYPE_IPK as u8 {
            w.loops[pos].p_index = IndexId::NULL;
        }
    }
    SQLITE_OK
}

/// `exprNodePatternLengthEst` / `estLikePatternLength(p, eCode)`: return the length (in
/// non-wildcard characters) of the longest string literal of a LIKE (`e_code!=0`) or GLOB
/// pattern expression.
fn est_like_pattern_length(p: &mut Parse, e: ExprId, e_code: u16) -> i32 {
    fn cb(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
        if p.e(e).op == TK_STRING {
            let mut sz = 0; // Pattern size in bytes
            let z = p.e(e).tok().as_bytes();
            let (c1, c2, c3) = if w.e_code != 0 { (b'%', b'_', 0u8) } else { (b'*', b'?', b'[') };
            let mut i = 0;
            while i < z.len() {
                let c = z[i];
                i += 1;
                if c == c3 {
                    if i < z.len() {
                        i += 1;
                    }
                    while i < z.len() && z[i] != b']' {
                        i += 1;
                    }
                } else if c != c1 && c != c2 {
                    sz += 1;
                }
            }
            if sz > w.n {
                w.n = sz;
            }
        }
        WRC_Continue
    }
    let mut w = Walker {
        n: 0,
        e_code,
        x_expr_callback: Some(cb),
        x_select_callback: Some(sqlite3_select_walk_fail),
        ..Walker::default()
    };
    sqlite3_walk_expr(p, &mut w, e);
    w.n
}

/// `sqlite3ExprIsLikeOperator(pExpr)`: if pExpr is one of "like", "glob", "match", or
/// "regexp", then return the corresponding SQLITE_INDEX_CONSTRAINT_xxxx value. If it is not
/// a LIKE-like operator, return 0.
pub fn sqlite3_expr_is_like_operator(p: &Parse, e: ExprId) -> i32 {
    let z = p.e(e).tok();
    if z.eq_ignore_ascii_case("match") {
        64 // SQLITE_INDEX_CONSTRAINT_MATCH
    } else if z.eq_ignore_ascii_case("glob") {
        66 // SQLITE_INDEX_CONSTRAINT_GLOB
    } else if z.eq_ignore_ascii_case("like") {
        65 // SQLITE_INDEX_CONSTRAINT_LIKE
    } else if z.eq_ignore_ascii_case("regexp") {
        67 // SQLITE_INDEX_CONSTRAINT_REGEXP
    } else {
        0
    }
}

/// `whereLoopOutputAdjust(pWC, pLoop, nRow)`: adjust the WhereLoop.nOut value downward to
/// account for terms of the WHERE clause that reference only the table of pLoop and that are
/// not used by an index: such terms filter rows.
///
/// For every WHERE clause term that is not used by the index and which has a truth
/// probability assigned by one of the likelihood(), likely(), or unlikely() SQL functions,
/// reduce the estimated number of output rows by the probability specified.
///
/// TUNING: For every WHERE clause term that is not used by the index and which does not have
/// an assigned truth probability, heuristics described below are used to try to estimate the
/// truth probability. TODO --> Perhaps this is something that could be improved by better
/// table statistics.
///
/// Heuristic 1: Estimate the truth probability as 93.75%. The 93.75% value corresponds to -1
/// in LogEst notation, so this means decrement the WhereLoop.nOut field for every such WHERE
/// clause term.
///
/// Heuristic 2: If there exists one or more WHERE clause terms of the form "x==EXPR" and EXPR
/// is not a constant 0 or 1, then make sure the final output row estimate is no greater than
/// 1/4 of the total number of rows in the table. In other words, assume that x==EXPR will
/// filter out at least 3 out of 4 rows. If EXPR is -1 or 0 or 1, then maybe the "x" column
/// is boolean or else -1 or 0 or 1 is a common default value on the "x" column and so in
/// that case only cap the output row estimate at 1/2 instead of 1/4.
///
/// Heuristic 3: If there is a LIKE or GLOB (or REGEXP or MATCH) operator on the LHS of the
/// WHERE clause term and the RHS has a string-constant pattern, reduce the output estimate
/// by the length of that pattern.
fn where_loop_output_adjust(p: &mut Parse, w: &mut WhereInfo, wc: WcId, l: &mut WhereLoop, n_row: LogEst) {
    let not_allowed: Bitmask = !(l.prereq | l.mask_self);
    let mut i_reduce: LogEst = 0; // pLoop->nOut should not exceed nRow-iReduce

    // Skip all this if the FROM clause of the query is a single table and there is no ORDER
    // BY: the nOut value cannot change the plan and is not shown.
    if p.src(w.p_tab_list).a.len() <= 1 && w.p_order_by.is_null() {
        return;
    }
    let n_base = w.a_wc[wc].n_base as usize;
    for k in 0..n_base {
        let term = w.a_wc[wc].a[k].clone();
        if (term.prereq_all & not_allowed) != 0 {
            continue;
        }
        if (term.prereq_all & l.mask_self) == 0 {
            continue;
        }
        if (term.wt_flags & TERM_VIRTUAL) != 0 {
            continue;
        }
        let this = TermRef::new(wc, k);
        let mut found = false;
        for j in (0..l.n_l_term()).rev() {
            let Some(x) = l.a_l_term[j] else { continue };
            if x == this {
                found = true;
                break;
            }
            // if( pX->iParent>=0 && (&pWC->a[pX->iParent])==pTerm ) break;
            let xp = w.term(x).i_parent;
            if xp >= 0 && xp as usize == k {
                found = true;
                break;
            }
        }
        if !found {
            if l.mask_self == term.prereq_all {
                // If there are extra terms in the WHERE clause not used by an index that
                // depend only on the table being scanned, and that will tend to cause many
                // rows to be omitted, then mark that table as "self-culling".
                //
                // 2022-03-24: Self-culling only applies if either the extra terms are
                // straight comparison operators that are non-true with NULL operand, or if
                // the loop is not an OUTER JOIN.
                if (term.e_operator & 0x3f) != 0
                    || (p.src(w.p_tab_list).a[l.i_tab as usize].jointype as i32 & (JT_LEFT | JT_LTORJ)) == 0
                {
                    l.ws_flags |= WHERE_SELFCULL as u32;
                }
            }
            if term.truth_prob <= 0 {
                // If a truth probability is specified using the likelihood() hints, then use
                // the probability as-is
                l.n_out += term.truth_prob;
            } else {
                // In the absence of explicit truth probabilities, use heuristics to guess a
                // reasonable truth probability.
                let op_expr = term.p_expr;
                l.n_out -= 1;
                if (term.e_operator & (WO_EQ | WO_IS)) != 0 && (term.wt_flags & TERM_HIGHTRUTH) == 0 {
                    let right = p.e(op_expr).p_right;
                    let k2 = match sqlite3_expr_is_integer(p, right, true, true) {
                        Some(v) if (-1..=1).contains(&v) => 10,
                        _ => 20,
                    };
                    if i_reduce < k2 {
                        w.a_wc[wc].a[k].wt_flags |= TERM_HEURTRUTH;
                        i_reduce = k2;
                    }
                } else if p.e(op_expr).has(EP_InfixFunc) && p.e(op_expr).op == TK_FUNCTION {
                    let e_op = sqlite3_expr_is_like_operator(p, op_expr);
                    if e_op > 0 {
                        let rhs = p.l(p.e(op_expr).x_list()).a[0].p_expr;
                        let sz_pattern = est_like_pattern_length(p, rhs, (e_op == 65) as u16);
                        if sz_pattern > 0 {
                            l.n_out -= (sz_pattern * 2) as LogEst;
                        }
                    }
                }
            }
        }
    }
    if l.n_out > n_row - i_reduce {
        l.n_out = n_row - i_reduce;
    }
}

/// `whereRangeVectorLen(pParse, iCur, pIdx, nEq, pTerm)`: term pTerm is a vector range
/// comparison operation. The first comparison in the vector can be processed as part of
/// index pIdx with cursor iCur (the left-most column of the vector is index column nEq).
/// This function determines how many of the following comparisons can be processed as part
/// of the same range constraint: the number of terms of the vector that match the index.
fn where_range_vector_len(p: &mut Parse, i_cur: i32, idx: IndexId, n_eq: i32, term: &WhereTerm) -> i32 {
    let e = term.p_expr;
    let mut n_cmp = sqlite3_expr_vector_size(p, p.e(e).p_left);
    n_cmp = n_cmp.min(p.idx(idx).n_column as i32 - n_eq);
    let mut i = 1;
    while i < n_cmp {
        // Test if comparison i of pTerm is compatible with column (i+nEq) of the index. If
        // not, exit the loop.
        let mut lhs = p.l(p.e(p.e(e).p_left).x_list()).a[i as usize].p_expr;
        let r0 = p.e(e).p_right;
        let mut rhs = if p.e(r0).use_x_select() {
            p.l(p.s(p.e(r0).x_select()).p_e_list).a[i as usize].p_expr
        } else {
            p.l(p.e(r0).x_list()).a[i as usize].p_expr
        };

        // Check that the LHS of the comparison is a column reference to the right column of
        // the right source table. And that the sort order of the index column is the same as
        // the sort order of the leftmost index column.
        let k = (i + n_eq) as usize;
        if p.e(lhs).op != TK_COLUMN
            || p.e(lhs).i_table != i_cur
            || p.e(lhs).i_column != p.idx(idx).ai_column[k]
            || p.idx(idx).a_sort_order[k] != p.idx(idx).a_sort_order[n_eq as usize]
        {
            break;
        }

        let aff = sqlite3_compare_affinity(p, rhs, sqlite3_expr_affinity(p, lhs));
        let idxaff = sqlite3_table_column_affinity(p, p.idx(idx).p_table, p.e(lhs).i_column as i32);
        if aff != idxaff {
            break;
        }
        if p.e(e).has(EP_Commuted) {
            std::mem::swap(&mut rhs, &mut lhs);
        }
        let Some(coll) = sqlite3_binary_compare_coll_seq(p, lhs, rhs) else { break };
        if !coll.name.eq_ignore_ascii_case(&p.idx(idx).az_coll[k]) {
            break;
        }
        i += 1;
    }
    i
}

/// `whereLoopAddBtreeIndex(pBuilder, pSrc, pProbe, nInMul)`: we have so far matched
/// pBuilder->pNew->u.btree.nEq terms of the index pIndex. Try to match one more.
///
/// When this function is called, pBuilder->pNew->nOut contains the number of rows expected
/// to be visited by the index scan before considering term pTerm, or the values of nIn and
/// nInMul. In other words, assuming that all "x IN(...)" terms are replaced with "x = ?".
/// This function adjusts the estimate by considering the term and the nInMul value.
///
/// If pProbe->idxType==SQLITE_IDXTYPE_IPK, that means pIndex is a fake index used for the
/// INTEGER PRIMARY KEY.
fn where_loop_add_btree_index(
    p: &mut Parse,
    w: &mut WhereInfo,
    b: &mut WhereLoopBuilder,
    i_src: usize,
    probe: IndexId,
    n_in_mul: LogEst,
) -> i32 {
    let mut rc = SQLITE_OK;
    let mut p_top: Option<TermRef> = None;
    let mut p_btm: Option<TermRef> = None; // Top and bottom range constraints
    if p.n_err != 0 {
        return p.rc;
    }
    let src = p.src(w.p_tab_list).a[i_src].clone();
    let mut op_mask: u16 = if b.p_new.ws_flags & WHERE_BTM_LIMIT as u32 != 0 {
        WO_LT | WO_LE
    } else {
        WO_EQ | WO_IN | WO_GT | WO_GE | WO_LT | WO_LE | WO_ISNULL | WO_IS
    };
    if p.idx(probe).b_unordered {
        op_mask &= !(WO_GT | WO_GE | WO_LT | WO_LE);
    }

    let saved_n_eq = b.p_new.n_eq;
    let saved_n_btm = b.p_new.n_btm;
    let saved_n_top = b.p_new.n_top;
    let saved_n_skip = b.p_new.n_skip;
    let saved_n_l_term = b.p_new.n_l_term();
    let saved_ws_flags = b.p_new.ws_flags;
    let saved_prereq = b.p_new.prereq;
    let saved_n_out = b.p_new.n_out;
    let mut scan = WhereScan::default();
    let mut term_opt = where_scan_init(p, w, &mut scan, b.p_wc, src.i_cursor, saved_n_eq as i32, op_mask as u32, probe);
    b.p_new.r_setup = 0;
    let r_size = p.idx(probe).ai_row_log_est[0];
    let r_log_size = est_log(r_size);
    while rc == SQLITE_OK {
        let Some(tr) = term_opt else { break };
        let term = w.term(tr).clone();
        let e_op = term.e_operator; // Shorthand for pTerm->eOperator
        let mut n_in: LogEst = 0;
        macro_rules! next {
            () => {{
                term_opt = where_scan_next(p, w, &mut scan);
                continue;
            }};
        }
        if (e_op == WO_ISNULL || (term.wt_flags & TERM_VNULL) != 0) && index_column_not_null(p, probe, saved_n_eq as usize) {
            next!(); // ignore IS [NOT] NULL constraints on NOT NULL columns
        }
        if term.prereq_right & b.p_new.mask_self != 0 {
            next!();
        }

        // Do not allow the upper bound of a LIKE optimization range constraint to mix with a
        // lower range bound from some other source
        if term.wt_flags & TERM_LIKEOPT != 0 && term.e_operator == WO_LT {
            next!();
        }

        if (src.jointype as i32 & (JT_LEFT | JT_LTORJ | JT_RIGHT)) != 0 && !constraint_compatible_with_outer_join(p, &term, &src) {
            next!();
        }
        if p.idx(probe).is_unique() && saved_n_eq as i32 == p.idx(probe).n_key_col as i32 - 1 {
            b.bld_flags1 |= SQLITE_BLDF1_UNIQUE;
        } else {
            b.bld_flags1 |= SQLITE_BLDF1_INDEXED;
        }
        b.p_new.ws_flags = saved_ws_flags;
        b.p_new.n_eq = saved_n_eq;
        b.p_new.n_btm = saved_n_btm;
        b.p_new.n_top = saved_n_top;
        b.p_new.a_l_term.truncate(saved_n_l_term);
        b.p_new.a_l_term.push(Some(tr));
        b.p_new.prereq = (saved_prereq | term.prereq_right) & !b.p_new.mask_self;

        if e_op & WO_IN != 0 {
            let e = term.p_expr;
            if p.e(e).use_x_select() {
                // "x IN (SELECT ...)": TUNING: the SELECT returns 25 rows
                let mut b_redundant = false;
                n_in = 46;

                // The expression may actually be of the form (x, y) IN (SELECT...). In this
                // case there is a separate term for each of (x) and (y). However, the nIn
                // multiplier should only be applied once, not once for each such term. The
                // following loop checks that pTerm is the first such term in use, and sets
                // nIn back to 0 if it is not.
                for i in 0..b.p_new.n_l_term() - 1 {
                    if let Some(t) = b.p_new.a_l_term[i]
                        && w.term(t).p_expr == e
                    {
                        n_in = 0;
                        if w.term(t).i_field == term.i_field {
                            // Detect when two or more columns of an index match the same
                            // column of a vector IN operater, and avoid adding the column to
                            // the WhereLoop more than once. See tag-20250707-01 in
                            // test/rowvalue.test
                            b_redundant = true;
                        }
                    }
                }
                if b_redundant {
                    b.p_new.a_l_term.pop();
                    next!();
                }
            } else if p.e(e).x_list().nn() && p.l(p.e(e).x_list()).n_expr() != 0 {
                // "x IN (value, value, ...)"
                n_in = sqlite3_log_est(p.l(p.e(e).x_list()).n_expr() as u64);
            }
            if p.idx(probe).has_stat1 && r_log_size >= 10 {
                // Let:
                //   N = the total number of rows in the table
                //   K = the number of entries on the RHS of the IN operator
                //   M = the number of rows in the table that match terms to the to the left
                //       of the IN operator
                // Then, the average cost per row of a scan is close to 1.0. The cost of each
                // of the K lookups is about log2(N). So the cost of the IN operator is
                // roughly:
                //                  K*log2(N) + M
                // The cost of a full scan is M. Use the IN operator only if the total number
                // of rows visited by the indexed lookups is less than doing the full scan.
                let m = p.idx(probe).ai_row_log_est[saved_n_eq as usize];
                let log_k = est_log(n_in);
                // TUNING      v-----  10 to bias toward indexed IN
                let x = m + log_k + 10 - (n_in + r_log_size);
                if x >= 0 {
                    // prefers indexed lookup
                } else if n_in_mul < 2 && p.db.optimization_enabled(SQLITE_SeekScan) {
                    // prefers skip-scan
                    b.p_new.ws_flags |= WHERE_IN_SEEKSCAN as u32;
                } else {
                    // prefers normal scan
                    next!();
                }
            }
            b.p_new.ws_flags |= WHERE_COLUMN_IN as u32;
        } else if e_op & (WO_EQ | WO_IS) != 0 {
            let i_col = p.idx(probe).ai_column[saved_n_eq as usize] as i32;
            b.p_new.ws_flags |= WHERE_COLUMN_EQ as u32;
            if i_col == XN_ROWID || (i_col >= 0 && n_in_mul == 0 && saved_n_eq as i32 == p.idx(probe).n_key_col as i32 - 1) {
                let pr = p.idx(probe);
                if i_col == XN_ROWID || pr.uniq_not_null || (pr.n_key_col == 1 && pr.on_error != 0 && (e_op & WO_EQ) != 0) {
                    b.p_new.ws_flags |= WHERE_ONEROW as u32;
                } else {
                    b.p_new.ws_flags |= WHERE_UNQ_WANTED as u32;
                }
            }
            if scan.i_equiv > 1 {
                b.p_new.ws_flags |= WHERE_TRANSCONS as u32;
            }
        } else if e_op & WO_ISNULL != 0 {
            b.p_new.ws_flags |= WHERE_COLUMN_NULL as u32;
        } else {
            let n_vec_len = where_range_vector_len(p, src.i_cursor, probe, saved_n_eq as i32, &term);
            if e_op & (WO_GT | WO_GE) != 0 {
                b.p_new.ws_flags |= (WHERE_COLUMN_RANGE | WHERE_BTM_LIMIT) as u32;
                b.p_new.n_btm = n_vec_len as u16;
                p_btm = Some(tr);
                p_top = None;
                if term.wt_flags & TERM_LIKEOPT != 0 {
                    // Range constraints that come from the LIKE optimization are always used
                    // in pairs.
                    let top = TermRef { wc: tr.wc, i: tr.i + 1 };
                    p_top = Some(top);
                    b.p_new.a_l_term.push(Some(top));
                    b.p_new.ws_flags |= WHERE_TOP_LIMIT as u32;
                    b.p_new.n_top = 1;
                }
            } else {
                b.p_new.ws_flags |= (WHERE_COLUMN_RANGE | WHERE_TOP_LIMIT) as u32;
                b.p_new.n_top = n_vec_len as u16;
                p_top = Some(tr);
                p_btm = if (b.p_new.ws_flags & WHERE_BTM_LIMIT as u32) != 0 {
                    b.p_new.a_l_term[b.p_new.n_l_term() - 2]
                } else {
                    None
                };
            }
        }

        // At this point pNew->nOut is set to the number of rows expected to be visited by
        // the index scan before considering term pTerm, or the values of nIn and nInMul. In
        // other words, assuming that all "x IN(...)" terms are replaced with "x = ?". This
        // block updates the value of pNew->nOut to account for pTerm (but not nIn/nInMul).
        if b.p_new.ws_flags & WHERE_COLUMN_RANGE as u32 != 0 {
            // Adjust nOut using stat4 data. Or, if there is no stat4 data, using some other
            // estimate.
            let lo = p_btm.map(|t| w.term(t).clone());
            let up = p_top.map(|t| w.term(t).clone());
            where_range_scan_est(lo.as_ref(), up.as_ref(), &mut b.p_new);
        } else {
            b.p_new.n_eq += 1;
            let n_eq = b.p_new.n_eq as usize;
            if term.truth_prob <= 0 && p.idx(probe).ai_column[saved_n_eq as usize] >= 0 {
                b.p_new.n_out += term.truth_prob;
                b.p_new.n_out -= n_in;
            } else {
                let ix = p.idx(probe);
                let a = &ix.ai_row_log_est;
                // (a resized index: aiRowLogEst[nColumn] is the memory of aiColumn[0])
                let a_n_eq = if ix.is_resized && n_eq == ix.n_column as usize { ix.ai_column[0] } else { a[n_eq] };
                b.p_new.n_out += a_n_eq - a[n_eq - 1];
                if e_op & WO_ISNULL != 0 {
                    // TUNING: If there is no likelihood() value, assume that a "col IS NULL"
                    // expression matches twice as many rows as (col=?).
                    b.p_new.n_out += 10;
                }
            }
        }

        // Set rCostIdx to the estimated cost of visiting selected rows in the index. The
        // estimate is the sum of two values:
        //   1.  The cost of doing one search-by-key to find the first matching entry
        //   2.  Stepping forward in the index pNew->nOut times to find all additional
        //       matching entries.
        let mut r_cost_idx: LogEst;
        if p.idx(probe).idx_type == SQLITE_IDXTYPE_IPK as u8 {
            // The pProbe->szIdxRow is low for an IPK table since the interior pages are
            // small. Thus szIdxRow gives a good estimate of seek cost. But the leaf pages are
            // full-size, so pProbe->szIdxRow would badly under-estimate the scanning cost.
            r_cost_idx = b.p_new.n_out + 16;
        } else {
            let sz_tab_row = p.tab(src.p_s_tab).sz_tab_row as i32;
            r_cost_idx = b.p_new.n_out + 1 + ((15 * p.idx(probe).sz_idx_row as i32) / sz_tab_row) as LogEst;
        }
        r_cost_idx = sqlite3_log_est_add(r_log_size, r_cost_idx);

        // Estimate the cost of running the loop. If all data is coming from the index, then
        // this is just the cost of doing the index lookup and scan. But if some data is
        // coming out of the main table, we also have to add in the cost of doing pNew->nOut
        // searches to locate the row in the main table corresponding to the index entry.
        b.p_new.r_run = r_cost_idx;
        if (b.p_new.ws_flags & (WHERE_IDX_ONLY | WHERE_IPK | WHERE_EXPRIDX) as u32) == 0 {
            b.p_new.r_run = sqlite3_log_est_add(b.p_new.r_run, b.p_new.n_out + 16);
        }

        let n_out_unadjusted = b.p_new.n_out; // nOut before IN() and WHERE adjustments
        b.p_new.r_run += n_in_mul + n_in;
        b.p_new.n_out += n_in_mul + n_in;
        {
            let mut l = std::mem::take(&mut b.p_new);
            where_loop_output_adjust(p, w, b.p_wc, &mut l, r_size);
            b.p_new = l;
        }
        if src.from_exists {
            b.p_new.n_out = 0;
        }
        rc = where_loop_insert(p, w, b);

        if b.p_new.ws_flags & WHERE_COLUMN_RANGE as u32 != 0 {
            b.p_new.n_out = saved_n_out;
        } else {
            b.p_new.n_out = n_out_unadjusted;
        }

        if (b.p_new.ws_flags & WHERE_TOP_LIMIT as u32) == 0
            && (b.p_new.n_eq as i32) < p.idx(probe).n_column as i32
            && ((b.p_new.n_eq as i32) < p.idx(probe).n_key_col as i32 || p.idx(probe).idx_type != SQLITE_IDXTYPE_PRIMARYKEY as u8)
        {
            where_loop_add_btree_index(p, w, b, i_src, probe, n_in_mul + n_in);
        }
        b.p_new.n_out = saved_n_out;
        term_opt = where_scan_next(p, w, &mut scan);
    }
    b.p_new.prereq = saved_prereq;
    b.p_new.n_eq = saved_n_eq;
    b.p_new.n_btm = saved_n_btm;
    b.p_new.n_top = saved_n_top;
    b.p_new.n_skip = saved_n_skip;
    b.p_new.ws_flags = saved_ws_flags;
    b.p_new.n_out = saved_n_out;
    b.p_new.a_l_term.truncate(saved_n_l_term);

    // Consider using a skip-scan if there are no WHERE clause constraints available for the
    // left-most terms of the index, and if the average number of repeats in the left-most
    // terms is at least 18.
    //
    // The magic number 18 is selected on the basis that scanning 17 rows is almost always
    // quicker than an index seek (even though if the index contains fewer than 2^17 rows we
    // assume otherwise in other parts of the code). But this makes it difficult to compare
    // the cost of a skip-scan against a full table scan....
    if saved_n_eq == saved_n_skip
        && (saved_n_eq as i32 + 1) < p.idx(probe).n_key_col as i32
        && saved_n_eq as usize == b.p_new.n_l_term()
        && !p.idx(probe).no_skip_scan
        && p.idx(probe).has_stat1
        && p.db.optimization_enabled(SQLITE_SkipScan)
        && p.idx(probe).ai_row_log_est[saved_n_eq as usize + 1] >= 42 // TUNING: Minimum for skip-scan
        && !src.from_exists
    {
        b.p_new.n_eq += 1;
        b.p_new.n_skip += 1;
        b.p_new.a_l_term.push(None);
        b.p_new.ws_flags |= WHERE_SKIPSCAN as u32;
        let a = &p.idx(probe).ai_row_log_est;
        let mut n_iter = a[saved_n_eq as usize] - a[saved_n_eq as usize + 1];
        b.p_new.n_out -= n_iter;
        // TUNING: Because uncertainties in the estimates for skip-scan queries, add a 1.375
        // fudge factor to make skip-scan slightly less likely.
        n_iter += 5;
        where_loop_add_btree_index(p, w, b, i_src, probe, n_iter + n_in_mul);
        b.p_new.n_out = saved_n_out;
        b.p_new.n_eq = saved_n_eq;
        b.p_new.n_skip = saved_n_skip;
        b.p_new.ws_flags = saved_ws_flags;
        b.p_new.a_l_term.truncate(saved_n_l_term);
    }
    rc
}

/// `indexMightHelpWithOrderBy(pBuilder, pIndex, iCursor)`: return True if it is possible that
/// pIndex might be useful in implementing the ORDER BY clause in pBuilder.
///
/// Return False if pBuilder does not contain an ORDER BY clause or if there is no way for
/// pIndex to be useful in implementing that ORDER BY clause.
fn index_might_help_with_order_by(p: &mut Parse, w: &WhereInfo, index: IndexId, i_cursor: i32) -> bool {
    if p.idx(index).b_unordered {
        return false;
    }
    let ob = w.p_order_by;
    if ob.is_null() {
        return false;
    }
    for ii in 0..p.l(ob).a.len() {
        let e = sqlite3_expr_skip_collate_and_likely(p, p.l(ob).a[ii].p_expr);
        if e.is_null() {
            continue;
        }
        if (p.e(e).op == TK_COLUMN || p.e(e).op == TK_AGG_COLUMN) && p.e(e).i_table == i_cursor {
            if p.e(e).i_column < 0 {
                return true;
            }
            for jj in 0..p.idx(index).n_key_col as usize {
                if p.e(e).i_column == p.idx(index).ai_column[jj] {
                    return true;
                }
            }
        } else {
            let a_col_expr = p.idx(index).a_col_expr;
            if a_col_expr.nn() {
                for jj in 0..p.idx(index).n_key_col as usize {
                    if p.idx(index).ai_column[jj] as i32 != XN_EXPR {
                        continue;
                    }
                    if sqlite3_expr_compare_skip(p, e, p.l(a_col_expr).a[jj].p_expr, i_cursor) == 0 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// `whereUsablePartialIndex(iTab, jointype, pWC, pWhere)`: check to see if a partial index
/// with pPartIndexWhere can be used in the current query. Return true if it can be and false
/// if not.
fn where_usable_partial_index(
    p: &mut Parse, w: &WhereInfo, i_tab: i32, jointype: u8, wc: WcId, mut p_where: ExprId, bound: Option<(usize, &Value)>,
) -> bool {
    if jointype as i32 & JT_LTORJ != 0 {
        return false;
    }
    while p.e(p_where).op == TK_AND {
        let l = p.e(p_where).p_left;
        if !where_usable_partial_index(p, w, i_tab, jointype, wc, l, bound) {
            return false;
        }
        p_where = p.e(p_where).p_right;
    }
    for i in 0..w.a_wc[wc].a.len() {
        let (e, wt_flags) = {
            let t = &w.a_wc[wc].a[i];
            (t.p_expr, t.wt_flags)
        };
        let implies = |p: &mut Parse, tab| {
            sqlite3_expr_implies_expr(p, e, p_where, tab)
                || bound.is_some_and(|binding| sqlite3_partial_bound_eq(p, e, p_where, tab, binding))
        };
        if (!p.e(e).has(EP_OuterON) || p.e(e).w == i_tab)
            && ((jointype as i32 & JT_OUTER) == 0 || p.e(e).has(EP_OuterON))
            && implies(p, i_tab)
            && !implies(p, -1)
            && (wt_flags & TERM_VNULL) == 0
        {
            return true;
        }
    }
    false
}

/// `exprIsCoveredByIndex(pExpr, pIdx, iTabCur)`: pIdx is an index containing expressions.
/// Check it see if pExpr is the same as any expression in the index.
fn expr_is_covered_by_index(p: &Parse, e: ExprId, idx: IndexId, i_tab_cur: i32) -> bool {
    let ix = p.idx(idx);
    for i in 0..ix.n_column as usize {
        if ix.ai_column[i] as i32 == XN_EXPR && sqlite3_expr_compare(p, false, e, p.l(ix.a_col_expr).a[i].p_expr, i_tab_cur) == 0 {
            return true;
        }
    }
    false
}

/// `whereIsCoveringIndexWalkCallback(pWalk, pExpr)`: information passed in is pWalk->u.pCovIdxCk
/// (`p_idx`, `i2` = iTabCur, `b1` = bExpr, `b2` = bUnidx). Call it pCk.
///
/// If the Expr node references the table with cursor pCk->iTabCur, then make sure that
/// column is covered by the index pCk->pIdx. We know that all columns less than 63 (really
/// BMS-1) are covered, so we don't need to check them. But we do need to check any column at
/// 63 or greater.
///
/// If the index does not cover the column, then set pWalk->eCode to non-zero and return
/// WRC_Abort to stop the search.
///
/// If this node does not disprove that the index can be a covering index, then just return
/// WRC_Continue, to continue the search.
///
/// If pCk->pIdx contains indexed expressions and one of those expressions matches pExpr,
/// then prune the search.
fn where_is_covering_index_walk_callback(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    let idx = w.p_idx;
    if p.e(e).op == TK_COLUMN || p.e(e).op == TK_AGG_COLUMN {
        if p.e(e).i_table != w.i2 {
            return WRC_Continue;
        }
        let ix = p.idx(idx);
        for i in 0..ix.n_column as usize {
            if ix.ai_column[i] == p.e(e).i_column {
                return WRC_Continue;
            }
        }
        w.b2 = true;
        return WRC_Abort;
    } else if p.idx(idx).b_has_expr && expr_is_covered_by_index(p, e, idx, w.i2) {
        w.b1 = true;
        return WRC_Prune;
    }
    WRC_Continue
}

/// `whereIsCoveringIndex(pWInfo, pIdx, iTabCur)`: pIdx is an index that covers all of the
/// low-number columns used by pWInfo->pSelect (columns from 0 through 62) or an index on
/// expressions. pIdx is known to be a covering index for all columns up through 62, or the
/// index has expressions. This routine checks if the index covers any columns of table
/// iTabCur numbered 63 and greater, and whether every use of the table goes through an
/// indexed expression.
///
/// Return 0 if pIdx is not a covering index, WHERE_IDX_ONLY if pIdx is a covering index,
/// WHERE_EXPRIDX if pIdx might be a covering index but uses expressions (the final
/// determination is made in sqlite3WhereEnd()).
fn where_is_covering_index(p: &mut Parse, w: &WhereInfo, idx: IndexId, i_tab_cur: i32) -> u32 {
    if w.p_select.is_null() {
        // We don't have access to the full query, so we cannot check to see if pIdx is
        // covering. Assume it is not.
        return 0;
    }
    if !p.idx(idx).b_has_expr {
        let ix = p.idx(idx);
        let mut i = 0;
        while i < ix.n_column as usize {
            if ix.ai_column[i] as i32 >= BMS - 1 {
                break;
            }
            i += 1;
        }
        if i >= ix.n_column as usize {
            // pIdx does not index any columns greater than 62, but we know from colMask that
            // columns greater than 62 are used, so this is not a covering index
            return 0;
        }
    }
    let mut wk = Walker {
        x_expr_callback: Some(where_is_covering_index_walk_callback),
        x_select_callback: Some(sqlite3_select_walk_noop),
        p_idx: idx,
        i2: i_tab_cur,
        b1: false,
        b2: false,
        ..Walker::default()
    };
    sqlite3_walk_select(p, &mut wk, w.p_select);
    if wk.b2 {
        0
    } else if wk.b1 {
        WHERE_EXPRIDX as u32
    } else {
        WHERE_IDX_ONLY as u32
    }
}

/// `wherePartIdxExpr(pParse, pIdx, pPart, pMask, iIdxCur, pItem)`: this function is called
/// for a partial index - one with a WHERE clause - in two scenarios. In both cases, it
/// determines whether or not the WHERE clause on the index implies that a column of the
/// table may be safely replaced by a constant expression. For example, in the following
/// SELECT:
///
///   CREATE INDEX i1 ON t1(b, c) WHERE a=<expr>;
///   SELECT a, b, c FROM t1 WHERE a=<expr> AND b=?;
///
/// The "a" in the select-list may be replaced by <expr>, iff:
///
///    (a) <expr> is a constant expression, and
///    (b) The (a=<expr>) comparison uses the BINARY collation sequence, and
///    (c) Column "a" has an affinity other than NONE or BLOB.
///
/// If argument pItem is NULL, then pMask must not be NULL. In this case this function is
/// being called as part of determining whether or not pIdx is a covering index. This
/// function clears any bits in (*pMask) corresponding to columns that may be replaced by
/// constants in the WHERE clause of pIdx.
///
/// Otherwise, if pItem is not NULL, then this function is being called from within
/// sqlite3WhereBegin(), when the planner has already decided to use the index. In this case
/// an IndexedExpr object is added to the Parse.pIdxPartExpr list for each column that can
/// be replaced by a constant.
fn where_part_idx_expr(
    p: &mut Parse,
    idx: IndexId,
    mut part: ExprId,
    p_mask: Option<&mut Bitmask>,
    i_idx_cur: i32,
    item: Option<(SrcListId, usize)>,
) {
    let mut mask_ref = p_mask;
    if p.e(part).op == TK_AND {
        let r = p.e(part).p_right;
        where_part_idx_expr(p, idx, r, mask_ref.as_deref_mut(), i_idx_cur, item);
        part = p.e(part).p_left;
    }
    if p.e(part).op == TK_EQ || p.e(part).op == TK_IS {
        let (left, right) = (p.e(part).p_left, p.e(part).p_right);
        if p.e(left).op != TK_COLUMN {
            return;
        }
        if sqlite3_expr_is_constant_no_parse(p, right) == 0 {
            return;
        }
        let coll = sqlite3_expr_compare_coll_seq(p, part);
        if !sqlite3_is_binary(coll.as_deref()) {
            return;
        }
        let i_column = p.e(left).i_column as i32;
        if i_column < 0 {
            return;
        }
        let aff = p.tab(p.idx(idx).p_table).a_col[i_column as usize].affinity;
        if aff >= SQLITE_AFF_TEXT {
            if let Some((sl, i)) = item {
                let (jointype, i_cursor) = {
                    let it = &p.src(sl).a[i];
                    (it.jointype as i32, it.i_cursor)
                };
                let b_null_row = (jointype & (JT_LEFT | JT_LTORJ)) != 0;
                let dup = sqlite3_expr_dup(p, right, 0);
                p.p_idx_part_expr.push(IndexedExpr {
                    p_expr: dup,
                    i_data_cur: i_cursor,
                    i_idx_cur,
                    i_idx_col: i_column,
                    b_maybe_null_row: b_null_row,
                    aff,
                });
            } else if i_column < BMS - 1
                && let Some(m) = mask_ref
            {
                *m &= !(1u64 << i_column);
            }
        }
    }
}

/// `whereLoopAddBtree(pBuilder, mPrereq)`: add all WhereLoop objects for a single table of
/// the join where the table is identified by pBuilder->pNew->iTab. That table is guaranteed
/// to be a b-tree table, not a virtual table.
///
/// The costs (WhereLoop.rRun) of the b-tree loops added by this function are calculated as
/// follows:
///
/// For a full scan, assuming the table (or index) contains nRow rows:
///
///     cost = nRow * 3.0                    // full-table scan
///     cost = nRow * K                      // scan of covering index
///     cost = nRow * (K+3.0)                // scan of non-covering index
///
/// where K is a value between 1.1 and 3.0 set based on the relative estimated average size
/// of the index and table records.
///
/// For an index scan, where nVisit is the number of index rows visited by the scan, and
/// nSeek is the number of seek operations required on the index b-tree:
///
///     cost = nSeek * (log(nRow) + K * nVisit)          // covering index
///     cost = nSeek * (log(nRow) + (K+3.0) * nVisit)    // non-covering index
///
/// Normally, nSeek is 1. nSeek values greater than 1 come about if the WHERE clause includes
/// "x IN (....)" terms used in place of "x=?". Or when implicit "x IN (SELECT x FROM tbl)"
/// terms are added by the skip-scan logic.
///
/// The estimated values (nRow, nVisit, nSeek) often contain a large amount of uncertainty.
/// For this reason, scoring is designed to pick plans that "do the least harm" if the
/// estimates turn out to be wrong. For example, a full-table scan plan is assigned a cost
/// slightly higher than similar index-based plans in order to prevent the query planner from
/// evaluating very large tables using very slow CPUs.
fn where_loop_add_btree(p: &mut Parse, w: &mut WhereInfo, b: &mut WhereLoopBuilder, m_prereq: Bitmask) -> i32 {
    let mut rc = SQLITE_OK;
    let mut i_sort_idx = 1; // Index number
    let tab_list = w.p_tab_list;
    let i_src = b.p_new.i_tab as usize;
    let src = p.src(tab_list).a[i_src].clone();
    let tab = src.p_s_tab;
    let wc = b.p_wc;

    let probes: Vec<IndexId> = if src.is_indexed_by {
        // An INDEXED BY clause specifies a particular index to use
        vec![src.p_ib_index]
    } else if !p.tab(tab).has_rowid() {
        p.tab(tab).p_index.clone()
    } else {
        // There is no INDEXED BY clause. Create a fake Index object in local variable sPk to
        // represent the rowid primary key index. Make this fake index the first in a chain of
        // Index objects with all of the real indices to follow
        let s_pk = Index {
            n_key_col: 1,
            n_column: 1,
            ai_column: vec![-1],
            ai_row_log_est: vec![p.tab(tab).n_row_log_est, 0],
            on_error: OE_Replace as u8,
            p_table: tab,
            sz_idx_row: 3, // TUNING: Interior rows of IPK table are very small
            idx_type: SQLITE_IDXTYPE_IPK as u8,
            az_coll: vec!["BINARY".to_owned()],
            a_coll_static: vec![true],
            a_sort_order: vec![0],
            ..Index::default()
        };
        let pk = p.new_idx(s_pk);
        let mut v = vec![pk];
        if !src.not_indexed {
            // The real indices of the table are only considered if the NOT INDEXED qualifier
            // is omitted from the FROM clause
            v.extend(p.tab(tab).p_index.iter().copied());
        }
        v
    };
    let mut r_size = p.tab(tab).n_row_log_est;

    // Automatic indexes
    if b.p_or_set.is_none() // Not part of an OR optimization
        && (w.wctrl_flags as i32 & (WHERE_RIGHT_JOIN | WHERE_OR_SUBCLAUSE)) == 0
        && (p.db.flags & SQLITE_AutoIndex) != 0
        && !src.is_indexed_by // Has no INDEXED BY clause
        && !src.not_indexed // Has no NOT INDEXED clause
        && !src.is_correlated // Not a correlated subquery
        && !src.is_recursive // Not a recursive common table expression.
        && (src.jointype as i32 & JT_RIGHT) == 0
    // Not the right tab of a RIGHT JOIN
    {
        // Generate auto-index WhereLoops
        let r_log_size = est_log(r_size); // Logarithm of the number of rows in the table
        let n_term = w.a_wc[wc].a.len();
        let mut k = 0;
        while rc == SQLITE_OK && k < n_term {
            let term = w.a_wc[wc].a[k].clone();
            if term.prereq_right & b.p_new.mask_self != 0 {
                k += 1;
                continue;
            }
            if term_can_drive_index(p, &term, tab_list, i_src, 0) {
                b.p_new.n_eq = 1;
                b.p_new.n_skip = 0;
                b.p_new.p_index = IndexId::NULL;
                b.p_new.a_l_term.clear();
                b.p_new.a_l_term.push(Some(TermRef::new(wc, k)));
                // TUNING: One-time cost for computing the automatic index is estimated to be
                // X*N*log2(N) where N is the number of rows in the table. In other words, the
                // setup cost is proportional to the cost of sorting the table ... The cost
                // of an automatic index on an ephemeral table (a materialized view or
                // subquery) is much less, as there is no table to scan.
                b.p_new.r_setup = r_log_size + r_size;
                if !p.tab(tab).is_view() && (p.tab(tab).tab_flags & TF_Ephemeral) == 0 {
                    b.p_new.r_setup += 28;
                } else {
                    // Greatly reduced setup cost for auto indexes on ephemeral
                    // materializations of views
                    b.p_new.r_setup -= 25;
                }
                if b.p_new.r_setup < 0 {
                    b.p_new.r_setup = 0;
                }
                // TUNING: Each index lookup yields 20 rows in the table. This is a guess.
                // 4.3.2015: Changed to 43.
                b.p_new.n_out = 43;
                b.p_new.r_run = sqlite3_log_est_add(r_log_size, b.p_new.n_out);
                b.p_new.ws_flags = WHERE_AUTO_INDEX as u32;
                b.p_new.prereq = m_prereq | term.prereq_right;
                rc = where_loop_insert(p, w, b);
            }
            k += 1;
        }
    }

    // Loop over all indices. If there was an INDEXED BY clause, then only consider index
    // pProbe.
    for &probe in &probes {
        if rc != SQLITE_OK {
            break;
        }
        let this_sort_idx = i_sort_idx;
        i_sort_idx += 1;
        let part = p.idx(probe).p_part_idx_where;
        if part.nn() {
            let bound = if p.is_toplevel() && p.tab(tab).i_db == 0 {
                p.db.host.partial_index_binding(p.idx(probe).tnum)
            } else {
                None
            };
            if !where_usable_partial_index(p, w, src.i_cursor, src.jointype, wc, part, bound) {
                continue; // Partial index inappropriate for this query
            }
        }
        if p.idx(probe).b_no_query {
            continue;
        }
        r_size = p.idx(probe).ai_row_log_est[0];
        b.p_new.n_eq = 0;
        b.p_new.n_btm = 0;
        b.p_new.n_top = 0;
        b.p_new.n_distinct_col = 0;
        b.p_new.n_skip = 0;
        b.p_new.a_l_term.clear();
        b.p_new.i_sort_idx = 0;
        b.p_new.r_setup = 0;
        b.p_new.prereq = m_prereq;
        b.p_new.n_out = r_size;
        b.p_new.p_index = probe;
        b.p_new.p_order_by = ExprListId::NULL;
        let b_help = index_might_help_with_order_by(p, w, probe, src.i_cursor);

        if p.idx(probe).idx_type == SQLITE_IDXTYPE_IPK as u8 {
            // Integer primary key index
            b.p_new.ws_flags = WHERE_IPK as u32;

            // Full table scan
            b.p_new.i_sort_idx = if b_help { this_sort_idx as u8 } else { 0 };
            // TUNING: Cost of full table scan is 3.0*N. The 3.0 factor is an extra cost
            // designed to discourage the use of full table scans, since index lookups have
            // better worst-case performance if our stat guesses are wrong. Reduce the 3.0
            // penalty slightly (1/3) to help counter the fact that we do not yet know how
            // many rows will be filtered by the WHERE clause.
            b.p_new.r_run = r_size + 16;
            {
                let mut l = std::mem::take(&mut b.p_new);
                where_loop_output_adjust(p, w, wc, &mut l, r_size);
                b.p_new = l;
            }
            if src.is_subquery {
                if src.via_coroutine {
                    b.p_new.ws_flags |= WHERE_COROUTINE as u32;
                }
                // Do not set btree.pOrderBy for a recursive CTE. In this case the ORDER BY
                // clause does not affect the order in which the subquery returns its rows.
                let sub = src.p_subq.as_ref().expect("subquery").p_select;
                if (p.s(sub).sel_flags & SF_Recursive) == 0 {
                    b.p_new.p_order_by = p.s(sub).p_order_by;
                }
            } else if src.from_exists {
                b.p_new.n_out = 0;
            }
            rc = where_loop_insert(p, w, b);
            b.p_new.n_out = r_size;
            if rc != 0 {
                break;
            }
        } else {
            let mut m: Bitmask;
            if p.idx(probe).is_covering {
                m = 0;
                b.p_new.ws_flags = (WHERE_IDX_ONLY | WHERE_INDEXED) as u32;
            } else {
                m = src.col_used & p.idx(probe).col_not_idxed;
                if part.nn() {
                    where_part_idx_expr(p, probe, part, Some(&mut m), 0, None);
                }
                b.p_new.ws_flags = WHERE_INDEXED as u32;
                if m == (1u64 << 63) || (p.idx(probe).b_has_expr && !p.idx(probe).b_has_v_col && m != 0) {
                    let is_cov = where_is_covering_index(p, w, probe, src.i_cursor);
                    if is_cov != 0 {
                        m = 0;
                        b.p_new.ws_flags |= is_cov;
                    }
                } else if m == 0 && (p.tab(tab).has_rowid() || w.p_select.nn()) {
                    b.p_new.ws_flags = (WHERE_IDX_ONLY | WHERE_INDEXED) as u32;
                }
            }

            // Full scan via index
            if b_help
                || !p.tab(tab).has_rowid()
                || part.nn()
                || src.is_indexed_by
                || (m == 0
                    && !p.idx(probe).b_unordered
                    && p.idx(probe).sz_idx_row < p.tab(tab).sz_tab_row
                    && (w.wctrl_flags as i32 & WHERE_ONEPASS_DESIRED) == 0
                    && p.db.optimization_enabled(SQLITE_CoverIdxScan))
            {
                b.p_new.i_sort_idx = if b_help { this_sort_idx as u8 } else { 0 };

                // The cost of visiting the index rows is N*K, where K is between 1.1 and 3.0,
                // depending on the relative sizes of the index and table rows.
                b.p_new.r_run = r_size + 1 + ((15 * p.idx(probe).sz_idx_row as i32) / p.tab(tab).sz_tab_row as i32) as LogEst;
                if m != 0 {
                    // If this is a non-covering index scan, add in the cost of doing table
                    // lookups. The cost will be 3x the number of lookups. Take into account
                    // WHERE clause terms that can be satisfied using just the index, and that
                    // do not require a table lookup.
                    let mut n_lookup: LogEst = r_size + 16; // Base cost: N*3
                    let i_cur = src.i_cursor;
                    for ii in 0..w.a_wc[0].a.len() {
                        let (te, truth_prob, e_operator) = {
                            let t = &w.a_wc[0].a[ii];
                            (t.p_expr, t.truth_prob, t.e_operator)
                        };
                        if !sqlite3_expr_covered_by_index(p, te, i_cur, probe) {
                            break;
                        }
                        // pTerm can be evaluated using just the index. So reduce the expected
                        // number of table lookups accordingly
                        if truth_prob <= 0 {
                            n_lookup += truth_prob;
                        } else {
                            n_lookup -= 1;
                            if e_operator & (WO_EQ | WO_IS) != 0 {
                                n_lookup -= 19;
                            }
                        }
                    }
                    b.p_new.r_run = sqlite3_log_est_add(b.p_new.r_run, n_lookup);
                }
                {
                    let mut l = std::mem::take(&mut b.p_new);
                    where_loop_output_adjust(p, w, wc, &mut l, r_size);
                    b.p_new = l;
                }
                if (src.jointype as i32 & JT_RIGHT) != 0 && p.idx(probe).a_col_expr.nn() {
                    // Do not do an SCAN of a index-on-expression in a RIGHT JOIN because the
                    // code generator for index-on-expression does not handle that case
                } else {
                    if src.from_exists {
                        b.p_new.n_out = 0;
                    }
                    rc = where_loop_insert(p, w, b);
                }
                b.p_new.n_out = r_size;
                if rc != 0 {
                    break;
                }
            }
        }

        b.bld_flags1 = 0;
        rc = where_loop_add_btree_index(p, w, b, i_src, probe, 0);
        // (SQLITE_BLDF1_INDEXED: pTab->tabFlags |= TF_MaybeReanalyze — only read by
        // "PRAGMA optimize")
        if src.is_indexed_by {
            break;
        }
    }
    rc
}

/// `whereLoopAddOr(pBuilder, mPrereq, mUnusable)`: add WhereLoop entries to handle OR terms.
/// This works for either btree or virtual tables.
fn where_loop_add_or(p: &mut Parse, w: &mut WhereInfo, b: &mut WhereLoopBuilder, m_prereq: Bitmask, m_unusable: Bitmask) -> i32 {
    let mut rc = SQLITE_OK;
    let wc = b.p_wc;
    let n_term = w.a_wc[wc].a.len();
    let (jointype, i_cur, item_tab) = {
        let it = &p.src(w.p_tab_list).a[b.p_new.i_tab as usize];
        (it.jointype as i32, it.i_cursor, it.p_s_tab)
    };

    // The multi-index OR optimization does not work for RIGHT and FULL JOIN
    if jointype & JT_RIGHT != 0 {
        return SQLITE_OK;
    }

    let mut k = 0;
    while k < n_term && rc == SQLITE_OK {
        let (e_operator, or_info) = {
            let t = &w.a_wc[wc].a[k];
            (t.e_operator, t.p_or_info)
        };
        if (e_operator & WO_OR) != 0
            && let Some(oi) = or_info
            && (oi.indexable & b.p_new.mask_self) != 0
        {
            let or_wc = oi.wc;
            let n_or = w.a_wc[or_wc].a.len();
            let mut once = true;
            let mut s_sum = WhereOrSet::default();
            // sSubBuild = *pBuilder; sSubBuild.pOrSet = &sCur;
            let mut sub = WhereLoopBuilder {
                p_wc: b.p_wc,
                p_new: std::mem::take(&mut b.p_new),
                p_or_set: Some(WhereOrSet::default()),
                bld_flags1: b.bld_flags1,
                bld_flags2: b.bld_flags2,
                i_plan_limit: b.i_plan_limit,
            };
            for j in 0..n_or {
                let (o_op, o_cur, and_info) = {
                    let t = &w.a_wc[or_wc].a[j];
                    (t.e_operator, t.left_cursor, t.p_and_info)
                };
                let mut temp_wc: Option<WcId> = None;
                if (o_op & WO_AND) != 0 {
                    sub.p_wc = and_info.expect("pAndInfo");
                } else if o_cur == i_cur {
                    // tempWC.pWInfo = pWC->pWInfo; tempWC.pOuter = pWC; tempWC.op = TK_AND;
                    // tempWC.nTerm = 1; tempWC.nBase = 1; tempWC.a = pOrTerm;
                    let t = w.a_wc[or_wc].a[j].clone();
                    w.a_wc.push(WhereClause { p_outer: Some(wc), op: TK_AND, has_or: false, n_base: 1, a: vec![t] });
                    let id = w.a_wc.len() - 1;
                    temp_wc = Some(id);
                    sub.p_wc = id;
                } else {
                    continue;
                }
                sub.p_or_set = Some(WhereOrSet::default()); // sCur.n = 0
                if p.tab(item_tab).is_virtual() {
                    rc = where_loop_add_virtual(p, w, &mut sub, m_prereq, m_unusable);
                } else {
                    rc = where_loop_add_btree(p, w, &mut sub, m_prereq);
                }
                if rc == SQLITE_OK {
                    rc = where_loop_add_or(p, w, &mut sub, m_prereq, m_unusable);
                }
                if let Some(id) = temp_wc {
                    // the temporary clause aliased pOrTerm: carry its flags back
                    let f = w.a_wc[id].a[0].wt_flags;
                    w.a_wc[or_wc].a[j].wt_flags = f;
                }
                let s_cur = sub.p_or_set.expect("pOrSet");
                if s_cur.n == 0 {
                    s_sum.n = 0;
                    break;
                } else if once {
                    s_sum = s_cur; // whereOrMove(&sSum, &sCur)
                    once = false;
                } else {
                    let s_prev = s_sum;
                    s_sum.n = 0;
                    for i in 0..s_prev.n as usize {
                        for jj in 0..s_cur.n as usize {
                            where_or_insert(
                                &mut s_sum,
                                s_prev.a[i].prereq | s_cur.a[jj].prereq,
                                sqlite3_log_est_add(s_prev.a[i].r_run, s_cur.a[jj].r_run),
                                sqlite3_log_est_add(s_prev.a[i].n_out, s_cur.a[jj].n_out),
                            );
                        }
                    }
                }
            }
            b.p_new = std::mem::take(&mut sub.p_new);
            b.p_new.a_l_term.clear();
            b.p_new.a_l_term.push(Some(TermRef::new(wc, k)));
            b.p_new.ws_flags = WHERE_MULTI_OR as u32;
            b.p_new.r_setup = 0;
            b.p_new.i_sort_idx = 0;
            // memset(&pNew->u, 0, sizeof(pNew->u));
            b.p_new.n_eq = 0;
            b.p_new.n_btm = 0;
            b.p_new.n_top = 0;
            b.p_new.n_distinct_col = 0;
            b.p_new.p_index = IndexId::NULL;
            b.p_new.p_order_by = ExprListId::NULL;
            let mut i = 0;
            while rc == SQLITE_OK && i < s_sum.n as usize {
                // TUNING: Currently sSum.a[i].rRun is set to the sum of the costs of all
                // sub-scans required by the OR-scan. However, due to rounding errors, it may
                // be that the cost of the OR-scan is equal to its most expensive sub-scan.
                // Add the smallest possible penalty (equivalent to multiplying the cost by
                // 1.07) to ensure that this does not happen. Otherwise, for WHERE clauses
                // such as the following where there is an index on "y":
                //
                //     WHERE likelihood(x=?, 0.99) OR y=?
                //
                // the planner may elect to "OR" together a full-table scan and an index
                // lookup using "y". Not only is this a waste of time, it would not be
                // possible to code (the code generator does not handle a full scan in an OR).
                b.p_new.r_run = s_sum.a[i].r_run + 1;
                b.p_new.n_out = s_sum.a[i].n_out;
                b.p_new.prereq = s_sum.a[i].prereq;
                rc = where_loop_insert(p, w, b);
                i += 1;
            }
        }
        k += 1;
    }
    rc
}

// ---------------------------------------------------------------------------------------------
// Virtual tables (where.c: allocateIndexInfo ... whereLoopAddVirtual)
// ---------------------------------------------------------------------------------------------

/// `ALLBITS`
const ALLBITS: Bitmask = !0;

/// `SMASKBIT32(n)`
#[inline]
fn smaskbit32(n: i32) -> u32 {
    if (0..=31).contains(&n) { 1u32 << n } else { 0 }
}

/// `struct HiddenIndexInfo`: extra information appended to the end of sqlite3_index_info but
/// not visible to the xBestIndex function, at least not directly. The sqlite3_vtab_collation()
/// interface knows how to reach it, however.
#[derive(Debug, Clone, Default)]
pub struct HiddenIndexInfo {
    /// `pWC`: the Where clause being analyzed
    pub wc: WcId,
    /// `eDistinct`: Value to return from sqlite3_vtab_distinct()
    pub e_distinct: i32,
    /// `mIn`: Mask of terms that are <col> IN (...)
    pub m_in: u32,
    /// `mHandleIn`: Terms that vtab will handle as <col> IN (...)
    pub m_handle_in: u32,
    /// `aRhs[]`: RHS values of the constraints, computed on the first request (outer None:
    /// not computed yet)
    pub a_rhs: Vec<Option<Option<Value>>>,
}

/// `termFromWhereClause(pWC, iTerm)`: return term iTerm of the WhereClause passed as the first
/// argument. Terms are numbered from 0 upwards, starting with the terms in pWC->a[], then
/// those in pWC->pOuter->a[] (if any), and so on.
fn term_from_where_clause(w: &WhereInfo, wc: WcId, mut i_term: i32) -> Option<TermRef> {
    let mut c = Some(wc);
    while let Some(x) = c {
        let n = w.a_wc[x].a.len() as i32;
        if i_term < n {
            return Some(TermRef::new(x, i_term as usize));
        }
        i_term -= n;
        c = w.a_wc[x].p_outer;
    }
    None
}

/// The calls of the module into `HiddenIndexInfo` during its xBestIndex (where.c:
/// `sqlite3_vtab_in()`, `sqlite3_vtab_rhs_value()`, `sqlite3_vtab_distinct()`,
/// `sqlite3_vtab_collation()`).
struct HiddenCalls<'x, 'a> {
    p: &'x mut Parse<'a>,
    w: &'x WhereInfo,
    h: &'x mut HiddenIndexInfo,
    /// `aConstraint[].iTermOffset` of the constraints passed to the module
    term_offsets: Vec<i32>,
}

impl IndexInfoHidden for HiddenCalls<'_, '_> {
    fn vtab_in(&mut self, i_cons: i32, b_handle: i32) -> bool {
        let m = smaskbit32(i_cons);
        if m & self.h.m_in != 0 {
            if b_handle == 0 {
                self.h.m_handle_in &= !m;
            } else if b_handle > 0 {
                self.h.m_handle_in |= m;
            }
            return true;
        }
        false
    }

    fn rhs_value(&mut self, i_cons: i32) -> (i32, Option<Value>) {
        if i_cons < 0 || i_cons as usize >= self.term_offsets.len() {
            return (crate::vtab::SQLITE_MISUSE, None); // EV: R-30545-25046
        }
        let ic = i_cons as usize;
        if self.h.a_rhs[ic].is_none() {
            let t = term_from_where_clause(self.w, self.h.wc, self.term_offsets[ic]).expect("term");
            let right = self.p.e(self.w.term(t).p_expr).p_right;
            let v = crate::build_tab::sqlite3_value_from_expr(self.p, right, SQLITE_AFF_BLOB);
            self.h.a_rhs[ic] = Some(v);
        }
        let v = self.h.a_rhs[ic].clone().flatten();
        if v.is_none() {
            // IMP: R-19933-32160
            return (crate::vtab::SQLITE_NOTFOUND, None); // IMP: R-36424-56542
        }
        (SQLITE_OK, v)
    }

    fn distinct(&self) -> i32 {
        self.h.e_distinct
    }

    fn collation(&mut self, i_cons: i32) -> Option<String> {
        if i_cons < 0 || i_cons as usize >= self.term_offsets.len() {
            return None;
        }
        let t = term_from_where_clause(self.w, self.h.wc, self.term_offsets[i_cons as usize]).expect("term");
        let x = self.w.term(t).p_expr;
        let mut c = None;
        if self.p.e(x).p_left.nn() {
            c = crate::expr::sqlite3_expr_compare_coll_seq(self.p, x);
        }
        Some(c.map_or_else(|| "BINARY".to_owned(), |c| c.name.clone()))
    }
}

/// `allocateIndexInfo(pWInfo, pWC, mUnusable, pSrc, pmNoOmit)`: allocate and populate an
/// sqlite3_index_info structure. Returns it with its HiddenIndexInfo and the mask of terms
/// not to omit.
fn allocate_index_info(p: &mut Parse, w: &mut WhereInfo, wc: WcId, m_unusable: Bitmask, i_src: usize) -> (IndexInfo, HiddenIndexInfo, u16) {
    let tab_list = w.p_tab_list;
    let src_item = p.src(tab_list).a[i_src].clone();
    let tab = src_item.p_s_tab;
    let mut m_no_omit: u16 = 0;
    let mut e_distinct = 0;
    let order_by = w.p_order_by;

    // Find all WHERE clause constraints referring to this virtual table. Mark each term with
    // the TERM_OK flag. Set nTerm to the number of terms found.
    let mut n_term = 0;
    let mut c = Some(wc);
    while let Some(x) = c {
        for i in 0..w.a_wc[x].a.len() {
            w.a_wc[x].a[i].wt_flags &= !TERM_OK;
            let t = &w.a_wc[x].a[i];
            if t.left_cursor != src_item.i_cursor {
                continue;
            }
            if t.prereq_right & m_unusable != 0 {
                continue;
            }
            if (t.e_operator & !WO_EQUIV) == 0 {
                continue;
            }
            if t.wt_flags & TERM_VNULL != 0 {
                continue;
            }
            if (src_item.jointype as i32 & (JT_LEFT | JT_LTORJ | JT_RIGHT)) != 0
                && !constraint_compatible_with_outer_join(p, t, &src_item)
            {
                continue;
            }
            n_term += 1;
            w.a_wc[x].a[i].wt_flags |= TERM_OK;
        }
        c = w.a_wc[x].p_outer;
    }

    // If the ORDER BY clause contains only columns in the current virtual table then allocate
    // space for the aOrderBy part of the sqlite3_index_info structure.
    let mut n_order_by = 0;
    if order_by.nn() {
        let n = p.l(order_by).a.len();
        let mut i = 0;
        while i < n {
            let e = p.l(order_by).a[i].p_expr;

            // Skip over constant terms in the ORDER BY clause
            if crate::expr::sqlite3_expr_is_constant_no_parse(p, e) != 0 {
                i += 1;
                continue;
            }

            // Virtual tables are unable to deal with NULLS FIRST
            if p.l(order_by).a[i].sort_flags & KEYINFO_ORDER_BIGNULL != 0 {
                break;
            }

            // First case - a direct column references without a COLLATE operator
            if p.e(e).op == TK_COLUMN && p.e(e).i_table == src_item.i_cursor {
                i += 1;
                continue;
            }

            // 2nd case - a column reference with a COLLATE operator. Only match of the
            // COLLATE operator matches the collation of the column.
            if p.e(e).op == TK_COLLATE {
                let e2 = p.e(e).p_left;
                if p.e(e2).op == TK_COLUMN && p.e(e2).i_table == src_item.i_cursor {
                    let c2 = p.e(e2).i_column;
                    p.em(e).i_column = c2;
                    if c2 < 0 {
                        i += 1;
                        continue; // Collseq does not matter for rowid
                    }
                    let z_coll = crate::schema::sqlite3_column_coll(&p.tab(tab).a_col[c2 as usize]).unwrap_or("BINARY").to_owned();
                    if p.e(e).tok().eq_ignore_ascii_case(&z_coll) {
                        i += 1;
                        continue;
                    }
                }
            }

            // No matches cause a break out of the loop
            break;
        }
        if i == n {
            let b_sort_by_group = i32::from((w.wctrl_flags as i32 & WHERE_SORTBYGROUP) != 0);
            n_order_by = n;
            if (w.wctrl_flags as i32 & WHERE_DISTINCTBY) != 0 && !src_item.rowid_used {
                e_distinct = 2 + b_sort_by_group;
            } else if w.wctrl_flags as i32 & WHERE_GROUPBY != 0 {
                e_distinct = 1 - b_sort_by_group;
            } else if w.wctrl_flags as i32 & WHERE_WANT_DISTINCT != 0 {
                e_distinct = 3;
            }
        }
    }

    // Allocate the sqlite3_index_info structure
    let mut info = IndexInfo { col_used: src_item.col_used, ..IndexInfo::default() };
    if !p.tab(tab).has_rowid() {
        // Ensure that all bits associated with PK columns are set. This is to ensure they are
        // available for cases like RIGHT joins or OR loops.
        let pk = crate::schema::sqlite3_primary_key_index(p, tab);
        for i in 0..p.idx(pk).n_key_col as usize {
            let mut i_col = p.idx(pk).ai_column[i] as i32;
            if i_col >= BMS - 1 {
                i_col = BMS - 1;
            }
            info.col_used |= 1u64 << i_col;
        }
    }
    let mut hidden = HiddenIndexInfo { wc, e_distinct, m_in: 0, m_handle_in: 0, a_rhs: vec![None; n_term] };
    let mut j = 0;
    let mut i = 0i32;
    let mut c = Some(wc);
    while let Some(x) = c {
        for k in 0..w.a_wc[x].a.len() {
            let t = &w.a_wc[x].a[k];
            let i_term = i + k as i32;
            if (t.wt_flags & TERM_OK) == 0 {
                continue;
            }
            let mut cons = IndexConstraint { i_column: t.left_column, i_term_offset: i_term, ..IndexConstraint::default() };
            let mut op = t.e_operator & WO_ALL;
            if op == WO_IN {
                if (t.wt_flags & TERM_SLICE) == 0 {
                    hidden.m_in |= smaskbit32(j as i32);
                }
                op = WO_EQ;
            }
            if op == WO_AUX {
                cons.op = t.e_match_op;
            } else if op & (WO_ISNULL | WO_IS) != 0 {
                if op == WO_ISNULL {
                    cons.op = crate::vtab::SQLITE_INDEX_CONSTRAINT_ISNULL;
                } else {
                    cons.op = crate::vtab::SQLITE_INDEX_CONSTRAINT_IS;
                }
            } else {
                // The direct assignment is possible only because the WO_ and
                // SQLITE_INDEX_CONSTRAINT_ codes are identical.
                cons.op = op as u8;
                if op & (WO_LT | WO_LE | WO_GT | WO_GE) != 0 && crate::expr::sqlite3_expr_is_vector(p, p.e(t.p_expr).p_right) {
                    if j < 16 {
                        m_no_omit |= 1 << j;
                    }
                    if op == WO_LT {
                        cons.op = WO_LE as u8;
                    }
                    if op == WO_GT {
                        cons.op = WO_GE as u8;
                    }
                }
            }
            info.a_constraint.push(cons);
            j += 1;
        }
        i += w.a_wc[x].a.len() as i32;
        c = w.a_wc[x].p_outer;
    }
    info.a_constraint_usage = vec![IndexConstraintUsage::default(); info.a_constraint.len()];
    for i in 0..n_order_by {
        let e = p.l(order_by).a[i].p_expr;
        if crate::expr::sqlite3_expr_is_constant_no_parse(p, e) != 0 {
            continue;
        }
        info.a_order_by.push(IndexOrderBy {
            i_column: p.e(e).i_column as i32,
            desc: p.l(order_by).a[i].sort_flags & KEYINFO_ORDER_DESC != 0,
        });
    }
    (info, hidden, m_no_omit)
}

/// `sqlite3LogEstFromDouble(x)`: convert a double into a LogEst. In other words, compute an
/// approximation for 10*log2(x).
pub fn sqlite3_log_est_from_double(x: f64) -> LogEst {
    if x <= 1.0 {
        return 0;
    }
    if x <= 2_000_000_000.0 {
        return crate::select::sqlite3_log_est(x as u64);
    }
    let a = x.to_bits();
    let e = (a >> 52) as i64 - 1022;
    (e * 10) as LogEst
}

/// `vtabBestIndex(pParse, pTab, p)`: invoke the xBestIndex() method of the virtual table with
/// the sqlite3_index_info object. If an error occurs, pParse is populated with an error
/// message and an appropriate error code is returned. A return of SQLITE_CONSTRAINT from
/// xBestIndex is not considered an error. SQLITE_CONSTRAINT indicates that the current
/// configuration of "unusable" flags in sqlite3_index_info can not result in a valid plan.
fn vtab_best_index(p: &mut Parse, w: &WhereInfo, tab: TableId, info: &mut IndexInfo, hidden: &mut HiddenIndexInfo) -> i32 {
    let vt = crate::vtab::sqlite3_get_vtable(p, tab).cloned().expect("VTable");
    dump_index_info_inputs(p, tab, info, hidden);
    let host = p.db.host;
    let term_offsets = info.a_constraint.iter().map(|c| c.i_term_offset).collect();
    let (rc, z_err) = {
        let mut calls = HiddenCalls { p, w, h: hidden, term_offsets };
        host.vtab_best_index(vt.id, info, &mut calls)
    };
    dump_index_info_outputs(p, rc, info, hidden);

    if rc != SQLITE_OK && rc != crate::vtab::SQLITE_CONSTRAINT {
        if rc == SQLITE_NOMEM {
            p.error_msg("out of memory");
        } else {
            match z_err {
                None => p.error_msg(crate::vtab::sqlite3_err_str(rc)),
                Some(m) => p.error_msg(m),
            }
        }
    }
    if vt.b_all_schemas {
        // sqlite3VtabUsesAllSchemas(pParse)
        let n_db = p.db.a_db.len();
        for i in 0..n_db {
            crate::build::sqlite3_code_verify_schema(p, i as i32);
        }
        if p.write_mask != 0 {
            for i in 0..n_db {
                crate::build::sqlite3_begin_write_operation(p, false, i as i32);
            }
        }
    }
    rc
}

/// (V4b verification aid) the inputs of an xBestIndex call: `BI` line of the where dump.
fn dump_index_info_inputs(p: &mut Parse, tab: TableId, info: &IndexInfo, h: &HiddenIndexInfo) {
    if p.where_dump.is_none() {
        return;
    }
    let cons: Vec<String> =
        info.a_constraint.iter().map(|c| format!("{}:{}:{}:{}", c.i_column, c.op, u8::from(c.usable), c.i_term_offset)).collect();
    let ob: Vec<String> = info.a_order_by.iter().map(|o| format!("{}:{}", o.i_column, u8::from(o.desc))).collect();
    let line = format!(
        "BI tab={} nCons={} cons=[{}] ob=[{}] colUsed={:x} distinct={} mIn={:x}",
        p.tab(tab).z_name,
        info.a_constraint.len(),
        cons.join(","),
        ob.join(","),
        info.col_used,
        h.e_distinct,
        h.m_in
    );
    if let Some(d) = p.where_dump.as_mut() {
        d.push(line);
    }
}

/// (V4b verification aid) the outputs of an xBestIndex call: `BO` line of the where dump.
fn dump_index_info_outputs(p: &mut Parse, rc: i32, info: &IndexInfo, h: &HiddenIndexInfo) {
    if p.where_dump.is_none() {
        return;
    }
    let usage: Vec<String> = info.a_constraint_usage.iter().map(|u| format!("{}:{}", u.argv_index, u8::from(u.omit))).collect();
    let line = format!(
        "BO rc={} idxNum={} idxStr={} cost={:016x} rows={} obc={} flags={:x} hIn={:x} usage=[{}]",
        rc,
        info.idx_num,
        info.idx_str.as_deref().unwrap_or("(null)"),
        info.estimated_cost.to_bits(),
        info.estimated_rows,
        u8::from(info.order_by_consumed),
        info.idx_flags,
        h.m_handle_in,
        usage.join(",")
    );
    if let Some(d) = p.where_dump.as_mut() {
        d.push(line);
    }
}

/// C keeps `u.vtab` and `u.btree` of a WhereLoop in one union: what a virtual-table loop
/// leaves in `pBuilder->pNew` is what the next table's `whereLoopAddBtree()` finds in the
/// `u.btree` fields it does not reset (`nBtm` / `nTop` of an automatic-index loop). This
/// mirrors the bytes: `nEq`/`nBtm` = `idxNum`, `nTop` = the bit-field byte (needFree,
/// bOmitOffset, bIdxNumHex) and `isOrdered`, `nDistinctCol` = `omitMask` (x86-64 layout).
fn alias_vtab_into_btree(l: &mut WhereLoop) {
    let n = l.vt_idx_num as u32;
    l.n_eq = (n & 0xffff) as u16;
    l.n_btm = (n >> 16) as u16;
    let bits = u16::from(l.vt_need_free) | (u16::from(l.vt_b_omit_offset) << 1) | (u16::from(l.vt_b_idx_num_hex) << 2);
    l.n_top = bits | (u16::from(l.vt_is_ordered as u8) << 8);
    l.n_distinct_col = l.vt_omit_mask;
}

/// `isLimitTerm(pTerm)`: return true if pTerm is a virtual table LIMIT or OFFSET term.
fn is_limit_term(t: &WhereTerm) -> bool {
    t.e_match_op >= crate::vtab::SQLITE_INDEX_CONSTRAINT_LIMIT && t.e_match_op <= crate::vtab::SQLITE_INDEX_CONSTRAINT_OFFSET
}

/// `allConstraintsUsed(aUsage, nCons)`: return true if the first nCons constraints in the
/// pUsage array are marked as in-use (have argvIndex>0). False otherwise.
fn all_constraints_used(a_usage: &[IndexConstraintUsage], n_cons: usize) -> bool {
    a_usage[..n_cons].iter().all(|u| u.argv_index > 0)
}

/// `whereLoopAddVirtualOne(pBuilder, mPrereq, mUsable, mExclude, pIdxInfo, mNoOmit, pbIn,
/// pbRetryLimit)`: argument pIdxInfo is already populated with all constraints that may be
/// used by the virtual table identified by pBuilder->pNew->iTab. This function marks a subset
/// of those constraints usable, invokes the xBestIndex method and adds the returned plan to
/// pBuilder.
///
/// A constraint is marked usable if:
///
///   * Argument mUsable indicates that its prerequisites are available, and
///
///   * It is not one of the operators specified in the mExclude mask passed as the fourth
///     argument (which in practice is either WO_IN or 0).
///
/// Argument mPrereq is a mask of tables that must be scanned before the virtual table in
/// question. These are added to the plans prerequisites before it is added to pBuilder.
///
/// Output parameter *pbIn is set to true if the plan added to pBuilder uses one or more WO_IN
/// terms, or false otherwise.
#[allow(clippy::too_many_arguments)]
fn where_loop_add_virtual_one(
    p: &mut Parse,
    w: &mut WhereInfo,
    b: &mut WhereLoopBuilder,
    m_prereq: Bitmask,
    m_usable: Bitmask,
    m_exclude: u16,
    info: &mut IndexInfo,
    hidden: &mut HiddenIndexInfo,
    m_no_omit: u16,
    pb_in: &mut bool,
    pb_retry_limit: Option<&mut bool>,
) -> i32 {
    let wc = b.p_wc;
    let i_tab = b.p_new.i_tab as usize;
    let tab = p.src(w.p_tab_list).a[i_tab].p_s_tab;
    let n_constraint = info.a_constraint.len();
    *pb_in = false;
    b.p_new.prereq = m_prereq;

    // Set the usable flag on the subset of constraints identified by arguments mUsable and
    // mExclude.
    let has_retry = pb_retry_limit.is_some();
    for i in 0..n_constraint {
        let t = term_from_where_clause(w, wc, info.a_constraint[i].i_term_offset).expect("term");
        let term = w.term(t);
        info.a_constraint[i].usable = false;
        if (term.prereq_right & m_usable) == term.prereq_right && (term.e_operator & m_exclude) == 0 && (has_retry || !is_limit_term(term)) {
            info.a_constraint[i].usable = true;
        }
    }

    // Initialize the output fields of the sqlite3_index_info structure
    for u in info.a_constraint_usage.iter_mut() {
        *u = IndexConstraintUsage::default();
    }
    info.idx_str = None;
    info.need_to_free_idx_str = false;
    info.idx_num = 0;
    info.order_by_consumed = false;
    info.estimated_cost = crate::vtab::SQLITE_BIG_DBL / 2.0;
    info.estimated_rows = 25;
    info.idx_flags = 0;
    hidden.m_handle_in = 0;

    // Invoke the virtual table xBestIndex() method
    let rc = vtab_best_index(p, w, tab, info, hidden);
    if rc != 0 {
        if rc == crate::vtab::SQLITE_CONSTRAINT {
            // If the xBestIndex method returns SQLITE_CONSTRAINT, that means that the
            // particular combination of parameters provided is unusable. Make no entries in
            // the loop table.
            return SQLITE_OK;
        }
        return rc;
    }

    let mut mx_term: i32 = -1;
    let mut a_l_term: Vec<Option<TermRef>> = vec![None; n_constraint];
    // memset(&pNew->u.vtab, 0, sizeof(pNew->u.vtab)): the union also clears u.btree
    {
        let l = &mut b.p_new;
        l.vt_idx_num = 0;
        l.vt_need_free = false;
        l.vt_b_omit_offset = false;
        l.vt_b_idx_num_hex = false;
        l.vt_is_ordered = 0;
        l.vt_omit_mask = 0;
        l.vt_idx_str = None;
        l.vt_m_handle_in = 0;
        l.n_eq = 0;
        l.n_btm = 0;
        l.n_top = 0;
        l.n_distinct_col = 0;
        l.p_index = IndexId::NULL;
        l.p_order_by = ExprListId::NULL;
    }
    let mut pb_retry_limit = pb_retry_limit;
    for i in 0..n_constraint {
        let i_term = info.a_constraint_usage[i].argv_index - 1;
        if i_term >= 0 {
            let j = info.a_constraint[i].i_term_offset;
            let t = if j >= 0 { term_from_where_clause(w, wc, j) } else { None };
            if i_term as usize >= n_constraint || t.is_none() || a_l_term[i_term as usize].is_some() || !info.a_constraint[i].usable {
                let name = p.tab(tab).z_name.clone();
                p.error_msg(format!("{name}.xBestIndex malfunction"));
                alias_vtab_into_btree(&mut b.p_new);
                return SQLITE_ERROR;
            }
            let t = t.expect("term");
            b.p_new.prereq |= w.term(t).prereq_right;
            a_l_term[i_term as usize] = Some(t);
            if i_term > mx_term {
                mx_term = i_term;
            }
            if info.a_constraint_usage[i].omit {
                if i < 16 && ((1 << i) & m_no_omit) == 0 {
                    b.p_new.vt_omit_mask |= 1 << i_term;
                }
                if w.term(t).e_match_op == crate::vtab::SQLITE_INDEX_CONSTRAINT_OFFSET {
                    b.p_new.vt_b_omit_offset = true;
                }
            }
            if smaskbit32(i as i32) & hidden.m_handle_in != 0 {
                b.p_new.vt_m_handle_in |= 1u32.checked_shl(i_term as u32).unwrap_or(0);
            } else if (w.term(t).e_operator & WO_IN) != 0 {
                // A virtual table that is constrained by an IN clause may not consume the
                // ORDER BY clause because (1) the order of IN terms is not necessarily
                // related to the order of output terms and (2) Multiple outputs from a single
                // IN value will not merge together.
                info.order_by_consumed = false;
                info.idx_flags &= !crate::vtab::SQLITE_INDEX_SCAN_UNIQUE;
                *pb_in = true;
            }

            if is_limit_term(w.term(t)) && (*pb_in || !all_constraints_used(&info.a_constraint_usage, i)) {
                // If there is an IN(...) term handled as an == (separate call to xFilter for
                // each value on the RHS of the IN) and a LIMIT or OFFSET term handled as well,
                // the plan is unusable. Similarly, if there is a LIMIT/OFFSET and there are
                // other unused terms, the plan cannot be used. In these cases set variable
                // *pbRetryLimit to true to tell the caller to retry with LIMIT and OFFSET
                // disabled.
                if let Some(r) = pb_retry_limit.as_deref_mut() {
                    *r = true;
                }
                alias_vtab_into_btree(&mut b.p_new);
                return SQLITE_OK;
            }
        }
    }

    for i in 0..=mx_term {
        if a_l_term[i as usize].is_none() {
            // The non-zero argvIdx values must be contiguous. Raise an error if they are not
            let name = p.tab(tab).z_name.clone();
            p.error_msg(format!("{name}.xBestIndex malfunction"));
            alias_vtab_into_btree(&mut b.p_new);
            return SQLITE_ERROR;
        }
    }
    a_l_term.truncate((mx_term + 1) as usize);
    let l = &mut b.p_new;
    l.a_l_term = a_l_term;
    l.vt_idx_num = info.idx_num;
    l.vt_need_free = info.need_to_free_idx_str;
    info.need_to_free_idx_str = false;
    l.vt_idx_str = info.idx_str.clone();
    l.vt_is_ordered = if info.order_by_consumed { info.a_order_by.len() as i8 } else { 0 };
    l.vt_b_idx_num_hex = (info.idx_flags & crate::vtab::SQLITE_INDEX_SCAN_HEX) != 0;
    l.r_setup = 0;
    l.r_run = sqlite3_log_est_from_double(info.estimated_cost);
    l.n_out = crate::select::sqlite3_log_est(info.estimated_rows as u64);

    // Set the WHERE_ONEROW flag if the xBestIndex() method indicated that the scan will visit
    // at most one row. Clear it otherwise.
    if info.idx_flags & crate::vtab::SQLITE_INDEX_SCAN_UNIQUE != 0 {
        l.ws_flags |= WHERE_ONEROW as u32;
    } else {
        l.ws_flags &= !(WHERE_ONEROW as u32);
    }
    alias_vtab_into_btree(&mut b.p_new);
    let rc = where_loop_insert(p, w, b);
    if b.p_new.vt_need_free {
        b.p_new.vt_need_free = false;
    }
    alias_vtab_into_btree(&mut b.p_new);
    rc
}

/// `whereLoopAddVirtual(pBuilder, mPrereq, mUnusable)`: add all WhereLoop objects for a table
/// of the join identified by pBuilder->pNew->iTab. That table is guaranteed to be a virtual
/// table.
///
/// If there are no LEFT or CROSS JOIN joins in the query, both mPrereq and mUnusable are set
/// to 0. Otherwise, mPrereq is a mask of all FROM clause entries that occur before the virtual
/// table in the FROM clause and are separated from it by at least one LEFT or CROSS JOIN.
/// Similarly, the mUnusable mask contains all FROM clause entries that occur after the
/// virtual table and are separated from it by at least one LEFT or CROSS JOIN.
///
/// For example, if the query were:
///
///   ... FROM t1, t2 LEFT JOIN t3, t4, vt CROSS JOIN t5, t6;
///
/// then mPrereq corresponds to (t1, t2) and mUnusable to (t5, t6).
///
/// All the tables in mPrereq must be scanned before the current virtual table. So any terms
/// for which all prerequisites are satisfied by mPrereq may be specified as "usable" in all
/// calls to xBestIndex. Conversely, all tables in mUnusable must be scanned after the current
/// virtual table, so any terms for which the prerequisites overlap with mUnusable should
/// always be configured as "not-usable" for xBestIndex.
fn where_loop_add_virtual(p: &mut Parse, w: &mut WhereInfo, b: &mut WhereLoopBuilder, m_prereq: Bitmask, m_unusable: Bitmask) -> i32 {
    let wc = b.p_wc;
    let i_tab = b.p_new.i_tab as usize;
    let (mut info, mut hidden, m_no_omit) = allocate_index_info(p, w, wc, m_unusable, i_tab);
    b.p_new.r_setup = 0;
    b.p_new.ws_flags = WHERE_VIRTUALTABLE as u32;
    b.p_new.a_l_term.clear();
    b.p_new.vt_need_free = false;
    b.p_new.n_top &= !1; // (the needFree bit, see alias_vtab_into_btree())
    let n_constraint = info.a_constraint.len();
    let mut b_in = false;
    let mut b_retry = false;

    // First call xBestIndex() with all constraints usable.
    let mut rc = where_loop_add_virtual_one(p, w, b, m_prereq, ALLBITS, 0, &mut info, &mut hidden, m_no_omit, &mut b_in, Some(&mut b_retry));
    if b_retry {
        rc = where_loop_add_virtual_one(p, w, b, m_prereq, ALLBITS, 0, &mut info, &mut hidden, m_no_omit, &mut b_in, None);
    }

    // If the call to xBestIndex() with all terms enabled produced a plan that does not
    // require any source tables (IOW: a plan with mBest==0) and does not use an IN(...)
    // operator, then there is no point in making any further calls to xBestIndex() since they
    // will all return the same result (if the xBestIndex() implementation is sane).
    let m_best = b.p_new.prereq & !m_prereq;
    if rc == SQLITE_OK && (m_best != 0 || b_in) {
        let mut seen_zero = false; // True if a plan with no prereqs seen
        let mut seen_zero_no_in = false; // Plan with no prereqs and no IN(...) seen
        let mut m_prev: Bitmask = 0;
        let mut m_best_no_in: Bitmask = 0;

        // If the plan produced by the earlier call uses an IN(...) term, call xBestIndex
        // again, this time with IN(...) terms disabled.
        if b_in {
            rc = where_loop_add_virtual_one(p, w, b, m_prereq, ALLBITS, WO_IN, &mut info, &mut hidden, m_no_omit, &mut b_in, None);
            m_best_no_in = b.p_new.prereq & !m_prereq;
            if m_best_no_in == 0 {
                seen_zero = true;
                seen_zero_no_in = true;
            }
        }

        // Call xBestIndex once for each distinct value of (prereqRight & ~mPrereq) in the set
        // of terms that apply to the current virtual table.
        while rc == SQLITE_OK {
            let mut m_next: Bitmask = ALLBITS;
            for i in 0..n_constraint {
                let i_term = info.a_constraint[i].i_term_offset;
                let t = term_from_where_clause(w, wc, i_term).expect("term");
                let m_this = w.term(t).prereq_right & !m_prereq;
                if m_this > m_prev && m_this < m_next {
                    m_next = m_this;
                }
            }
            m_prev = m_next;
            if m_next == ALLBITS {
                break;
            }
            if m_next == m_best || m_next == m_best_no_in {
                continue;
            }
            rc = where_loop_add_virtual_one(p, w, b, m_prereq, m_next | m_prereq, 0, &mut info, &mut hidden, m_no_omit, &mut b_in, None);
            if b.p_new.prereq == m_prereq {
                seen_zero = true;
                if !b_in {
                    seen_zero_no_in = true;
                }
            }
        }

        // If the calls to xBestIndex() in the above loop did not find a plan that requires no
        // source tables at all (i.e. one guaranteed to be usable), make a call here with all
        // source tables disabled
        if rc == SQLITE_OK && !seen_zero {
            rc = where_loop_add_virtual_one(p, w, b, m_prereq, m_prereq, 0, &mut info, &mut hidden, m_no_omit, &mut b_in, None);
            if !b_in {
                seen_zero_no_in = true;
            }
        }

        // If the calls to xBestIndex() have so far failed to find a plan that requires no
        // source tables at all and does not use an IN(...) operator, make a final call to
        // obtain one here.
        if rc == SQLITE_OK && !seen_zero_no_in {
            rc = where_loop_add_virtual_one(p, w, b, m_prereq, m_prereq, WO_IN, &mut info, &mut hidden, m_no_omit, &mut b_in, None);
        }
    }

    // freeIndexInfo(pParse->db, p)
    rc
}

/// `whereLoopAddAll(pBuilder)`: add all WhereLoop objects for all tables
fn where_loop_add_all(p: &mut Parse, w: &mut WhereInfo, b: &mut WhereLoopBuilder) -> i32 {
    let mut m_prereq: Bitmask = 0;
    let mut m_prior: Bitmask = 0;
    let tab_list = w.p_tab_list;
    let mut rc = SQLITE_OK;
    let mut b_first_past_rj = false;
    let mut has_right_cross_join = false;

    // Loop over the tables in the join, from left to right
    b.i_plan_limit = SQLITE_QUERY_PLANNER_LIMIT;
    for i_tab in 0..w.n_level as usize {
        let mut m_unusable: Bitmask = 0;
        let (jointype, i_cursor, from_exists, tab) = {
            let it = &p.src(tab_list).a[i_tab];
            (it.jointype as i32, it.i_cursor, it.from_exists, it.p_s_tab)
        };
        b.p_new.i_tab = i_tab as u8;
        b.i_plan_limit += SQLITE_QUERY_PLANNER_LIMIT_INCR;
        b.p_new.mask_self = sqlite3_where_get_mask(&w.s_mask_set, i_cursor);
        if b_first_past_rj || (jointype & (JT_OUTER | JT_CROSS | JT_LTORJ)) != 0 {
            // Add prerequisites to prevent reordering of FROM clause terms across CROSS joins
            // and outer joins. The bFirstPastRJ boolean prevents the right operand of a
            // RIGHT JOIN from being reordered also.
            //
            // The hasRightCrossJoin flag prevent FROM-clause terms from moving from the
            // right side of a LEFT JOIN or CROSS JOIN over to the left side when they are
            // part of an inner join...
            if jointype & (JT_LTORJ | JT_CROSS) != 0 {
                has_right_cross_join = true;
            }
            m_prereq |= m_prior;
            b_first_past_rj = (jointype & JT_RIGHT) != 0;
        } else if from_exists {
            // joins that result from the EXISTS-to-JOIN optimization should not be moved to
            // the left of any of their dependencies
            let n_base = w.a_wc[0].n_base as usize;
            for i in 0..n_base {
                let pa = w.a_wc[0].a[i].prereq_all;
                if (b.p_new.mask_self & pa) != 0 {
                    m_prereq |= pa & (b.p_new.mask_self - 1);
                }
            }
        } else if !has_right_cross_join {
            m_prereq = 0;
        }
        if p.tab(tab).is_virtual() {
            for k in i_tab + 1..w.n_level as usize {
                let (jt, cur) = {
                    let it = &p.src(tab_list).a[k];
                    (it.jointype as i32, it.i_cursor)
                };
                if (jt & (JT_OUTER | JT_CROSS)) != 0 {
                    m_unusable |= sqlite3_where_get_mask(&w.s_mask_set, cur);
                }
            }
            rc = where_loop_add_virtual(p, w, b, m_prereq, m_unusable);
        } else {
            rc = where_loop_add_btree(p, w, b, m_prereq);
        }
        if rc == SQLITE_OK && w.a_wc[b.p_wc].has_or {
            rc = where_loop_add_or(p, w, b, m_prereq, m_unusable);
        }
        m_prior |= b.p_new.mask_self;
        if rc != 0 {
            if rc == SQLITE_DONE {
                // We hit the query planner search limit set by iPlanLimit
                // sqlite3_log(SQLITE_WARNING, "abbreviated query algorithm search");
                rc = SQLITE_OK;
            } else {
                break;
            }
        }
    }
    // whereLoopClear(db, pNew)
    b.p_new.a_l_term.clear();
    b.p_new.ws_flags = 0;
    rc
}

/// `wherePathMatchSubqueryOB(pLoop, iLoop, iCur, pOrderBy, pRevMask, pOBSat, wctrlFlags)`:
/// implementation of the order-by-subquery optimization:
///
/// WhereLoop pLoop, which the iLoop-th term of the nested loop, is really a subquery or CTE.
/// Its output order is described by pLoop->u.btree.pOrderBy. Check whether the ORDER BY
/// (or GROUP BY) clause of the outer query is satisfied, in whole or in part, by the order
/// in which the subquery returns its rows. Return true if at least one term was matched.
#[allow(clippy::too_many_arguments)]
fn where_path_match_subquery_ob(
    p: &Parse,
    l: &WhereLoop,
    i_loop: i32,
    i_cur: i32,
    order_by: ExprListId,
    p_rev_mask: &mut Bitmask,
    p_ob_sat: &mut Bitmask,
    wctrl_flags: u16,
) -> bool {
    use sqliters_vdbe::vdbeaux::{KEYINFO_ORDER_BIGNULL, KEYINFO_ORDER_DESC};
    let mut rev: u8 = 0; // True if iOB and jSub sort in opposite directions
    let sub_ob = l.p_order_by;
    let mut i_ob = 0usize;
    while ((1u64 << i_ob) & *p_ob_sat) != 0 {
        i_ob += 1;
    }
    let mut j_sub = 0usize;
    while j_sub < p.l(sub_ob).a.len() && i_ob < p.l(order_by).a.len() {
        let sub_item = &p.l(sub_ob).a[j_sub];
        if sub_item.i_order_by_col == 0 {
            break;
        }
        let ob_expr = p.l(order_by).a[i_ob].p_expr;
        if p.e(ob_expr).op != TK_COLUMN && p.e(ob_expr).op != TK_AGG_COLUMN {
            break;
        }
        if p.e(ob_expr).i_table != i_cur {
            break;
        }
        if p.e(ob_expr).i_column as i32 != sub_item.i_order_by_col as i32 - 1 {
            break;
        }
        if (wctrl_flags as i32 & WHERE_GROUPBY) == 0 {
            let sf_ob = p.l(order_by).a[i_ob].sort_flags; // sortFlags for iOB
            let sf_sub = sub_item.sort_flags; // sortFlags for jSub
            if (sf_sub & KEYINFO_ORDER_BIGNULL) != (sf_ob & KEYINFO_ORDER_BIGNULL) {
                break;
            }
            let rev_idx = sf_sub & KEYINFO_ORDER_DESC;
            if j_sub > 0 {
                if (rev ^ rev_idx) != (sf_ob & KEYINFO_ORDER_DESC) {
                    break;
                }
            } else {
                rev = rev_idx ^ (sf_ob & KEYINFO_ORDER_DESC);
                if rev != 0 {
                    if (l.ws_flags & WHERE_COROUTINE as u32) != 0 {
                        // Cannot run a co-routine in reverse order
                        break;
                    }
                    *p_rev_mask |= 1u64 << i_loop;
                }
            }
        }
        *p_ob_sat |= 1u64 << i_ob;
        j_sub += 1;
        i_ob += 1;
    }
    j_sub > 0
}

/// `wherePathSatisfiesOrderBy(pWInfo, pOrderBy, pPath, wctrlFlags, nLoop, pLast, pRevMask)`:
/// examine a WherePath (with the addition of the extra WhereLoop of the 6th parameters) to
/// see if it outputs rows in the requested ORDER BY (or GROUP BY) without requiring a
/// separate sort operation. Return N:
///
///   N>0:   N terms of the ORDER BY clause are satisfied
///   N==0:  No terms of the ORDER BY clause are satisfied
///   N<0:   Unknown yet how many terms of ORDER BY might be satisfied.
///
/// Note that processing for WHERE_GROUPBY and WHERE_DISTINCTBY is not as strict. With GROUP
/// BY and DISTINCT the only requirement is that equivalent rows appear immediately adjacent
/// to one another. GROUP BY and DISTINCT do not require rows to appear in any particular
/// order as long as equivalent rows are grouped together. Thus for GROUP BY and DISTINCT the
/// pOrderBy terms can be matched in any order. With ORDER BY, the pOrderBy terms must be
/// matched in strict left-to-right order.
#[allow(clippy::too_many_arguments)]
fn where_path_satisfies_order_by(
    p: &mut Parse,
    w: &mut WhereInfo,
    order_by: ExprListId,
    path_loops: &[usize],
    wctrl_flags: u16,
    n_loop: u16,
    last: usize,
    p_rev_mask: &mut Bitmask,
) -> i8 {
    use sqliters_vdbe::vdbeaux::{KEYINFO_ORDER_BIGNULL, KEYINFO_ORDER_DESC};
    let mut ob_sat: Bitmask = 0; // Mask of ORDER BY terms satisfied so far

    // We say the WhereLoop is "one-row" if it generates no more than one row of output. A
    // WhereLoop is one-row if all of the following are true:
    //  (a) All index columns match with WHERE_COLUMN_EQ.
    //  (b) The index is unique
    // Any WhereLoop with an WHERE_COLUMN_EQ constraint on the rowid is one-row. Every
    // one-row WhereLoop will have the WHERE_ONEROW bit set in wsFlags.
    //
    // We say the WhereLoop is "order-distinct" if the set of columns from that WhereLoop
    // that are in the ORDER BY clause are different for every row of the WhereLoop. Every
    // one-row WhereLoop is automatically order-distinct. A WhereLoop that has no columns in
    // the ORDER BY clause is not order-distinct. To be order-distinct is not quite the same
    // as being UNIQUE since a UNIQUE column or index can have multiple rows that are NULL
    // and NULL values are equivalent for the purpose of order-distinct. To be order-distinct,
    // the columns must be UNIQUE and NOT NULL.
    //
    // The rowid for a table is always UNIQUE and NOT NULL so whenever the rowid is part of
    // the ORDER BY clause, the loop is order-distinct. If there is a WHERE_INDEXED loop with
    // a UNIQUE index, it is order-distinct if every column of the index is NOT NULL.
    if n_loop != 0 && p.db.optimization_disabled(SQLITE_OrderByIdxJoin) {
        return 0;
    }

    let n_order_by = p.l(order_by).n_expr() as u16;
    if n_order_by as i32 > BMS - 1 {
        return 0; // Cannot optimize overly large ORDER BYs
    }
    let mut is_order_distinct = true;
    let ob_done: Bitmask = (1u64 << n_order_by) - 1;
    let mut order_distinct_mask: Bitmask = 0;
    let mut ready: Bitmask = 0;
    let mut eq_op_mask: u16 = WO_EQ | WO_IS | WO_ISNULL;
    if wctrl_flags as i32 & (WHERE_ORDERBY_LIMIT | WHERE_ORDERBY_MAX | WHERE_ORDERBY_MIN) != 0 {
        eq_op_mask |= WO_IN;
    }
    let mut cur_loop: usize = usize::MAX;
    let mut i_loop: i32 = 0;
    while is_order_distinct && ob_sat < ob_done && i_loop <= n_loop as i32 {
        if i_loop > 0 {
            ready |= w.loops[cur_loop].mask_self;
        }
        if i_loop < n_loop as i32 {
            cur_loop = path_loops[i_loop as usize];
            if wctrl_flags as i32 & WHERE_ORDERBY_LIMIT != 0 {
                i_loop += 1;
                continue;
            }
        } else {
            cur_loop = last;
        }
        if w.loops[cur_loop].ws_flags & WHERE_VIRTUALTABLE as u32 != 0 {
            if w.loops[cur_loop].vt_is_ordered != 0 && w.p_order_by == order_by {
                ob_sat = ob_done;
            } else {
                // No further ORDER BY terms may be matched. So this call should return >=0,
                // not -1. Clear isOrderDistinct to ensure it does so.
                is_order_distinct = false;
            }
            break;
        }
        let i_cur = p.src(w.p_tab_list).a[w.loops[cur_loop].i_tab as usize].i_cursor;

        // Mark off any ORDER BY term X that is a column in the table of the current loop for
        // which there is a term in the WHERE clause of the form X IS NULL or X=? that
        // reference only outer loops.
        for i in 0..n_order_by as usize {
            if (1u64 << i) & ob_sat != 0 {
                continue;
            }
            let ob_e = p.l(order_by).a[i].p_expr;
            let ob_expr = sqlite3_expr_skip_collate_and_likely(p, ob_e);
            if ob_expr.is_null() {
                continue;
            }
            if p.e(ob_expr).op != TK_COLUMN && p.e(ob_expr).op != TK_AGG_COLUMN {
                continue;
            }
            if p.e(ob_expr).i_table != i_cur {
                continue;
            }
            let ob_col = p.e(ob_expr).i_column as i32;
            let Some(tr) = sqlite3_where_find_term(p, w, 0, i_cur, ob_col, !ready, eq_op_mask as u32, IndexId::NULL) else {
                continue;
            };
            let term = w.term(tr).clone();
            if term.e_operator == WO_IN {
                // IN terms are only valid for sorting in the ORDER BY LIMIT optimization, and
                // then only if they are actually used by the query plan
                if !w.loops[cur_loop].a_l_term.iter().any(|t| *t == Some(tr)) {
                    continue;
                }
            }
            if (term.e_operator & (WO_EQ | WO_IS)) != 0 && ob_col >= 0 {
                let coll1 = sqlite3_expr_nn_coll_seq(p, ob_e);
                let c_expr = term.p_expr;
                let Some(coll2) = sqlite3_expr_compare_coll_seq(p, c_expr) else { continue };
                if !coll1.name.eq_ignore_ascii_case(&coll2.name) {
                    continue;
                }
                let aff_right = sqlite3_expr_affinity(p, p.e(c_expr).p_right);
                // Left operand must be a column, hence always has affinity
                if aff_right != 0 && aff_right != sqlite3_expr_affinity(p, p.e(c_expr).p_left) {
                    continue; // An affinity conversion would be required
                }
            }
            ob_sat |= 1u64 << i;
        }

        if (w.loops[cur_loop].ws_flags & WHERE_ONEROW as u32) == 0 {
            let (n_column, n_key_col, index);
            if w.loops[cur_loop].ws_flags & WHERE_IPK as u32 != 0 {
                if w.loops[cur_loop].p_order_by.nn() && p.db.optimization_enabled(SQLITE_OrderBySubq) && {
                    let l = w.loops[cur_loop].clone();
                    where_path_match_subquery_ob(p, &l, i_loop, i_cur, order_by, p_rev_mask, &mut ob_sat, wctrl_flags)
                } {
                    n_column = 0;
                    is_order_distinct = false;
                } else {
                    n_column = 1;
                }
                index = IndexId::NULL;
                n_key_col = 0;
            } else {
                index = w.loops[cur_loop].p_index;
                if index.is_null() || p.idx(index).b_unordered {
                    return 0;
                } else {
                    n_key_col = p.idx(index).n_key_col;
                    n_column = p.idx(index).n_column;
                    // All relevant terms of the index must also be non-NULL in order for
                    // isOrderDistinct to be true. So the isOrderDistinct value computed here
                    // might be set to 0 later on if the index column is nullable.
                    is_order_distinct = p.idx(index).is_unique() && (w.loops[cur_loop].ws_flags & WHERE_SKIPSCAN as u32) == 0;
                }
            }

            // Loop through all columns of the index and deal with the ones that are not
            // constrained by == or IN.
            let mut rev: u8 = 0;
            let mut rev_set = false;
            let mut distinct_columns = false;
            for j in 0..n_column as usize {
                let mut b_once = true; // True to run the ORDER BY search loop
                let (n_eq, n_skip) = (w.loops[cur_loop].n_eq as usize, w.loops[cur_loop].n_skip as usize);
                if j < n_eq && j >= n_skip {
                    let tj = w.loops[cur_loop].a_l_term[j].expect("term");
                    let e_op = w.term(tj).e_operator;

                    // Skip over == and IS and ISNULL terms. (Also skip IN terms when doing
                    // WHERE_ORDERBY_LIMIT processing). Except, IS and ISNULL terms imply
                    // that the index is not UNIQUE NOT NULL in which case the loop might
                    // deliver multiple rows for the same key.
                    if (e_op & eq_op_mask) != 0 {
                        if e_op & (WO_ISNULL | WO_IS) != 0 {
                            is_order_distinct = false;
                        }
                        continue;
                    } else if e_op & WO_IN != 0 {
                        // ALWAYS() justification: eOp is an equality operator due to the
                        // j<pLoop->u.btree.nEq constraint above. Any equality other than WO_IN
                        // is captured by the previous "if". So this one always has to be
                        // WO_IN.
                        let x = w.term(tj).p_expr;
                        for i in j + 1..n_eq {
                            let ti = w.loops[cur_loop].a_l_term[i].expect("term");
                            if w.term(ti).p_expr == x {
                                b_once = false;
                                break;
                            }
                        }
                    }
                }

                // Get the column number in the table (iColumn) and sort order (revIdx) for
                // the j-th column of the index.
                let (mut i_column, rev_idx);
                if index.nn() {
                    i_column = p.idx(index).ai_column[j] as i32;
                    rev_idx = p.idx(index).a_sort_order[j] & KEYINFO_ORDER_DESC;
                    if i_column == p.tab(p.idx(index).p_table).i_p_key as i32 {
                        i_column = XN_ROWID;
                    }
                } else {
                    i_column = XN_ROWID;
                    rev_idx = 0;
                }

                // An unconstrained column that might be NULL means that this WhereLoop is not
                // well-ordered.
                if is_order_distinct {
                    if i_column >= 0 && j >= n_eq && p.tab(p.idx(index).p_table).a_col[i_column as usize].not_null == 0 {
                        is_order_distinct = false;
                    }
                    if i_column == XN_EXPR {
                        is_order_distinct = false;
                    }
                }

                // Find the ORDER BY term that corresponds to the j-th column of the index and
                // mark that ORDER BY term having been satisfied.
                let mut is_match = false;
                let mut i = 0usize;
                while b_once && i < n_order_by as usize {
                    if (1u64 << i) & ob_sat != 0 {
                        i += 1;
                        continue;
                    }
                    let ob_e = p.l(order_by).a[i].p_expr;
                    let ob_expr = sqlite3_expr_skip_collate_and_likely(p, ob_e);
                    if ob_expr.is_null() {
                        i += 1;
                        continue;
                    }
                    if (wctrl_flags as i32 & (WHERE_GROUPBY | WHERE_DISTINCTBY)) == 0 {
                        b_once = false;
                    }
                    if i_column >= XN_ROWID {
                        if p.e(ob_expr).op != TK_COLUMN && p.e(ob_expr).op != TK_AGG_COLUMN {
                            i += 1;
                            continue;
                        }
                        if p.e(ob_expr).i_table != i_cur {
                            i += 1;
                            continue;
                        }
                        if p.e(ob_expr).i_column as i32 != i_column {
                            i += 1;
                            continue;
                        }
                    } else {
                        let ix_expr = p.l(p.idx(index).a_col_expr).a[j].p_expr;
                        if sqlite3_expr_compare_skip(p, ob_expr, ix_expr, i_cur) != 0 {
                            i += 1;
                            continue;
                        }
                    }
                    if i_column != XN_ROWID {
                        let coll = sqlite3_expr_nn_coll_seq(p, ob_e);
                        if !coll.name.eq_ignore_ascii_case(&p.idx(index).az_coll[j]) {
                            i += 1;
                            continue;
                        }
                    }
                    if wctrl_flags as i32 & WHERE_DISTINCTBY != 0 {
                        w.loops[cur_loop].n_distinct_col = j as u16 + 1;
                    }
                    is_match = true;
                    break;
                }
                let sf = if is_match { p.l(order_by).a[i].sort_flags } else { 0 };
                if is_match && (wctrl_flags as i32 & WHERE_GROUPBY) == 0 {
                    // Make sure the sort order is compatible in an ORDER BY clause. Sort
                    // order is irrelevant for a GROUP BY clause.
                    if rev_set {
                        if (rev ^ rev_idx) != (sf & KEYINFO_ORDER_DESC) {
                            is_match = false;
                        }
                    } else {
                        rev = rev_idx ^ (sf & KEYINFO_ORDER_DESC);
                        if rev != 0 {
                            *p_rev_mask |= 1u64 << i_loop;
                        }
                        rev_set = true;
                    }
                }
                if is_match && (sf & KEYINFO_ORDER_BIGNULL) != 0 {
                    if j == n_eq {
                        w.loops[cur_loop].ws_flags |= WHERE_BIGNULL_SORT as u32;
                    } else {
                        is_match = false;
                    }
                }
                if is_match {
                    if i_column == XN_ROWID {
                        distinct_columns = true;
                    }
                    ob_sat |= 1u64 << i;
                } else {
                    // No match found
                    if j == 0 || j < n_key_col as usize {
                        is_order_distinct = false;
                    }
                    break;
                }
            } // end Loop over all index columns
            if distinct_columns {
                is_order_distinct = true;
            }
        } // end-if not one-row

        // Mark off any other ORDER BY terms that reference pLoop
        if is_order_distinct {
            order_distinct_mask |= w.loops[cur_loop].mask_self;
            for i in 0..n_order_by as usize {
                if (1u64 << i) & ob_sat != 0 {
                    continue;
                }
                let e = p.l(order_by).a[i].p_expr;
                let m_term = sqlite3_where_expr_usage(p, &mut w.s_mask_set, e);
                if m_term == 0 && sqlite3_expr_is_constant_no_parse(p, e) == 0 {
                    continue;
                }
                if (m_term & !order_distinct_mask) == 0 {
                    ob_sat |= 1u64 << i;
                }
            }
        }
        i_loop += 1;
    } // End the loop over all WhereLoops from outer-most down to inner-most
    if ob_sat == ob_done {
        return n_order_by as i8;
    }
    if !is_order_distinct {
        let mut i = n_order_by as i32 - 1;
        while i > 0 {
            let m: Bitmask = if i < BMS { (1u64 << i) - 1 } else { 0 };
            if (ob_sat & m) == m {
                return i as i8;
            }
            i -= 1;
        }
        return 0;
    }
    -1
}

/// `whereSortingCost(pWInfo, nRow, nOrderBy, nSorted)`: return the cost of sorting nRow rows,
/// assuming that the keys have nOrderby columns and that the first nSorted columns are
/// already in order.
fn where_sorting_cost(p: &Parse, w: &WhereInfo, mut n_row: LogEst, n_order_by: i32, n_sorted: i32) -> LogEst {
    // Estimated cost of a full external sort, where N is the number of rows to sort is:
    //
    //   cost = (K * N * log(N)).
    //
    // Or, if the order-by clause has X terms but only the last Y terms are out of order, then
    // block-sorting will reduce the sorting cost to:
    //
    //   cost = (K * N * log(N)) * (Y/X)
    //
    // The constant K is at least 2.0 but will be larger if there are a large number of
    // columns to be sorted, as the sorting time is proportional to the amount of content to
    // be sorted. The algorithm does not currently distinguish between fat columns (BLOBs and
    // TEXTs) and thin columns (INTs and REALs), which is unfortunate.
    //
    // TUNING: sorting cost proportional to the number of output columns:
    let n_col = sqlite3_log_est(((p.l(p.s(w.p_select).p_e_list).n_expr() + 59) / 30) as u64);
    let mut r_sort_cost = n_row + n_col;
    if n_sorted > 0 {
        // Scale the result by (Y/X)
        r_sort_cost += sqlite3_log_est(((n_order_by - n_sorted) * 100 / n_order_by) as u64) - 66;
    }

    // Multiple by log(M) where M is the number of output rows. Use the LIMIT for M if it is
    // smaller. Or if this sort is for a DISTINCT operation, M will be the number of distinct
    // output rows, so set it equal to 0.5*N.
    if (w.wctrl_flags as i32 & WHERE_USE_LIMIT) != 0 {
        r_sort_cost += 10; // TUNING: Extra 2.0x if using LIMIT
        if n_sorted != 0 {
            r_sort_cost += 6; // TUNING: Extra 1.5x if also using partial sort
        }
        if w.i_limit < n_row {
            n_row = w.i_limit;
        }
    } else if (w.wctrl_flags as i32 & WHERE_WANT_DISTINCT) != 0 {
        // TUNING: In the sort for a DISTINCT operator, assume that the DISTINCT reduces the
        // number of output rows by a factor of 2
        if n_row > 10 {
            n_row -= 10;
        }
    }
    r_sort_cost += est_log(n_row);
    r_sort_cost
}

/// `computeMxChoice(pWInfo)`: compute the maximum number of paths in the solver algorithm,
/// for queries that have three or more terms in the FROM clause. Queries with zero, one, or
/// two FROM-clause terms use only a single path, or five paths.
///
/// Also run the star-schema heuristic: when a "fact table" with three or more "dimension
/// tables" is found, the cost of full scans of the dimension tables is raised to the cost
/// of the most expensive loop of the fact table so that the solver puts the fact table
/// on an outer loop.
fn compute_mx_choice(p: &Parse, w: &mut WhereInfo) -> i32 {
    let n_loop = w.n_level as usize; // Number of terms in the join
    if n_loop >= 4 // Constraint (aa)
        && !w.b_star_done
        && p.db.optimization_enabled(SQLITE_StarQuery)
    {
        let from_tabs = &p.src(w.p_tab_list).a; // All terms of the FROM clause
        let mut m_self_join: Bitmask = 0; // Tables that cannot be dimension tables
        w.b_star_done = true; // Only do this computation once

        // Look for fact tables with three or more dimensions where the dimension tables are
        // not separately from the fact tables by an outer or cross join. Adjust cost weights
        // if found.
        let mut start = 0usize; // Where to start searching for dimension-tables
        let n = w.loops.len();
        let mut m: Bitmask = 1;
        for i_from_idx in 0..n_loop {
            let mut n_dep = 0; // Number of dimension tables
            let mut m_seen: Bitmask = 0; // Mask of dimension tables
            let fact_tab = &from_tabs[i_from_idx]; // The candidate fact table
            if (fact_tab.jointype as i32 & (JT_OUTER | JT_CROSS)) != 0 {
                // If the candidate fact-table is the right table of an outer join restrict
                // the search for dimension-tables to be tables to the right of the
                // fact-table. Constraint (bb)
                if i_from_idx + 3 > n_loop {
                    break; // ^-- Impossible to reach nDep>=2 - Constraint (cc)
                }
                while start < n && w.loops[start].i_tab as usize <= i_from_idx {
                    start += 1;
                }
            }
            for k in start..n {
                let l = &w.loops[k];
                if (from_tabs[l.i_tab as usize].jointype as i32 & (JT_OUTER | JT_CROSS)) != 0 {
                    break; // Constraint (bb)
                }
                if (l.prereq & m) != 0 // pWInfo depends on iFromIdx
                    && (l.mask_self & m_seen) == 0 // pWInfo not already a dependency
                    && (l.mask_self & m_self_join) == 0
                // Not a self-join
                {
                    if from_tabs[l.i_tab as usize].p_s_tab == fact_tab.p_s_tab {
                        m_self_join |= m;
                    } else {
                        n_dep += 1;
                        m_seen |= l.mask_self;
                    }
                }
            }
            if n_dep <= 2 {
                m <<= 1;
                continue; // Constraint (cc)
            }

            // If we reach this point, it means that pFactTab is a fact table with four or
            // more dimensions connected by inner joins. Proceed to make cost adjustments.
            w.b_star_used = true;

            // Compute the maximum cost of any WhereLoop for the fact table plus one epsilon
            let mut mx_run: LogEst = i16::MIN; // LOGEST_MIN
            for k in start..n {
                let l = &w.loops[k];
                if (l.i_tab as usize) < i_from_idx {
                    continue;
                }
                if (l.i_tab as usize) > i_from_idx {
                    break;
                }
                if l.r_run > mx_run {
                    mx_run = l.r_run;
                }
            }
            if mx_run < i16::MAX {
                mx_run += 1;
            }

            // Increase the cost of table scans for dimension tables to be slightly more than
            // the maximum cost of the fact table
            for k in start..n {
                let l = &mut w.loops[k];
                if (l.mask_self & m_seen) == 0 {
                    continue;
                }
                if l.n_l_term() != 0 {
                    continue;
                }
                if l.r_run < mx_run {
                    l.r_run = mx_run;
                }
            }
            m <<= 1;
        }
    }
    if w.b_star_used { 18 } else { 12 }
}

/// `whereLoopIsNoBetter(pCandidate, pBaseline)`: two WhereLoop objects, pCandidate and
/// pBaseline, are known to have the same cost. Look deep into each to see if pCandidate
/// appears to be slightly better than pBaseline. Return false if pCandidate is a better
/// choice than pBaseline. Return true if pCandidate is no better than pBaseline and so
/// pBaseline should be chosen.
///
/// At this point, the only test is that the smaller index is preferred.
fn where_loop_is_no_better(p: &Parse, candidate: &WhereLoop, baseline: &WhereLoop) -> bool {
    if (candidate.ws_flags & WHERE_INDEXED as u32) == 0 {
        return true;
    }
    if (baseline.ws_flags & WHERE_INDEXED as u32) == 0 {
        return true;
    }
    if p.idx(candidate.p_index).sz_idx_row < p.idx(baseline.p_index).sz_idx_row {
        return false;
    }
    true
}

/// `wherePathSolver(pWInfo, nRowEst)`: given the list of WhereLoop objects at pWInfo->pLoops,
/// this routine attempts to find the lowest cost path that visits each WhereLoop once. This
/// path is then loaded into the pWInfo->a[].pWLoop fields.
///
/// Assume that the total number of output rows that will need to be sorted will be nRowEst
/// (in the 10*log2 representation). Or, ignore sorting costs if nRowEst==0.
///
/// Return SQLITE_OK on success or SQLITE_NOMEM of a memory allocation error occurs.
fn where_path_solver(p: &mut Parse, w: &mut WhereInfo, n_row_est: LogEst) -> i32 {
    let n_loop = w.n_level as usize;
    let mut mx_i = 0usize; // Index of next entry to replace
    let mut mx_cost: LogEst = 0; // Maximum cost of a set of paths
    let mut mx_unsort: LogEst = 0; // Maximum unsorted cost of a set of path

    // TUNING: mxChoice is the maximum number of possible paths to preserve at each step.
    // Based on the number of loops in the FROM clause:
    //
    //     nLoop      mxChoice
    //     -----      --------
    //       1            1            // the most common case
    //       2            5
    //       3+        12 or 18        // see computeMxChoice()
    let mx_choice: usize = if n_loop <= 1 {
        1
    } else if n_loop == 2 {
        5
    } else if p.n_err != 0 {
        1
    } else {
        compute_mx_choice(p, w) as usize
    };

    // If nRowEst is zero and there is an ORDER BY clause, ignore it. In this case the purpose
    // of this call is to estimate the number of rows returned by the overall query. Once this
    // estimate has been obtained, the caller will invoke this function a second time, passing
    // the estimate as the nRowEst parameter.
    let n_order_by: i32 = if w.p_order_by.is_null() || n_row_est == 0 { 0 } else { p.l(w.p_order_by).n_expr() };

    // Allocate and initialize space for aTo, aFrom and aSortCost[]
    let mut a_to: Vec<WherePath> = vec![WherePath::default(); mx_choice];
    let mut a_from: Vec<WherePath> = vec![WherePath::default(); mx_choice];
    // If there is an ORDER BY clause and it is not being ignored, set up space for the
    // aSortCost[] array. Each element of the aSortCost array is either zero - meaning it has
    // not yet been initialized - or the cost of sorting nRowEst rows of data where the first
    // X terms of the ORDER BY clause are already in order, where X is the array index.
    let mut a_sort_cost: Vec<LogEst> = vec![0; n_order_by as usize];

    // Seed the search with a single WherePath containing zero WhereLoops.
    //
    // TUNING: Do not let the number of iterations go above 28. If the cost of computing an
    // automatic index is not paid back within the first 28 rows, then do not use the
    // automatic index.
    a_from[0].n_row = (p.n_query_loop as i32).min(48) as LogEst;
    let mut n_from = 1usize;
    if n_order_by != 0 {
        // If nLoop is zero, then there are no FROM terms in the query. Since in this case the
        // query may return a maximum of one row, the results are already in the requested
        // order. Set isOrdered to nOrderBy to indicate this. Or, if nLoop is greater than
        // zero, set isOrdered to -1 to indicate it is not yet known how many of the ORDER BY
        // terms are satisfied.
        a_from[0].is_ordered = if n_loop > 0 { -1 } else { n_order_by as i8 };
    }

    // Compute successively longer WherePaths using the previous generation of WherePaths as
    // the basis for the next. Keep track of the mxChoice best paths at each generation.
    for i_loop in 0..n_loop {
        let mut n_to = 0usize;
        for ii in 0..n_from {
            let from = a_from[ii].clone();
            for k in 0..w.loops.len() {
                let (prereq, mask_self, ws_flags, r_run, r_setup, l_n_out) = {
                    let l = &w.loops[k];
                    (l.prereq, l.mask_self, l.ws_flags, l.r_run, l.r_setup, l.n_out)
                };
                if (prereq & !from.mask_loop) != 0 {
                    continue;
                }
                if (mask_self & from.mask_loop) != 0 {
                    continue;
                }
                if (ws_flags & WHERE_AUTO_INDEX as u32) != 0 && from.n_row < 3 {
                    // Do not use an automatic index if the this loop is expected to run less
                    // than 1.25 times. It is tempting to also exclude automatic index usage
                    // on an outer loop, but sometimes an automatic index is useful in the
                    // outer loop of a correlated subquery.
                    continue;
                }

                // At this point, pWLoop is a candidate to be the next loop. Compute its cost
                let mut r_unsort: LogEst = r_run + from.n_row; // Unsorted cost of (pFrom+pWLoop)
                if r_setup != 0 {
                    r_unsort = sqlite3_log_est_add(r_setup, r_unsort);
                }
                r_unsort = sqlite3_log_est_add(r_unsort, from.r_unsort);
                let n_out: LogEst = from.n_row + l_n_out; // Rows visited by (pFrom+pWLoop)
                let mask_new = from.mask_loop | mask_self;
                let mut is_ordered = from.is_ordered;
                let mut rev_mask: Bitmask;
                if is_ordered < 0 {
                    rev_mask = 0;
                    let ob = w.p_order_by;
                    let flags = w.wctrl_flags;
                    is_ordered = where_path_satisfies_order_by(p, w, ob, &from.a_loop, flags, i_loop as u16, k, &mut rev_mask);
                } else {
                    rev_mask = from.rev_loop;
                }
                let r_cost: LogEst; // Cost of path (pFrom+pWLoop)
                if is_ordered >= 0 && (is_ordered as i32) < n_order_by {
                    if a_sort_cost[is_ordered as usize] == 0 {
                        a_sort_cost[is_ordered as usize] = where_sorting_cost(p, w, n_row_est, n_order_by, is_ordered as i32);
                    }
                    // TUNING: Add a small extra penalty (3) to sorting as an extra
                    // encouragement to the query planner to select a plan where the rows
                    // emerge in the correct order without any sorting required.
                    r_cost = sqlite3_log_est_add(r_unsort, a_sort_cost[is_ordered as usize]) + 3;
                } else {
                    r_cost = r_unsort;
                    r_unsort -= 2; // TUNING: Slight bias in favor of no-sort plans
                }

                // Check to see if pWLoop should be added to the set of mxChoice best-so-far
                // paths.
                //
                // First look for an existing path among best-so-far paths that covers the same
                // set of loops and has the same isOrdered setting as the current path
                // candidate.
                //
                // The term "((pTo->isOrdered^isOrdered)&0x80)==0" is equivalent to
                // (pTo->isOrdered==(-1))==(isOrdered==(-1))" for the range of legal values
                // for isOrdered: -1..64.
                let mut jj = 0usize;
                while jj < n_to {
                    let to = &a_to[jj];
                    if to.mask_loop == mask_new
                        && ((((to.is_ordered as u8) ^ (is_ordered as u8)) & 0x80) == 0 || i_loop == n_loop - 1)
                    {
                        break;
                    }
                    jj += 1;
                }
                if jj >= n_to {
                    // None of the existing best-so-far paths match the candidate.
                    if n_to >= mx_choice && (r_cost > mx_cost || (r_cost == mx_cost && r_unsort >= mx_unsort)) {
                        // The current candidate is no better than any of the mxChoice paths
                        // currently in the best-so-far buffer. So discard this candidate as
                        // not viable.
                        continue;
                    }
                    // If we reach this points it means that the new candidate path needs to
                    // be added to the set of best-so-far paths.
                    if n_to < mx_choice {
                        // Increase the size of the aTo set by one
                        jj = n_to;
                        n_to += 1;
                    } else {
                        // New path replaces the prior worst to keep count below mxChoice
                        jj = mx_i;
                    }
                } else {
                    // Control reaches here if best-so-far path pTo=aTo[jj] covers the same
                    // set of loops and has the same isOrdered setting as the candidate path.
                    // Check to see if the candidate should replace pTo or if candidate
                    // should be skipped.
                    //
                    // The conditional is an expanded vector comparison equivalent to:
                    //   (pTo->rCost,pTo->nRow,pTo->rUnsort) <= (rCost,nOut,rUnsort)
                    let to = &a_to[jj];
                    if (to.r_cost < r_cost)
                        || (to.r_cost == r_cost && to.n_row < n_out)
                        || (to.r_cost == r_cost && to.n_row == n_out && to.r_unsort < r_unsort)
                        || (to.r_cost == r_cost
                            && to.n_row == n_out
                            && to.r_unsort == r_unsort
                            && where_loop_is_no_better(p, &w.loops[k], &w.loops[to.a_loop[i_loop]]))
                    {
                        // Discard the candidate path from further consideration
                        continue;
                    }
                    // Control reaches here if the candidate path is better than the pTo path.
                    // Replace pTo with the candidate.
                }
                // pWLoop is a winner. Add it to the set of best so far
                {
                    let to = &mut a_to[jj];
                    to.mask_loop = from.mask_loop | mask_self;
                    to.rev_loop = rev_mask;
                    to.n_row = n_out;
                    to.r_cost = r_cost;
                    to.r_unsort = r_unsort;
                    to.is_ordered = is_ordered;
                    to.a_loop.clear();
                    to.a_loop.extend_from_slice(&from.a_loop[..i_loop]);
                    to.a_loop.push(k);
                }
                if n_to >= mx_choice {
                    mx_i = 0;
                    mx_cost = a_to[0].r_cost;
                    mx_unsort = a_to[0].n_row;
                    for j2 in 1..mx_choice {
                        let to = &a_to[j2];
                        if to.r_cost > mx_cost || (to.r_cost == mx_cost && to.r_unsort > mx_unsort) {
                            mx_cost = to.r_cost;
                            mx_unsort = to.r_unsort;
                            mx_i = j2;
                        }
                    }
                }
            }
        }

        // Swap the roles of aFrom and aTo for the next generation
        std::mem::swap(&mut a_from, &mut a_to);
        n_from = n_to;
    }

    if n_from == 0 {
        p.error_msg("no query solution");
        return SQLITE_ERROR;
    }

    // Only one path is available, which is the best path
    let from = a_from[0].clone();

    // Load the lowest cost path into pWInfo
    for i_loop in 0..n_loop {
        let k = from.a_loop[i_loop];
        let i_tab = w.loops[k].i_tab;
        let level = &mut w.a[i_loop];
        level.p_w_loop = k;
        level.i_from = i_tab;
        level.i_tab_cur = p.src(w.p_tab_list).a[i_tab as usize].i_cursor;
    }
    if (w.wctrl_flags as i32 & WHERE_WANT_DISTINCT) != 0
        && (w.wctrl_flags as i32 & WHERE_DISTINCTBY) == 0
        && w.e_distinct as i32 == WHERE_DISTINCT_NOOP
        && n_row_est != 0
    {
        let mut not_used: Bitmask = 0;
        let rs = w.p_result_set;
        let rc = where_path_satisfies_order_by(
            p,
            w,
            rs,
            &from.a_loop,
            WHERE_DISTINCTBY as u16,
            (n_loop - 1) as u16,
            from.a_loop[n_loop - 1],
            &mut not_used,
        );
        if rc as i32 == p.l(rs).n_expr() {
            w.e_distinct = WHERE_DISTINCT_ORDERED as u8;
        }
    }
    w.b_ordered_inner_loop = false;
    if w.p_order_by.nn() {
        let ob = w.p_order_by;
        let n_ob = p.l(ob).n_expr();
        w.n_ob_sat = from.is_ordered;
        if w.wctrl_flags as i32 & WHERE_DISTINCTBY != 0 {
            if from.is_ordered as i32 == n_ob {
                w.e_distinct = WHERE_DISTINCT_ORDERED as u8;
            }
        } else {
            w.rev_mask = from.rev_loop;
            if w.n_ob_sat <= 0 {
                w.n_ob_sat = 0;
                if n_loop > 0 {
                    let ws_flags = w.loops[from.a_loop[n_loop - 1]].ws_flags;
                    if (ws_flags & WHERE_ONEROW as u32) == 0
                        && (ws_flags & (WHERE_IPK | WHERE_COLUMN_IN) as u32) != (WHERE_IPK | WHERE_COLUMN_IN) as u32
                    {
                        let mut m: Bitmask = 0;
                        let rc = where_path_satisfies_order_by(
                            p,
                            w,
                            ob,
                            &from.a_loop,
                            WHERE_ORDERBY_LIMIT as u16,
                            (n_loop - 1) as u16,
                            from.a_loop[n_loop - 1],
                            &mut m,
                        );
                        if rc as i32 == n_ob {
                            w.b_ordered_inner_loop = true;
                            w.rev_mask = m;
                        }
                    }
                }
            } else if n_loop != 0 && w.n_ob_sat == 1 && (w.wctrl_flags as i32 & (WHERE_ORDERBY_MIN | WHERE_ORDERBY_MAX)) != 0 {
                w.b_ordered_inner_loop = true;
            }
        }
        if (w.wctrl_flags as i32 & WHERE_SORTBYGROUP) != 0 && w.n_ob_sat as i32 == n_ob && n_loop > 0 {
            let mut rev_mask: Bitmask = 0;
            let n_order =
                where_path_satisfies_order_by(p, w, ob, &from.a_loop, 0, (n_loop - 1) as u16, from.a_loop[n_loop - 1], &mut rev_mask);
            if n_order as i32 == n_ob {
                w.sorted = true;
                w.rev_mask = rev_mask;
            }
        }
    }

    w.n_row_out = from.n_row;
    if p.where_dump.is_some() {
        let mut names = Vec::new();
        for &k in &from.a_loop {
            let l = &w.loops[k];
            let name = if l.p_index.nn() && !p.idx(l.p_index).z_name.is_empty() { p.idx(l.p_index).z_name.clone() } else { "-".to_owned() };
            names.push(format!("{}:{}:{:x}", l.i_tab, name, l.ws_flags));
        }
        let line = format!(
            "PATH est={} nRow={} rCost={} rUnsort={} isOrdered={} rev={:x} nOBSat={} eDistinct={} loops=[{}]",
            n_row_est,
            from.n_row,
            from.r_cost,
            from.r_unsort,
            from.is_ordered,
            w.rev_mask,
            w.n_ob_sat,
            w.e_distinct,
            names.join(",")
        );
        if let Some(d) = p.where_dump.as_mut() {
            d.push(line);
        }
    }
    SQLITE_OK
}

/// `whereInterstageHeuristic(pWInfo)`: this routine implements a heuristic designed to
/// improve query planning. This routine is called in between the first and second call to
/// wherePathSolver(). Hence the name "Interstage" "Heuristic".
///
/// The first call to wherePathSolver() (hereafter just "solver()") computes the best path
/// without regard to the order of the outputs. The second call to the solver() builds upon
/// the first call to try to find an alternative path that satisfies the ORDER BY clause.
///
/// This routine looks at the results of the first solver() run, and for every FROM clause
/// term in the resulting query plan that uses an equality constraint against an index,
/// disable other WhereLoops for that same FROM clause term that would try to do a full-table
/// scan. This prevents an index search from being converted into a full-table scan in order
/// to satisfy an ORDER BY clause, since even though we might get slightly better performance
/// using the full-scan without sorting if the output size estimates are very precise, we
/// might also get severe performance degradation using the full-scan if the output size
/// estimate is too large. It is better to err on the side of caution.
///
/// Except, if the first solver() call generated a full-table scan in an outer loop then
/// stop this analysis at the first full-scan, since the second solver() run might try to
/// swap that full-scan for another in order to get the output into the correct order. In
/// other words, we allow a rewrite like this:
///
/// ```text
///     First Solver()                      Second Solver()
///       |-- SCAN t1                         |-- SCAN t2
///       |-- SEARCH t2                       `-- SEARCH t1
///       `-- SORT USING B-TREE
/// ```
///
/// The purpose of this routine is to disallow rewrites such as:
///
/// ```text
///     First Solver()                      Second Solver()
///       |-- SEARCH t1                       |-- SCAN t2     <--- bad!
///       |-- SEARCH t2                       `-- SEARCH t1
///       `-- SORT USING B-TREE
/// ```
///
/// See test cases in test/whereN.test for the real-world query that originally provoked
/// this heuristic.
fn where_interstage_heuristic(w: &mut WhereInfo) {
    for i in 0..w.n_level as usize {
        let k = w.a[i].p_w_loop;
        if k == usize::MAX || k >= w.loops.len() {
            break;
        }
        let (ws_flags, i_tab) = (w.loops[k].ws_flags, w.loops[k].i_tab);
        if (ws_flags & WHERE_VIRTUALTABLE as u32) != 0 {
            // Treat a vtab scan as similar to a full-table scan
            break;
        }
        if (ws_flags & (WHERE_COLUMN_EQ | WHERE_COLUMN_NULL | WHERE_COLUMN_IN) as u32) != 0 {
            for l in w.loops.iter_mut() {
                if l.i_tab != i_tab {
                    continue;
                }
                if (l.ws_flags & (WHERE_CONSTRAINT | WHERE_AUTO_INDEX) as u32) != 0 {
                    // Auto-index and index-constrained loops allowed to remain
                    continue;
                }
                l.prereq = !0; // Prevent 2nd solver() from using this one
            }
        } else {
            break;
        }
    }
}

/// `whereShortCut(pBuilder)`: most queries use only a single table (they are not joins) and
/// have simple == constraints against indexed fields. This routine attempts to plan those
/// simple cases using much less ceremony than the general-purpose query planner, and thereby
/// yield faster sqlite3_prepare() times for the common case.
///
/// Return non-zero on success, if this query can be handled by this no-frills query planner.
/// Return zero if this query needs the general-purpose query planner.
fn where_short_cut(p: &mut Parse, w: &mut WhereInfo, b: &mut WhereLoopBuilder) -> bool {
    if w.wctrl_flags as i32 & WHERE_OR_SUBCLAUSE != 0 {
        return false;
    }
    let item = p.src(w.p_tab_list).a[0].clone();
    let tab = item.p_s_tab;
    if p.tab(tab).is_virtual() {
        return false;
    }
    if item.is_indexed_by || item.not_indexed {
        return false;
    }
    let i_cur = item.i_cursor;
    let wc: WcId = 0;
    let mut l = std::mem::take(&mut b.p_new);
    l.ws_flags = 0;
    l.n_skip = 0;
    let mut scan = WhereScan::default();
    let mut t = where_scan_init(p, w, &mut scan, wc, i_cur, -1, (WO_EQ | WO_IS) as u32, IndexId::NULL);
    while let Some(tr) = t {
        if w.term(tr).prereq_right == 0 {
            break;
        }
        t = where_scan_next(p, w, &mut scan);
    }
    if let Some(tr) = t {
        l.ws_flags = (WHERE_COLUMN_EQ | WHERE_IPK | WHERE_ONEROW) as u32;
        l.a_l_term = vec![Some(tr)];
        l.n_eq = 1;
        // TUNING: Cost of a rowid lookup is 10
        l.r_run = 33; // 33==sqlite3LogEst(10)
    } else {
        let idxs = p.tab(tab).p_index.clone();
        for idx in idxs {
            {
                let ix = p.idx(idx);
                if !ix.is_unique() || ix.p_part_idx_where.nn() || ix.n_key_col > 3 {
                    continue;
                }
            }
            let op_mask = if p.idx(idx).uniq_not_null { WO_EQ | WO_IS } else { WO_EQ };
            let n_key = p.idx(idx).n_key_col as usize;
            let mut terms: Vec<Option<TermRef>> = Vec::new();
            let mut j = 0;
            while j < n_key {
                let mut t = where_scan_init(p, w, &mut scan, wc, i_cur, j as i32, op_mask as u32, idx);
                while let Some(tr) = t {
                    if w.term(tr).prereq_right == 0 {
                        break;
                    }
                    t = where_scan_next(p, w, &mut scan);
                }
                let Some(tr) = t else { break };
                terms.push(Some(tr));
                j += 1;
            }
            if j != n_key {
                continue;
            }
            l.ws_flags = (WHERE_COLUMN_EQ | WHERE_ONEROW | WHERE_INDEXED) as u32;
            if p.idx(idx).is_covering || (item.col_used & p.idx(idx).col_not_idxed) == 0 {
                l.ws_flags |= WHERE_IDX_ONLY as u32;
            }
            l.a_l_term = terms;
            l.n_eq = j as u16;
            l.p_index = idx;
            // TUNING: Cost of a unique index lookup is 15
            l.r_run = 39; // 39==sqlite3LogEst(15)
            break;
        }
    }
    if l.ws_flags != 0 {
        l.n_out = 1;
        l.mask_self = 1; // sqlite3WhereGetMask(&pWInfo->sMaskSet, iCur)
        if scan.i_equiv > 1 {
            l.ws_flags |= WHERE_TRANSCONS as u32;
        }
        w.loops.push(l);
        w.a[0].p_w_loop = w.loops.len() - 1;
        w.a[0].i_tab_cur = i_cur;
        w.n_row_out = 1;
        if w.p_order_by.nn() {
            w.n_ob_sat = p.l(w.p_order_by).n_expr() as i8;
        }
        if w.wctrl_flags as i32 & WHERE_WANT_DISTINCT != 0 {
            w.e_distinct = WHERE_DISTINCT_UNIQUE as u8;
        }
        return true;
    }
    b.p_new = l;
    false
}

/// `exprNodeIsDeterministic` / `exprIsDeterministic(p)`: return true if the expression
/// contains no non-deterministic SQL functions. Do not consider non-deterministic SQL
/// functions that are part of sub-select statements.
fn expr_is_deterministic(p: &mut Parse, e: ExprId) -> bool {
    fn cb(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
        if p.e(e).op == TK_FUNCTION && !p.e(e).has(EP_ConstFunc) {
            w.e_code = 0;
            return WRC_Abort;
        }
        WRC_Continue
    }
    let mut w = Walker { e_code: 1, x_expr_callback: Some(cb), x_select_callback: Some(sqlite3_select_walk_fail), ..Walker::default() };
    sqlite3_walk_expr(p, &mut w, e);
    w.e_code != 0
}

/// `whereOmitNoopJoin(pWInfo, notReady)`: check to see if there are any tables in the join
/// that can be omitted. A table can be omitted from the join if all of the following are
/// true:
///
///   1) The query must not be an aggregate.
///   2) The table must be the RHS of a LEFT JOIN.
///   3) Either the query must be DISTINCT, or else the ON or USING clause must contain a
///      constraint that limits the scan of the table to at most a single row.
///   4) The table must not be referenced by any part of the query apart from its own USING
///      or ON clause.
///   5) The table must not have an inner-join ON or USING clause if there is a RIGHT JOIN
///      anywhere in the query. Otherwise the ON or USING clause might be moved from the WHERE
///      clause to elsewhere in the query, which leads to wrong answers.
///   6) The ORDER BY clause has 63 or fewer terms
///   7) The omit-noop-join optimization is enabled.
///
/// Items (1), (6), and (7) are checked by the caller.
///
/// For example, given:
///
///     CREATE TABLE t1(ipk INTEGER PRIMARY KEY, v1);
///     CREATE TABLE t2(ipk INTEGER PRIMARY KEY, v2);
///     CREATE TABLE t3(ipk INTEGER PRIMARY KEY, v3);
///
/// then table t2 can be omitted from the following:
///
///     SELECT v1, v3 FROM t1
///       LEFT JOIN t2 ON (t1.ipk=t2.ipk)
///       LEFT JOIN t3 ON (t1.ipk=t3.ipk)
///
/// or from:
///
///     SELECT DISTINCT v1, v3 FROM t1
///       LEFT JOIN t2
///       LEFT JOIN t3 ON (t1.ipk=t3.ipk)
fn where_omit_noop_join(p: &Parse, w: &mut WhereInfo, mut not_ready: Bitmask) -> Bitmask {
    // These two preconditions checked by the caller combine to guarantee condition (1) of
    // the header comment on this routine.
    let mut tab_used = sqlite3_where_expr_list_usage(p, &mut w.s_mask_set, w.p_result_set);
    if w.p_order_by.nn() {
        tab_used |= sqlite3_where_expr_list_usage(p, &mut w.s_mask_set, w.p_order_by);
    }
    let has_right_join = (p.src(w.p_tab_list).a[0].jointype as i32 & JT_LTORJ) != 0;
    let mut i = w.n_level as i32 - 1;
    while i >= 1 {
        let iu = i as usize;
        let k = w.a[iu].p_w_loop;
        let (l_i_tab, l_ws_flags, l_mask_self) = (w.loops[k].i_tab, w.loops[k].ws_flags, w.loops[k].mask_self);
        let (jointype, i_cursor) = {
            let it = &p.src(w.p_tab_list).a[l_i_tab as usize];
            (it.jointype as i32, it.i_cursor)
        };
        i -= 1;
        if (jointype & (JT_LEFT | JT_RIGHT)) != JT_LEFT {
            continue;
        }
        if (w.wctrl_flags as i32 & WHERE_WANT_DISTINCT) == 0 && (l_ws_flags & WHERE_ONEROW as u32) == 0 {
            continue;
        }
        if (tab_used & l_mask_self) != 0 {
            continue;
        }
        let n_term = w.a_wc[0].a.len();
        let mut t = 0;
        while t < n_term {
            let (pa, e) = {
                let term = &w.a_wc[0].a[t];
                (term.prereq_all, term.p_expr)
            };
            if (pa & l_mask_self) != 0 && (!p.e(e).has(EP_OuterON) || p.e(e).w != i_cursor) {
                break;
            }
            if has_right_join && p.e(e).has(EP_InnerON) && p.e(e).w == i_cursor {
                break; // restriction (5)
            }
            t += 1;
        }
        if t < n_term {
            continue;
        }
        let m1: Bitmask = (1u64 << iu) - 1;
        w.rev_mask = (m1 & w.rev_mask) | ((w.rev_mask >> 1) & !m1);
        not_ready &= !l_mask_self;
        for term in w.a_wc[0].a.iter_mut() {
            if (term.prereq_all & l_mask_self) != 0 {
                term.wt_flags |= TERM_CODED;
                term.prereq_all = 0;
            }
        }
        w.a.remove(iu);
        w.a.push(WhereLevel::default());
        w.n_level -= 1;
    }
    not_ready
}

/// `whereCheckIfBloomFilterIsUseful(pWInfo)`: check to see if there are any SEARCH loops
/// that might benefit from using a Bloom filter. Consider a Bloom filter if:
///
///   (1)  The SEARCH happens more than N times where N is the number of rows in the table
///        that is being considered for the Bloom filter.
///   (2)  Some searches are expected to find zero rows. (This is determined by the
///        WHERE_SELFCULL flag on the term.)
///   (3)  Bloom-filter processing is not disabled. (Checked by the caller.)
///   (4)  The size of the table being searched is known by ANALYZE.
///
/// This block of code merely checks to see if a Bloom filter would be appropriate, and if so
/// sets the WHERE_BLOOMFILTER flag on the WhereLoop. The WhereLoop.prereq field is cleared
/// (to 0) on any loop for which a Bloom filter is to be attached... no: it is the code
/// generator that builds the filter at the right moment.
fn where_check_if_bloom_filter_is_useful(p: &Parse, w: &mut WhereInfo) {
    let mut n_search: LogEst = 0;
    for i in 0..w.n_level as usize {
        let k = w.a[i].p_w_loop;
        let req_flags = (WHERE_SELFCULL | WHERE_COLUMN_EQ) as u32;
        let tab = p.src(w.p_tab_list).a[w.loops[k].i_tab as usize].p_s_tab;
        if (p.tab(tab).tab_flags & TF_HasStat1) == 0 {
            break;
        }
        // pTab->tabFlags |= TF_MaybeReanalyze;
        if i >= 1
            && (w.loops[k].ws_flags & req_flags) == req_flags
            && sqlite3_where_loop_bloomable(p, &w.loops[k])
            && n_search > p.tab(tab).n_row_log_est
        {
            w.loops[k].ws_flags |= WHERE_BLOOMFILTER as u32;
            w.loops[k].ws_flags &= !(WHERE_IDX_ONLY as u32);
        }
        n_search += w.loops[k].n_out;
    }
}

/// `sqlite3IndexBloomable(pIdx, N)` (build.c): return true if the first N columns of the
/// index all use the BINARY collating sequence.
pub fn sqlite3_index_bloomable(p: &Parse, idx: IndexId, n: i32) -> bool {
    let ix = p.idx(idx);
    for i in 0..n as usize {
        if !ix.az_coll[i].eq_ignore_ascii_case("BINARY") {
            return false;
        }
    }
    true
}

/// `whereAddIndexedExpr(pParse, pIdx, iIdxCur, pTabItem)`: the index pIdx is used by a query
/// and contains one or more expressions. In other words pIdx is an index on an expression.
/// iIdxCur is the cursor number for the index and pTabItem is the FROM clause term of the
/// table. Add entries to the Parse.pIdxEpr list so that the code generator can read the
/// value of an indexed expression from the index.
fn where_add_indexed_expr(p: &mut Parse, idx: IndexId, i_idx_cur: i32, tab_list: SrcListId, i_item: usize) {
    let tab = p.idx(idx).p_table;
    let (i_cursor, jointype) = {
        let it = &p.src(tab_list).a[i_item];
        (it.i_cursor, it.jointype as i32)
    };
    for i in 0..p.idx(idx).n_column as usize {
        let j = p.idx(idx).ai_column[i] as i32;
        let e;
        if j == XN_EXPR {
            e = p.l(p.idx(idx).a_col_expr).a[i].p_expr;
        } else if j >= 0 && (p.tab(tab).a_col[j as usize].col_flags & COLFLAG_VIRTUAL as u16) != 0 {
            e = crate::build_tab::sqlite3_column_expr(p, tab, j as usize);
        } else {
            continue;
        }
        if sqlite3_expr_is_constant_no_parse(p, e) != 0 {
            continue;
        }
        let dup = sqlite3_expr_dup(p, e, 0);
        let aff = sqlite3_index_affinity_str(p, idx)[i];
        p.p_idx_epr.push(IndexedExpr {
            p_expr: dup,
            i_data_cur: i_cursor,
            i_idx_cur,
            i_idx_col: i as i32,
            b_maybe_null_row: (jointype & (JT_LEFT | JT_LTORJ | JT_RIGHT)) != 0,
            aff,
        });
    }
}

/// `whereReverseScanOrder(pWInfo)`: set the reverse-scan order mask to one for all tables in
/// the query with the exception of MATERIALIZED common table expressions that have their own
/// internal ORDER BY clauses.
///
/// This implements the PRAGMA reverse_unordered_selects=ON setting. (Also
/// SQLITE_DBCONFIG_REVERSE_SCANORDER).
fn where_reverse_scan_order(p: &Parse, w: &mut WhereInfo) {
    for ii in 0..p.src(w.p_tab_list).a.len() {
        let it = &p.src(w.p_tab_list).a[ii];
        let keep = it.is_cte && p.a_cte_use[it.p_cte_use as usize - 1].e_m10d == M10d_Yes as u8 && it.is_subquery && {
            let sub = it.p_subq.as_ref().expect("subquery").p_select;
            p.s(sub).p_order_by.nn()
        };
        if !keep {
            w.rev_mask |= 1u64 << ii;
        }
    }
}

/// The WHERE-term dump (verification aid): every term of a WhereClause after
/// `sqlite3WhereExprAnalyze()`.
fn dump_where_clause(p: &mut Parse, w: &WhereInfo, wc: WcId, depth: usize) {
    if p.where_dump.is_none() {
        return;
    }
    let mut lines = Vec::new();
    fn rec(p: &Parse, w: &WhereInfo, wc: WcId, depth: usize, lines: &mut Vec<String>) {
        for (i, t) in w.a_wc[wc].a.iter().enumerate() {
            lines.push(format!(
                "TERM {}{} top={} op={:x} cur={} col={} prR={:x} prA={:x} fl={:x} prob={} parent={} nChild={} iField={}",
                "  ".repeat(depth),
                i,
                p.e(t.p_expr).op,
                t.e_operator,
                t.left_cursor,
                if t.p_or_info.is_some() || t.p_and_info.is_some() { 0 } else { t.left_column },
                t.prereq_right,
                t.prereq_all,
                t.wt_flags,
                t.truth_prob,
                t.i_parent,
                t.n_child,
                if t.p_or_info.is_some() || t.p_and_info.is_some() { 0 } else { t.i_field },
            ));
            if t.e_operator == WO_AUX {
                lines.push(format!("TERMAUX {}{} mop={}", "  ".repeat(depth), i, t.e_match_op));
            }
            if let Some(oi) = t.p_or_info {
                lines.push(format!("ORINFO {}indexable={:x}", "  ".repeat(depth + 1), oi.indexable));
                rec(p, w, oi.wc, depth + 1, lines);
            } else if let Some(aw) = t.p_and_info {
                rec(p, w, aw, depth + 1, lines);
            }
        }
    }
    rec(p, w, wc, depth, &mut lines);
    if let Some(d) = p.where_dump.as_mut() {
        d.push(format!("WC nTerm={} nBase={}", w.a_wc[wc].a.len(), w.a_wc[wc].n_base));
        d.extend(lines);
    }
}

/// `sqlite3WhereBegin(pParse, pTabList, pWhere, pOrderBy, pResultSet, pSelect, wctrlFlags,
/// iAuxArg)`: generate the beginning of the loop used for WHERE clause processing. The
/// return value is a pointer to an opaque structure that contains information needed to
/// terminate the loop. Later, the calling routine should invoke sqlite3WhereEnd() with the
/// return value of this function in order to complete the WHERE clause processing.
///
/// If an error occurs, this routine returns NULL.
///
/// The basic idea is to do a nested loop, one loop for each table in the FROM clause of a
/// select. (INSERT and UPDATE statements are the same as a SELECT with only a single table
/// in the FROM clause.) For example, if the SQL is this:
///
///       SELECT * FROM t1, t2, t3 WHERE ...;
///
/// Then the code generated is conceptually like the following:
///
///      foreach row1 in t1 do       \    Code generated
///        foreach row2 in t2 do      |-- by sqlite3WhereBegin()
///          foreach row3 in t3 do   /
///            ...
///          end                     \    Code generated
///        end                        |-- by sqlite3WhereEnd()
///      end                         /
///
/// Note that the loops might not be nested in the order in which they appear in the FROM
/// clause if a different order is better able to make use of indices. Note also that when
/// the IN operator appears in the WHERE clause, it might result in additional nested loops
/// for scanning through all values on the right-hand side of the IN.
///
/// There are Btree cursors associated with each table. t1 uses cursor number
/// pTabList->a[0].iCursor. t2 uses the cursor pTabList->a[1].iCursor. And so forth. This
/// routine generates code to open those VDBE cursors and sqlite3WhereEnd() generates the
/// code to close them.
///
/// The code that sqlite3WhereBegin() generates leaves the cursors named in pTabList pointing
/// at their appropriate entries. The [...] code within the loop can use those cursors.
///
/// ORDER BY CLAUSE PROCESSING
///
/// pOrderBy is a pointer to the ORDER BY clause (or the GROUP BY clause if the
/// WHERE_GROUPBY flag is set in wctrlFlags) of a SELECT statement if there is one. If there
/// is no ORDER BY clause or if this routine is called from an UPDATE or DELETE statement,
/// then pOrderBy is NULL.
///
/// The iAuxArg parameter is the index of the first index cursor that may be used with the
/// ONEPASS optimization (WHERE_ONEPASS_DESIRED), the index cursor of an OR sub-clause
/// (WHERE_OR_SUBCLAUSE) or the LIMIT of the query (WHERE_USE_LIMIT).
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_where_begin(
    p: &mut Parse,
    tab_list: SrcListId,
    p_where: ExprId,
    order_by: ExprListId,
    result_set: ExprListId,
    select: SelectId,
    wctrl_flags: u16,
    i_aux_arg: i32,
) -> Option<WhereInfo> {
    let mut order_by = order_by;
    let mut wctrl_flags = wctrl_flags;
    let mut b_fordelete: u16 = 0; // OPFLAG_FORDELETE or zero, as appropriate

    // Variable initialization
    let mut s_wlb = WhereLoopBuilder::default();

    // An ORDER/GROUP BY clause of more than 63 terms cannot be optimized
    if order_by.nn() && p.l(order_by).n_expr() >= BMS {
        order_by = ExprListId::NULL;
        wctrl_flags &= !(WHERE_WANT_DISTINCT as u16);
        wctrl_flags |= WHERE_KEEP_ALL_JOINS as u16; // Disable omit-noop-join opt
    }

    // The number of tables in the FROM clause is limited by the number of bits in a Bitmask
    if p.src(tab_list).n_src() > BMS {
        p.error_msg(format!("at most {BMS} tables in a join"));
        return None;
    }

    // This function normally generates a nested loop for all tables in pTabList. But if the
    // WHERE_OR_SUBCLAUSE flag is set, then we should only generate code for the first table,
    // the one at index 0. This is used for the "OR" optimization.
    let mut n_tab_list: usize = if wctrl_flags as i32 & WHERE_OR_SUBCLAUSE != 0 { 1 } else { p.src(tab_list).a.len() };

    // Allocate and initialize the WhereInfo structure that will become the return value.
    let mut w = WhereInfo {
        p_tab_list: tab_list,
        p_order_by: order_by,
        p_result_set: result_set,
        ai_cur_one_pass: [-1, -1],
        n_level: n_tab_list as u8,
        wctrl_flags,
        i_limit: i_aux_arg as LogEst,
        saved_n_query_loop: p.n_query_loop as i32,
        p_select: select,
        // (pWLoop is NULL until wherePathSolver() succeeds)
        a: vec![WhereLevel { p_w_loop: usize::MAX, ..WhereLevel::default() }; n_tab_list],
        a_wc: vec![WhereClause::default()],
        ..WhereInfo::default()
    };
    w.i_break = p.v.make_label();
    w.i_continue = w.i_break;
    // Initialize ix[0] to a value that can never be a valid cursor number, to avoid an
    // initial test for pMaskSet->n==0 in sqlite3WhereGetMask()
    w.s_mask_set.n = 0;
    w.s_mask_set.ix[0] = -99;
    s_wlb.p_wc = 0;

    // Split the WHERE clause into separate subexpressions where each subexpression is
    // separated by an AND operator.
    sqlite3_where_split(p, &mut w, 0, p_where, TK_AND);

    // Special case: No FROM clause
    if n_tab_list == 0 {
        if order_by.nn() {
            w.n_ob_sat = p.l(order_by).n_expr() as i8;
        }
        if (wctrl_flags as i32 & WHERE_WANT_DISTINCT) != 0 && p.db.optimization_enabled(SQLITE_DistinctOpt) {
            w.e_distinct = WHERE_DISTINCT_UNIQUE as u8;
        }
        if w.p_select.nn() && (p.s(w.p_select).sel_flags & SF_MultiValue) == 0 {
            sqlite3_vdbe_explain(p, false, "SCAN CONSTANT ROW".to_owned());
        }
    } else {
        // Assign a bit from the bitmask to every term in the FROM clause.
        //
        // The N-th term of the FROM clause is assigned a bitmask of 1<<N.
        //
        // The rule of the previous sentence ensures that if X is the bitmask for a table
        // and Y>X is the bitmask for another table, then (X-1) is the bitmask for all
        // prior tables... Note that bitmasks are created for all pTabList->nSrc tables in
        // pTabList, not just the first nTabList tables. nTabList is normally equal to
        // pTabList->nSrc but might be shortened to 1 if the WHERE_OR_SUBCLAUSE flag is set.
        for ii in 0..p.src(tab_list).a.len() {
            let c = p.src(tab_list).a[ii].i_cursor;
            create_mask(&mut w.s_mask_set, c);
            sqlite3_where_tab_func_args(p, &mut w, 0, tab_list, ii);
        }
    }

    // Analyze all of the subexpressions.
    sqlite3_where_expr_analyze(p, &mut w, 0);
    if w.p_select.nn() && p.s(w.p_select).p_limit.nn() {
        let sel = w.p_select;
        crate::whereexpr::sqlite3_where_add_limit(p, &mut w, 0, sel);
    }
    if p.n_err != 0 {
        p.n_query_loop = w.saved_n_query_loop as LogEst;
        return None;
    }
    dump_where_clause(p, &w, 0, 0);

    // The False-WHERE-Term-Bypass optimization:
    //
    // If there are WHERE terms that are false, then no rows will be output, so skip over
    // all of the code generated here.
    //
    // Conditions:
    //
    //   (1)  The WHERE term must not refer to any tables in the join.
    //   (2)  The term must not come from an ON clause on the right-hand side of a LEFT or
    //        FULL JOIN.
    //   (3)  The term must not come from an ON clause, or there must be no RIGHT or FULL
    //        OUTER joins in pTabList.
    //   (4)  If the expression contains non-deterministic functions that are not within a
    //        sub-select. This is not required for correctness but rather to preserves
    //        SQLite's legacy behaviour in the following two cases:
    //
    //   WHERE random()>0;                           -- eval random() once per row
    //   WHERE (SELECT random())>0;                  -- eval random() just once overall
    //
    // Note that the Where term need not be a constant in order for this optimization to
    // apply, though it does need to be constant relative to the current subquery (condition
    // 1). The term might include variables from outer queries so that the value of the term
    // changes from one invocation of the current subquery to the next.
    let n_base = w.a_wc[0].n_base as usize;
    for ii in 0..n_base {
        let (wt_flags, x, prereq_all) = {
            let t = &w.a_wc[0].a[ii];
            (t.wt_flags, t.p_expr, t.prereq_all)
        };
        if wt_flags & TERM_VIRTUAL != 0 {
            continue;
        }
        if prereq_all == 0 // Conditions (1) and (2)
            && (n_tab_list == 0 || expr_is_deterministic(p, x)) // Condition (4)
            && !(p.e(x).has(EP_InnerON) // Condition (3)
                && (p.src(tab_list).a[0].jointype as i32 & JT_LTORJ) != 0)
        {
            sqlite3_expr_if_false(p, x, w.i_break, SQLITE_JUMPIFNULL);
            w.a_wc[0].a[ii].wt_flags |= TERM_CODED;
        }
    }

    if wctrl_flags as i32 & WHERE_WANT_DISTINCT != 0 {
        if p.db.optimization_disabled(SQLITE_DistinctOpt) {
            // Disable the DISTINCT optimization if SQLITE_DistinctOpt is set via
            // sqlite3_test_ctrl(SQLITE_TESTCTRL_OPTIMIZATIONS,...)
            wctrl_flags &= !(WHERE_WANT_DISTINCT as u16);
            w.wctrl_flags &= !(WHERE_WANT_DISTINCT as u16);
        } else if is_distinct_redundant(p, &w, tab_list, 0, result_set) {
            // The DISTINCT marking is pointless. Ignore it.
            w.e_distinct = WHERE_DISTINCT_UNIQUE as u8;
        } else if order_by.is_null() {
            // Try to ORDER BY the result set to make distinct processing easier
            w.wctrl_flags |= WHERE_DISTINCTBY as u16;
            w.p_order_by = result_set;
        }
    }

    // Construct the WhereLoop objects
    if n_tab_list != 1 || !where_short_cut(p, &mut w, &mut s_wlb) {
        let rc = where_loop_add_all(p, &mut w, &mut s_wlb);
        if rc != 0 || p.unsupported.is_some() {
            p.n_query_loop = w.saved_n_query_loop as LogEst;
            return None;
        }
        // (SQLITE_BLDF2_2NDPASS: STAT4 only)
        where_path_solver(p, &mut w, 0);
        if w.p_order_by.nn() {
            where_interstage_heuristic(&mut w);
            let est = if w.n_row_out < 0 { 1 } else { w.n_row_out + 1 };
            where_path_solver(p, &mut w, est);
        }
        // TUNING: Assume that a DISTINCT clause on a subquery reduces the output size by a
        // factor of 8 (LogEst -30). Search for tag-20250414a to see other cases.
        if (w.wctrl_flags as i32 & WHERE_WANT_DISTINCT) != 0 {
            w.n_row_out -= 30;
        }
    }
    if w.p_order_by.is_null() && (p.db.flags & SQLITE_ReverseOrder) != 0 {
        where_reverse_scan_order(p, &mut w);
    }
    if p.n_err != 0 {
        p.n_query_loop = w.saved_n_query_loop as LogEst;
        return None;
    }

    // Attempt to omit tables from a join that do not affect the result. See the comment on
    // whereOmitNoopJoin() for further information.
    //
    // This query optimization is factored out into a separate "no-inline" procedure to keep
    // the sqlite3WhereBegin() procedure from becoming too large and too slow.
    let mut not_ready: Bitmask = !0;
    if w.n_level >= 2 // Must be a join, or this opt8n is pointless
        && result_set.nn() // Condition (1)
        && 0 == (wctrl_flags as i32 & (WHERE_AGG_DISTINCT | WHERE_KEEP_ALL_JOINS)) // (1),(6)
        && p.db.optimization_enabled(SQLITE_OmitNoopJoin)
    // (7)
    {
        not_ready = where_omit_noop_join(p, &mut w, not_ready);
        n_tab_list = w.n_level as usize;
    }

    // Check to see if there are any SEARCH loops that might benefit from using a Bloom
    // filter.
    if w.n_level >= 2 && p.db.optimization_enabled(SQLITE_BloomFilter) {
        where_check_if_bloom_filter_is_useful(p, &mut w);
    }

    p.n_query_loop += w.n_row_out;

    // If the caller is an UPDATE or DELETE statement that is requesting to use a one-pass
    // algorithm, determine if this is appropriate.
    //
    // A one-pass approach can be used if the caller has requested one and either (a) the scan
    // visits at most one row or (b) each of the following are true:
    //
    //   * the caller has indicated that a one-pass approach can be used with multiple rows
    //     (by setting WHERE_ONEPASS_MULTIROW), and
    //   * the table is not a virtual table, and
    //   * either the scan does not use the OR optimization or the caller is a DELETE
    //     operation (WHERE_DUPLICATES_OK is only specified for DELETE).
    //
    // The last qualification is because an UPDATE statement uses WhereInfo.aiCurOnePass[1]
    // to determine whether or not it really can use a one-pass approach, and this is not set
    // accurately for scans that use the OR optimization.
    if (wctrl_flags as i32 & WHERE_ONEPASS_DESIRED) != 0 {
        let k = w.a[0].p_w_loop;
        let ws_flags = w.loops[k].ws_flags;
        let b_onerow = (ws_flags & WHERE_ONEROW as u32) != 0;
        let tab0 = p.src(tab_list).a[0].p_s_tab;
        if b_onerow
            || (0 != (wctrl_flags as i32 & WHERE_ONEPASS_MULTIROW)
                && !p.tab(tab0).is_virtual()
                && (0 == (ws_flags & WHERE_MULTI_OR as u32) || (wctrl_flags as i32 & WHERE_DUPLICATES_OK) != 0)
                && p.db.optimization_enabled(SQLITE_OnePass))
        {
            w.e_one_pass = if b_onerow { ONEPASS_SINGLE as u8 } else { ONEPASS_MULTI as u8 };
            if p.tab(tab0).has_rowid() && (ws_flags & WHERE_IDX_ONLY as u32) != 0 {
                if wctrl_flags as i32 & WHERE_ONEPASS_MULTIROW != 0 {
                    b_fordelete = OPFLAG_FORDELETE as u16;
                }
                w.loops[k].ws_flags = ws_flags & !(WHERE_IDX_ONLY as u32);
            }
        }
    }

    // Open all tables in the pTabList and any indices selected for searching those tables.
    for ii in 0..n_tab_list {
        let i_from = w.a[ii].i_from as usize;
        let (tab, jointype, i_cursor, col_used) = {
            let it = &p.src(tab_list).a[i_from];
            (it.p_s_tab, it.jointype as i32, it.i_cursor, it.col_used)
        };
        let i_db = p.tab(tab).i_db; // sqlite3SchemaToIndex(db, pTab->pSchema)
        let k = w.a[ii].p_w_loop;
        w.a[ii].addr_brk = p.v.make_label();
        if ii == 0 || (jointype & JT_LEFT) != 0 {
            w.a[ii].addr_halt = w.a[ii].addr_brk;
        } else if w.a[ii - 1].p_rj.is_some() {
            w.a[ii].addr_halt = w.a[ii - 1].addr_brk;
        } else {
            w.a[ii].addr_halt = w.a[ii - 1].addr_halt;
        }
        let l_ws_flags = w.loops[k].ws_flags;
        if (p.tab(tab).tab_flags & TF_Ephemeral) != 0 || p.tab(tab).is_view() {
            // Do nothing
        } else if (l_ws_flags & WHERE_VIRTUALTABLE as u32) != 0 {
            let vt = crate::vtab::sqlite3_get_vtable(p, tab).cloned().unwrap_or_default();
            p.v.add_op4(OP_VOpen, i_cursor, 0, 0, P4::Vtab(Arc::new(sqliters_vdbe::vdbeaux::P4Vtab { id: vt.id, name: p.tab(tab).z_name.clone() })));
        } else if p.tab(tab).is_virtual() {
            // noop
        } else if ((l_ws_flags & WHERE_IDX_ONLY as u32) == 0 && (wctrl_flags as i32 & WHERE_OR_SUBCLAUSE) == 0)
            || (jointype & (JT_LTORJ | JT_RIGHT)) != 0
        {
            let mut op = OP_OpenRead;
            if w.e_one_pass as i32 != ONEPASS_OFF {
                op = OP_OpenWrite;
                w.ai_cur_one_pass[0] = i_cursor;
            }
            sqlite3_open_table(p, i_cursor, i_db, tab, op);
            if w.e_one_pass as i32 == ONEPASS_OFF
                && (p.tab(tab).n_col() as i32) < BMS
                && (p.tab(tab).tab_flags & (TF_HasGenerated | TF_WithoutRowid)) == 0
                && (l_ws_flags & (WHERE_AUTO_INDEX | WHERE_BLOOMFILTER) as u32) == 0
            {
                // If we know that only a prefix of the record will be used, it is
                // advantageous to reduce the "column count" field in the P4 operand of the
                // OP_OpenRead/Write opcode. This prevents sqlite from needing to compute the
                // length of the data to parse the record
                let mut b = col_used;
                let mut n = 0;
                while b != 0 {
                    b >>= 1;
                    n += 1;
                }
                p.v.change_p4(-1, P4::Int32(n));
            }
            p.v.change_p5(b_fordelete);
            if ii >= 2 && (jointype & (JT_LTORJ | JT_LEFT)) == 0 && w.a[ii].addr_halt == w.a[0].addr_halt {
                p.v.add_op2(OP_IfEmpty, i_cursor, w.i_break);
            }
        } else {
            let (tnum, name) = (p.tab(tab).tnum, p.tab(tab).z_name.clone());
            sqlite3_table_lock(p, i_db, tnum, 0, &name);
        }
        if l_ws_flags & WHERE_INDEXED as u32 != 0 {
            let ix = w.loops[k].p_index;
            let i_index_cur;
            let mut op = OP_OpenRead;
            // iAuxArg is always set to a positive value if ONEPASS is possible
            if !p.tab(tab).has_rowid() && p.idx(ix).is_primary_key() && (wctrl_flags as i32 & WHERE_OR_SUBCLAUSE) != 0 {
                // This is one term of an OR-optimization using the PRIMARY KEY of a WITHOUT
                // ROWID table. No need for a separate index
                i_index_cur = w.a[ii].i_tab_cur;
                op = 0;
            } else if w.e_one_pass as i32 != ONEPASS_OFF {
                let mut c = i_aux_arg;
                for &j in &p.tab(tab).p_index {
                    if j == ix {
                        break;
                    }
                    c += 1;
                }
                i_index_cur = c;
                op = OP_OpenWrite;
                w.ai_cur_one_pass[1] = i_index_cur;
            } else if i_aux_arg != 0 && (wctrl_flags as i32 & WHERE_OR_SUBCLAUSE) != 0 {
                i_index_cur = i_aux_arg;
                op = OP_ReopenIdx;
            } else {
                i_index_cur = p.n_tab;
                p.n_tab += 1;
                if p.idx(ix).b_has_expr && p.db.optimization_enabled(SQLITE_IndexedExpr) {
                    where_add_indexed_expr(p, ix, i_index_cur, tab_list, i_from);
                }
                let part = p.idx(ix).p_part_idx_where;
                if part.nn() && (jointype & JT_RIGHT) == 0 {
                    where_part_idx_expr(p, ix, part, None, i_index_cur, Some((tab_list, i_from)));
                }
            }
            w.a[ii].i_idx_cur = i_index_cur;
            if op != 0 {
                let tnum = p.idx(ix).tnum;
                p.v.add_op3(op, i_index_cur, tnum as i32, i_db);
                sqlite3_vdbe_set_p4_key_info(p, ix);
                let lf = w.loops[k].ws_flags;
                if (lf & WHERE_CONSTRAINT as u32) != 0
                    && (lf & (WHERE_COLUMN_RANGE | WHERE_SKIPSCAN) as u32) == 0
                    && (lf & WHERE_BIGNULL_SORT as u32) == 0
                    && (lf & WHERE_IN_SEEKSCAN as u32) == 0
                    && (w.wctrl_flags as i32 & WHERE_ORDERBY_MIN) == 0
                    && w.e_distinct as i32 != WHERE_DISTINCT_ORDERED
                {
                    p.v.change_p5(OPFLAG_SEEKEQ as u16);
                }
            }
        }
        if i_db >= 0 {
            sqlite3_code_verify_schema(p, i_db);
        }
        if (jointype & JT_RIGHT) != 0 {
            let mut rj = WhereRightJoin::default();
            let b_bloomable;
            rj.i_match = p.n_tab;
            p.n_tab += 1;
            p.n_mem += 1;
            rj.reg_return = p.n_mem;
            p.v.add_op2(OP_Null, 0, rj.reg_return);
            if p.tab(tab).has_rowid() {
                p.v.add_op2(OP_OpenEphemeral, rj.i_match, 1);
                b_bloomable = true;
                let info = KeyInfo::alloc(p.enc(), 1, 0);
                p.v.append_p4(P4::KeyInfo(Arc::new(info)));
            } else {
                let pk = sqlite3_primary_key_index(p, tab);
                let n_key = p.idx(pk).n_key_col as i32;
                p.v.add_op2(OP_OpenEphemeral, rj.i_match, n_key);
                sqlite3_vdbe_set_p4_key_info(p, pk);
                b_bloomable = sqlite3_index_bloomable(p, pk, n_key);
            }
            if b_bloomable {
                p.n_mem += 1;
                rj.reg_bloom = p.n_mem;
                p.v.add_op2(OP_Blob, 65536, rj.reg_bloom);
            } else {
                rj.reg_bloom = 0;
            }
            w.a[ii].p_rj = Some(rj);
            w.loops[k].ws_flags &= !(WHERE_IDX_ONLY as u32);
            // The nature of RIGHT JOIN processing is such that it messes up the output order
            // for the join. Remove any ORDER BY or DISTINCT optimizations that might have
            // occurred. This is a loss, but RIGHT JOIN is rare, and so the simplicity of the
            // approach is worth it.
            w.n_ob_sat = 0;
            w.e_distinct = WHERE_DISTINCT_UNORDERED as u8;
        }
    }
    w.i_top = p.v.current_addr();

    // Generate the code to do the search. Each iteration of the for loop below generates
    // code for a single nested loop of the VM program.
    for ii in 0..n_tab_list {
        if p.n_err != 0 {
            p.n_query_loop = w.saved_n_query_loop as LogEst;
            return None;
        }
        let ws_flags = w.loops[w.a[ii].p_w_loop].ws_flags;
        let i_from = w.a[ii].i_from as usize;
        let (is_materialized, is_correlated, reg_return, addr_fill_sub) = {
            let it = &p.src(tab_list).a[i_from];
            let q = it.p_subq.as_ref();
            (it.is_materialized, it.is_correlated, q.map_or(0, |q| q.reg_return), q.map_or(0, |q| q.addr_fill_sub))
        };
        if is_materialized {
            let mut i_once = 0;
            if !is_correlated {
                i_once = p.v.add_op0(OP_Once);
            }
            p.v.add_op2(OP_Gosub, reg_return, addr_fill_sub);
            if i_once != 0 {
                p.v.jump_here(i_once);
            }
        }
        if (ws_flags & (WHERE_AUTO_INDEX | WHERE_BLOOMFILTER) as u32) != 0 {
            if (ws_flags & WHERE_AUTO_INDEX as u32) != 0 {
                construct_automatic_index(p, &mut w, 0, not_ready, ii);
            } else {
                sqlite3_construct_bloom_filter(p, &mut w, ii, not_ready);
            }
        }
        let _addr_explain = sqlite3_where_explain_one_scan(p, &w, tab_list, ii, wctrl_flags);
        w.a[ii].addr_body = p.v.current_addr();
        not_ready = sqlite3_where_code_one_loop_start(p, &mut w, ii, not_ready);
        w.i_continue = w.a[ii].addr_cont;
    }

    // Done.
    w.i_end_where = p.v.current_addr();
    if p.unsupported.is_some() {
        p.n_query_loop = w.saved_n_query_loop as LogEst;
        return None;
    }
    Some(w)
}

/// `sqlite3WhereEnd(pWInfo)`: generate the end of the WHERE loop. See comments on
/// sqlite3WhereBegin() for additional information.
pub fn sqlite3_where_end(p: &mut Parse, mut w: WhereInfo) {
    let tab_list = w.p_tab_list;
    let i_end = p.v.current_addr();
    let mut n_rj = 0;
    let mut addr_seek = 0;

    // Generate loop termination code.
    for i in (0..w.n_level as usize).rev() {
        if let Some(mut rj) = w.a[i].p_rj {
            // Terminate the subroutine that forms the interior of the loop of the RIGHT JOIN
            // table
            p.v.resolve_label(w.a[i].addr_cont);
            // Replace addrCont with a new label that will never be used, just so the
            // subsequent call to resolve pLevel->addrCont will have something to resolve.
            w.a[i].addr_cont = p.v.make_label();
            rj.end_subrtn = p.v.current_addr();
            p.v.add_op3(OP_Return, rj.reg_return, rj.addr_subrtn, 1);
            w.a[i].p_rj = Some(rj);
            n_rj += 1;
        }
        let k = w.a[i].p_w_loop;
        let level = w.a[i].clone();
        if level.op != OP_Noop {
            let l = &w.loops[k];
            if w.e_distinct as i32 == WHERE_DISTINCT_ORDERED
                && i == w.n_level as usize - 1 // Ticket [ef9318757b152e3] 2017-10-21
                && (l.ws_flags & WHERE_INDEXED as u32) != 0
                && p.idx(l.p_index).has_stat1
                && l.n_distinct_col > 0
                && p.idx(l.p_index).ai_row_log_est[l.n_distinct_col as usize] >= 36
            {
                let n = l.n_distinct_col as i32;
                let r1 = p.n_mem + 1;
                let mut addr_if_null = 0;
                if level.i_left_join != 0 {
                    addr_if_null = p.v.add_op2(OP_IfNullRow, level.i_idx_cur, r1);
                }
                for j in 0..n {
                    p.v.add_op3(OP_Column, level.i_idx_cur, j, r1 + j);
                }
                p.n_mem += n + 1;
                let op = if level.op == OP_Prev { OP_SeekLT } else { OP_SeekGT };
                addr_seek = p.v.add_op4_int(op, level.i_idx_cur, 0, r1, n);
                p.v.add_op2(OP_Goto, 1, level.p2);
                if level.i_left_join != 0 {
                    p.v.jump_here(addr_if_null);
                }
            }
        }
        if p.src(tab_list).a[level.i_from as usize].from_exists {
            // This is an EXISTS-to-JOIN optimization loop. If this loop sees a successful
            // row, it should break out of itself as well as other EXISTS-to-JOIN loops...
            p.v.add_op2(OP_Goto, 0, level.addr_brk);
        }

        // The common case: Advance to the next row
        let level = w.a[i].clone();
        p.v.resolve_label(level.addr_cont);
        if level.op != OP_Noop {
            p.v.add_op3(level.op, level.p1, level.p2, level.p3 as i32);
            p.v.change_p5(level.p5 as u16);
            if addr_seek != 0 {
                p.v.jump_here(addr_seek);
                addr_seek = 0;
            }
            if level.reg_bignull != 0 {
                p.v.resolve_label(level.addr_bignull);
                p.v.add_op2(OP_DecrJumpZero, level.reg_bignull, level.p2 - 1);
            }
        }
        let l_ws_flags = w.loops[k].ws_flags;
        if (l_ws_flags & WHERE_IN_ABLE as u32) != 0 && !level.a_in_loop.is_empty() {
            p.v.resolve_label(level.addr_nxt);
            for j in (0..level.a_in_loop.len()).rev() {
                let p_in = level.a_in_loop[j];
                p.v.jump_here(p_in.addr_in_top + 1);
                if p_in.e_end_loop_op != OP_Noop {
                    if p_in.n_prefix != 0 {
                        let b_early_out =
                            (l_ws_flags & WHERE_VIRTUALTABLE as u32) == 0 && (l_ws_flags & WHERE_IN_EARLYOUT as u32) != 0;
                        if level.i_left_join != 0 {
                            // For LEFT JOIN queries, cursor pIn->iCur might have been disabled
                            // by OP_NullRow. This occurs for WHERE clauses such as "a = ? AND
                            // b IN (...)", where the index is on (a, b). When the
                            // OP_IfNullRow jump is taken, the pIn->iBase register is NULLed
                            // out as a side effect. But as that register is not used when
                            // the jump is taken, the side-effect is harmless.
                            let a = p.v.current_addr() + 2 + b_early_out as i32;
                            p.v.add_op3(OP_IfNullRow, p_in.i_cur, a, p_in.i_base);
                        }
                        if b_early_out {
                            let a = p.v.current_addr() + 2;
                            p.v.add_op4_int(OP_IfNoHope, level.i_idx_cur, a, p_in.i_base, p_in.n_prefix);
                            // Retarget the OP_IsNull against the left operand of IN so it
                            // jumps past the OP_IfNoHope. This is because the OP_IsNull also
                            // bypasses the OP_Affinity opcode that is required by OP_IfNoHope.
                            p.v.jump_here(p_in.addr_in_top + 1);
                        }
                    }
                    p.v.add_op2(p_in.e_end_loop_op, p_in.i_cur, p_in.addr_in_top);
                }
                p.v.jump_here(p_in.addr_in_top - 1);
            }
        }
        p.v.resolve_label(level.addr_brk);
        if let Some(rj) = level.p_rj {
            p.v.add_op3(OP_Return, rj.reg_return, 0, 1);
        }
        if level.addr_skip != 0 {
            p.v.goto(level.addr_skip);
            p.v.jump_here(level.addr_skip);
            p.v.jump_here(level.addr_skip - 2);
        }
        if level.addr_like_rep != 0 {
            p.v.add_op2(OP_DecrJumpZero, (level.i_like_rep_cntr >> 1) as i32, level.addr_like_rep);
        }
        if level.i_left_join != 0 {
            let ws = l_ws_flags;
            let addr = p.v.add_op1(OP_IfPos, level.i_left_join);
            if (ws & WHERE_IDX_ONLY as u32) == 0 {
                let (via_coroutine, reg_result, n_col) = {
                    let it = &p.src(tab_list).a[level.i_from as usize];
                    (it.via_coroutine, it.p_subq.as_ref().map_or(0, |q| q.reg_result), p.tab(it.p_s_tab).n_col() as i32)
                };
                if via_coroutine {
                    p.v.add_op3(OP_Null, 0, reg_result, reg_result + n_col - 1);
                }
                p.v.add_op1(OP_NullRow, level.i_tab_cur);
            }
            if (ws & WHERE_INDEXED as u32) != 0 || ((ws & WHERE_MULTI_OR as u32) != 0 && level.p_covering_idx.nn()) {
                if ws & WHERE_MULTI_OR as u32 != 0 {
                    let ix = level.p_covering_idx;
                    let i_db = p.tab(p.idx(ix).p_table).i_db;
                    let tnum = p.idx(ix).tnum;
                    p.v.add_op3(OP_ReopenIdx, level.i_idx_cur, tnum as i32, i_db);
                    sqlite3_vdbe_set_p4_key_info(p, ix);
                }
                p.v.add_op1(OP_NullRow, level.i_idx_cur);
            }
            if ws & WHERE_IN_ABLE as u32 != 0 {
                for p_in in &level.a_in_loop {
                    if p_in.e_end_loop_op != OP_Noop {
                        p.v.add_op1(OP_NullRow, p_in.i_cur);
                    }
                }
            }
            if level.op == OP_Return {
                p.v.add_op2(OP_Gosub, level.p1, level.addr_first);
            } else {
                p.v.goto(level.addr_first);
            }
            p.v.jump_here(addr);
        }
    }

    for i in 0..w.n_level as usize {
        let level = w.a[i].clone();
        let i_from = level.i_from as usize;
        let (tab, via_coroutine, reg_result) = {
            let it = &p.src(tab_list).a[i_from];
            (it.p_s_tab, it.via_coroutine, it.p_subq.as_ref().map_or(0, |q| q.reg_result))
        };
        let k = level.p_w_loop;

        // Do RIGHT JOIN processing. Generate code that will output the unmatched rows of the
        // right operand of the RIGHT JOIN with all of the columns of the left operand set to
        // NULL.
        if level.p_rj.is_some() {
            sqlite3_where_right_join_loop(p, &mut w, i);
            continue;
        }

        // For a co-routine, change all OP_Column references to the table of the co-routine
        // into OP_Copy of result contained in a register. OP_Rowid becomes OP_Null.
        if via_coroutine {
            translate_column_to_copy(p, level.addr_body, level.i_tab_cur, reg_result, 0);
            continue;
        }

        // If this scan uses an index, make VDBE code substitutions to read data from the
        // index instead of from the table where possible. In some cases this optimization
        // prevents the table from ever being read, which can yield a significant performance
        // boost.
        //
        // Calls to the code generator in between sqlite3WhereBegin and sqlite3WhereEnd will
        // have created code that references the table directly. This loop scans all that
        // code looking for opcodes that reference the table and converts them into opcodes
        // that reference the index.
        let mut idx = IndexId::NULL;
        if w.loops[k].ws_flags & (WHERE_INDEXED | WHERE_IDX_ONLY) as u32 != 0 {
            idx = w.loops[k].p_index;
        } else if w.loops[k].ws_flags & WHERE_MULTI_OR as u32 != 0 {
            idx = level.p_covering_idx;
        }
        if idx.nn() {
            let last = if w.e_one_pass as i32 == ONEPASS_OFF || !p.tab(p.idx(idx).p_table).has_rowid() { i_end } else { w.i_end_where };
            if p.idx(idx).b_has_expr {
                for ie in p.p_idx_epr.iter_mut() {
                    if ie.i_idx_cur == level.i_idx_cur {
                        ie.i_data_cur = -1;
                        ie.i_idx_cur = -1;
                    }
                }
            }
            let start = level.addr_body + 1;
            let mut a = start;
            // do{ ... }while( (++pOp)<pLastOp );
            loop {
                let (opcode, p1, p2) = {
                    let op = p.v.get_op(a);
                    (op.opcode, op.p1, op.p2)
                };
                if p1 != level.i_tab_cur {
                    // no-op
                } else if opcode == OP_Column {
                    let mut x = p2;
                    if !p.tab(tab).has_rowid() {
                        let pk = sqlite3_primary_key_index(p, tab);
                        x = p.idx(pk).ai_column[x as usize] as i32;
                    } else {
                        x = sqlite3_storage_column_to_table(p.tab(tab), x);
                    }
                    x = sqlite3_table_column_to_index(p.idx(idx), x);
                    if x >= 0 {
                        let op = p.v.get_op_mut(a);
                        op.p2 = x;
                        op.p1 = level.i_idx_cur;
                    } else if w.loops[k].ws_flags & (WHERE_IDX_ONLY | WHERE_EXPRIDX) as u32 != 0 {
                        if w.loops[k].ws_flags & WHERE_IDX_ONLY as u32 != 0 {
                            // An error. pLoop is supposed to be a covering index loop, and
                            // yet the VM code refers to a column of the table that is not
                            // part of the index.
                            p.error_msg("internal query planner error");
                            p.rc = 2; // SQLITE_INTERNAL
                        } else {
                            // The WHERE_EXPRIDX flag is set by the planner when it is likely
                            // that pLoop is a covering index loop, but it is not possible to
                            // be 100% sure. In this case, any OP_Explain opcode corresponding
                            // to this loop describes the index as a "COVERING INDEX". But,
                            // pOp proves that pLoop is not actually a covering index loop. So
                            // clear the WHERE_EXPRIDX flag and rewrite the text that
                            // accompanies the OP_Explain opcode, if any.
                            w.loops[k].ws_flags &= !(WHERE_EXPRIDX as u32);
                            let flags = w.wctrl_flags;
                            sqlite3_where_add_explain_text(p, &w, level.addr_body - 1, tab_list, i, flags);
                        }
                    }
                } else if opcode == OP_Rowid {
                    let op = p.v.get_op_mut(a);
                    op.p1 = level.i_idx_cur;
                    op.opcode = OP_IdxRowid;
                } else if opcode == OP_IfNullRow {
                    p.v.get_op_mut(a).p1 = level.i_idx_cur;
                }
                a += 1;
                if a >= last {
                    break;
                }
            }
        }
    }

    // The "break" point is here, just past the end of the outer loop. Set it.
    p.v.resolve_label(w.i_break);

    // Final cleanup
    p.n_query_loop = w.saved_n_query_loop as LogEst;
    p.within_rj_subrtn = p.within_rj_subrtn.wrapping_sub(n_rj);
}
