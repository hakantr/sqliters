//! The query-rewriting optimizations of `select.c` that run before a SELECT is coded: the
//! query flattener (`flattenSubquery`), WHERE-clause constant propagation
//! (`propagateConstants`), the predicate push-down (`pushDownWhereTerms`), unused-column
//! elimination (`disableUnusedSubqueryResultColumns`), the count-of-view rewrite
//! (`countOfViewOptimization`), EXISTS-to-JOIN (`existsToJoin`) and the check of ON clauses
//! that were moved to the WHERE clause (`sqlite3SelectCheckOnClauses`).

use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::walker::*;

// ---------------------------------------------------------------------------------------------
// WHERE-clause constant propagation
// ---------------------------------------------------------------------------------------------
//
// `struct WhereConst` is carried in the walker: `v_expr` = apExpr ([i*2] is COLUMN and [i*2+1]
// is VALUE), `n` = nChng, `b1` = bHasAffBlob, `m` = mExcludeOn.

/// `constInsert(pConst, pColumn, pValue, pExpr)`: add a new entry to the pConst object. Except,
/// do not add duplicate pColumn entries. Also, do not add if doing so would not be appropriate.
///
/// The caller guarantees the pColumn is a column and pValue is a constant. This routine has to
/// do some additional checks before completing the insert.
fn const_insert(p: &mut Parse, c: &mut Walker, column: ExprId, value: ExprId, e: ExprId) {
    if p.e(column).has(EP_FixedCol) {
        return;
    }
    if sqlite3_expr_affinity(p, value) != 0 {
        return;
    }
    let coll = sqlite3_expr_compare_coll_seq(p, e);
    if !sqlite3_is_binary(coll.as_deref()) {
        return;
    }
    if sqlite3_expr_can_return_subtype(p, value) {
        return;
    }

    // 2018-10-25 ticket [cf5ed20f]: Make sure the same pColumn is not inserted more than once
    for i in 0..c.v_expr.len() / 2 {
        let e2 = c.v_expr[i * 2];
        if p.e(e2).i_table == p.e(column).i_table && p.e(e2).i_column == p.e(column).i_column {
            return; // Already present. Return without doing anything.
        }
    }
    if sqlite3_expr_affinity(p, column) <= SQLITE_AFF_BLOB {
        c.b1 = true;
    }
    c.v_expr.push(column);
    c.v_expr.push(value);
}

/// `findConstInWhere(pConst, pExpr)`: find all terms of COLUMN=VALUE or VALUE=COLUMN in pExpr
/// that is a conjunct in the WHERE clause; the ON-clause terms of mExcludeOn are skipped.
fn find_const_in_where(p: &mut Parse, c: &mut Walker, e: ExprId) {
    if e.is_null() {
        return;
    }
    if p.e(e).has(c.m) {
        return;
    }
    if p.e(e).op == TK_AND {
        let (l, r) = (p.e(e).p_left, p.e(e).p_right);
        find_const_in_where(p, c, r);
        find_const_in_where(p, c, l);
        return;
    }
    if p.e(e).op != TK_EQ {
        return;
    }
    let (left, right) = (p.e(e).p_left, p.e(e).p_right);
    if p.e(right).op == TK_COLUMN && sqlite3_expr_is_constant(p, left) != 0 {
        const_insert(p, c, right, left, e);
    }
    if p.e(left).op == TK_COLUMN && sqlite3_expr_is_constant(p, right) != 0 {
        const_insert(p, c, left, right, e);
    }
}

/// `propagateConstantExprRewriteOne(pConst, pExpr, bIgnoreAffBlob)`: this is a helper function
/// for Walker callback propagateConstantExprRewrite().
///
/// Argument pExpr is a candidate expression to be replaced by a value. If pExpr is equivalent
/// to one of the columns named in pWalker->u.pConst, then overwrite it with the corresponding
/// value. Except, do not do so if argument bIgnoreAffBlob is non-zero and the affinity of
/// pExpr is BLOB.
fn propagate_constant_expr_rewrite_one(p: &mut Parse, c: &mut Walker, e: ExprId, b_ignore_aff_blob: bool) -> i32 {
    if p.e(e).op != TK_COLUMN {
        return WRC_Continue;
    }
    if p.e(e).has(EP_FixedCol | c.m) {
        return WRC_Continue;
    }
    for i in 0..c.v_expr.len() / 2 {
        let column = c.v_expr[i * 2];
        if column == e {
            continue;
        }
        if p.e(column).i_table != p.e(e).i_table {
            continue;
        }
        if p.e(column).i_column != p.e(e).i_column {
            continue;
        }
        if b_ignore_aff_blob && sqlite3_expr_affinity(p, column) <= SQLITE_AFF_BLOB {
            break;
        }
        // A match is found. Add the EP_FixedCol property
        c.n += 1;
        p.em(e).flags &= !EP_Leaf;
        p.em(e).flags |= EP_FixedCol;
        let v = c.v_expr[i * 2 + 1];
        let d = sqlite3_expr_dup(p, v, 0);
        p.em(e).p_left = d;
        break;
    }
    WRC_Prune
}

/// `propagateConstantExprRewrite(pWalker, pExpr)`: this is a Walker expression callback. pExpr
/// is a node from the WHERE clause of a SELECT statement. This function examines pExpr to see
/// if any substitutions based on the contents of pWalker->u.pConst should be made to pExpr or
/// its immediate children.
///
/// A substitution is made if:
///
///   + pExpr is a column with an affinity other than BLOB that matches one of the columns in
///     pWalker->u.pConst, or
///
///   + pExpr is a binary comparison operator (=, <=, >=, <, >) that uses an affinity other
///     than TEXT and one of its immediate children is a column that matches one of the columns
///     in pWalker->u.pConst.
fn propagate_constant_expr_rewrite(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if w.b1 {
        let op = p.e(e).op;
        if (op >= TK_EQ && op <= TK_GE) || op == TK_IS {
            let l = p.e(e).p_left;
            propagate_constant_expr_rewrite_one(p, w, l, false);
            let l = p.e(e).p_left;
            if sqlite3_expr_affinity(p, l) != SQLITE_AFF_TEXT {
                let r = p.e(e).p_right;
                propagate_constant_expr_rewrite_one(p, w, r, false);
            }
        }
    }
    let b = w.b1;
    propagate_constant_expr_rewrite_one(p, w, e, b)
}

/// `propagateConstants(pParse, p)`: the WHERE-clause constant propagation optimization.
///
/// If the WHERE clause contains terms of the form COLUMN=CONSTANT or CONSTANT=COLUMN that are
/// top-level AND-connected terms that are not part of a ON clause from a LEFT JOIN, then
/// throughout the query replace all other occurrences of COLUMN with CONSTANT.
///
/// One must be careful with the affinity and collation of the comparison (see the long
/// comment in `select.c`): only BINARY comparisons whose value has no affinity take part, and
/// a column of BLOB affinity is only replaced where it is the operand of a comparison.
///
/// Return the number of substitutions made.
pub fn propagate_constants(p: &mut Parse, sel: SelectId) -> i32 {
    let mut n_chng = 0;
    loop {
        let mut w = Walker::default();
        let src = p.s(sel).p_src;
        if src.nn() && !p.src(src).a.is_empty() && (p.src(src).a[0].jointype as i32 & JT_LTORJ) != 0 {
            // Do not propagate constants on any ON clause if there is a RIGHT JOIN anywhere in
            // the query
            w.m = EP_InnerON | EP_OuterON;
        } else {
            // Do not propagate constants through the ON clause of a LEFT JOIN
            w.m = EP_OuterON;
        }
        let wh = p.s(sel).p_where;
        find_const_in_where(p, &mut w, wh);
        if !w.v_expr.is_empty() {
            w.x_expr_callback = Some(propagate_constant_expr_rewrite);
            w.x_select_callback = Some(sqlite3_select_walk_noop);
            w.x_select_callback2 = None;
            w.walker_depth = 0;
            sqlite3_walk_expr(p, &mut w, wh);
            n_chng += w.n;
        }
        if w.n == 0 {
            break;
        }
    }
    n_chng
}

// ---------------------------------------------------------------------------------------------
// ON clauses moved to the WHERE clause
// ---------------------------------------------------------------------------------------------
//
// `struct CheckOnCtx` chain: the walker's `v_ctx` stack (the last entry is the current
// context, the one before it its pParent): (pSrc, iJoin, bFuncArg).

/// `selectCheckOnClausesExpr(pWalker, pExpr)`: walker callback of
/// sqlite3SelectCheckOnClauses().
fn select_check_on_clauses_expr(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    let top = w.v_ctx.len() - 1;
    let (ctx_src, ctx_join, _) = w.v_ctx[top];
    // Check if pExpr is root or near-root of an ON clause constraint that needs to be checked
    // to ensure that it does not refer to tables in its FROM clause to the right of itself.
    // i.e. it is either:
    //
    //   + an ON clause on an OUTER join, or
    //   + an ON clause on an INNER join within a FROM that features at least one RIGHT or
    //     FULL join.
    if p.e(e).has(EP_OuterON) || (p.e(e).has(EP_InnerON) && (p.src(ctx_src).a[0].jointype as i32 & JT_LTORJ) != 0) {
        // If CheckOnCtx.iJoin is already set, then fall through and process this expression
        // node as normal. Or, if CheckOnCtx.iJoin is still 0, set it to the cursor number of
        // the RHS of the join to which this ON expression was attached and then iterate
        // through the entire expression.
        if ctx_join < 0 {
            w.v_ctx[top].1 = p.e(e).w;
            sqlite3_walk_expr_nn(p, w, e);
            w.v_ctx[top].1 = -1;
            return WRC_Prune;
        }
    }

    if p.e(e).op == TK_COLUMN {
        // A column expression. Find the SrcList (if any) to which it refers. Then, if
        // CheckOnCtx.iJoin indicates that this expression is part of an ON clause from that
        // SrcList (i.e. if iJoin is non-zero), check that it does not refer to a table to the
        // right of CheckOnCtx.iJoin.
        let i_tab = p.e(e).i_table;
        let mut k = top as isize;
        while k >= 0 {
            let (src, i_join, b_func_arg) = w.v_ctx[k as usize];
            let n_src = p.src(src).a.len();
            let mut ii = 0usize;
            while ii < n_src && p.src(src).a[ii].i_cursor != i_tab {
                ii += 1;
            }
            if ii < n_src {
                // pSrc is the FROM clause that contains iTab
                if i_join >= 0 {
                    let mut jj = ii as isize - 1;
                    while jj >= 0 && p.src(src).a[jj as usize].i_cursor != i_join {
                        jj -= 1;
                    }
                    if jj >= 0 {
                        // Table iJoin appears to the left of table iTab in the SrcList. So
                        // pExpr is part of an ON clause that refers to a table to the right
                        // of itself.
                        let msg = format!(
                            "{} references tables to its right",
                            if b_func_arg { "table-function argument" } else { "ON clause" }
                        );
                        p.error_msg(msg);
                        return WRC_Abort;
                    }
                }
                break;
            }
            k -= 1;
        }
    }
    WRC_Continue
}

/// `selectCheckOnClausesSelect(pWalker, pSelect)`: walker callback of
/// sqlite3SelectCheckOnClauses().
fn select_check_on_clauses_select(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let ctx_src = w.v_ctx[w.v_ctx.len() - 1].0;
    if p.s(sel).p_src == ctx_src || p.src(p.s(sel).p_src).a.is_empty() {
        WRC_Continue
    } else {
        w.v_ctx.push((p.s(sel).p_src, -1, false));
        sqlite3_walk_select(p, w, sel);
        w.v_ctx.pop();
        p.sm(sel).sel_flags &= !SF_OnToWhere;
        WRC_Prune
    }
}

/// `sqlite3SelectCheckOnClauses(pParse, pSelect)`: check all ON clauses in pSelect to verify
/// that they do not reference columns to the right.
pub fn sqlite3_select_check_on_clauses(p: &mut Parse, sel: SelectId) {
    let mut w = Walker {
        x_expr_callback: Some(select_check_on_clauses_expr),
        x_select_callback: Some(select_check_on_clauses_select),
        ..Walker::default()
    };
    let src = p.s(sel).p_src;
    w.v_ctx.push((src, -1, false));
    let wh = p.s(sel).p_where;
    sqlite3_walk_expr(p, &mut w, wh);
    p.sm(sel).sel_flags &= !SF_OnToWhere;

    // Check for any table-function args that are attached to virtual tables on the RHS of an
    // outer join. They are subject to similar constraints as ON clauses. (Table-valued
    // functions are virtual tables: sqlite3LocateTableItem() has already reported them as
    // unsupported.)
}

// ---------------------------------------------------------------------------------------------
// The query flattener
// ---------------------------------------------------------------------------------------------

/// `struct SubstContext`: an instance of the SubstContext object describes a substitution
/// edit to be performed on a parse tree.
///
/// All references to columns in table iTable are to be replaced by corresponding expressions
/// in pEList.
///
/// ## About "isOuterJoin":
///
/// The isOuterJoin column indicates that the replacement will occur into a position in the
/// parent that is NULL-able due to an OUTER JOIN. Either the target slot in the parent is the
/// right operand of a LEFT JOIN, or one of the left operands of a RIGHT JOIN. In either case,
/// we need to potentially bypass the substituted expression with OP_IfNullRow.
///
/// Suppose the original expression is an integer constant. Even though the table has the
/// nullRow flag set, because the expression is an integer constant, it will not be NULLed
/// out. So instead, we insert an OP_IfNullRow opcode before substituted expressions so that
/// they are not evaluated when the nullRow flag is set (TK_IF_NULL_ROW).
struct SubstContext {
    /// Replace references to this table
    i_table: i32,
    /// New table number
    i_new_table: i32,
    /// Add TK_IF_NULL_ROW opcodes on each replacement
    is_outer_join: bool,
    /// Depth of sub-query recursion. Top==1
    n_sel_depth: i32,
    /// Replacement expressions
    p_e_list: ExprListId,
    /// Collation sequences for replacement expr
    p_c_list: ExprListId,
}

/// `substExpr(pSubst, pExpr)`: scan through the expression pExpr. Replace every reference to
/// a column in table number iTable with a copy of the iColumn-th entry in pEList. (But leave
/// references to the ROWID column unchanged.)
///
/// This routine is part of the flattening procedure. A subquery whose result set is defined
/// by pEList appears as entry in the FROM clause of a SELECT such that the VDBE cursor
/// assigned to that FORM clause entry is iTable. This routine makes the necessary changes to
/// pExpr so that it refers directly to the source table of the subquery rather the result set
/// of the subquery.
fn subst_expr(p: &mut Parse, x: &mut SubstContext, e: ExprId) -> ExprId {
    if e.is_null() {
        return e;
    }
    if p.e(e).has(EP_OuterON | EP_InnerON) && p.e(e).w == x.i_table {
        p.em(e).w = x.i_new_table;
    }
    if p.e(e).op == TK_COLUMN && p.e(e).i_table == x.i_table && !p.e(e).has(EP_FixedCol) {
        let i_column = p.e(e).i_column as usize;
        let mut copy = p.l(x.p_e_list).a[i_column].p_expr;
        if sqlite3_expr_is_vector(p, copy) {
            sqlite3_vector_error_msg(p, copy);
            return e;
        }
        if x.is_outer_join && (p.e(copy).op != TK_COLUMN || p.e(copy).i_table != x.i_new_table) {
            let if_null_row = Expr {
                op: TK_IF_NULL_ROW,
                p_left: copy,
                i_table: x.i_new_table,
                i_column: -99,
                flags: EP_IfNullRow,
                ..Expr::default()
            };
            copy = p.new_expr(if_null_row);
        }
        let mut new = sqlite3_expr_dup(p, copy, 0);
        if x.is_outer_join {
            p.em(new).flags |= EP_CanBeNull;
        }
        if p.e(new).op == TK_TRUEFALSE {
            let v = sqlite3_expr_truth_value(p, new);
            let n = p.em(new);
            n.i_value = v;
            n.z_token = None;
            n.op = TK_INTEGER;
            n.flags |= EP_IntValue;
        }
        // Ensure that the expression now has an implicit collation sequence, just as it did
        // when it was a column of a view or sub-query.
        {
            let nat = sqlite3_expr_coll_seq(p, new);
            let c_expr = p.l(x.p_c_list).a[i_column].p_expr;
            let coll = sqlite3_expr_coll_seq(p, c_expr);
            if !crate::whereexpr::coll_ptr_eq(&nat, &coll) || (p.e(new).op != TK_COLUMN && p.e(new).op != TK_COLLATE) {
                let name = coll.as_ref().map_or("BINARY".to_owned(), |c| c.name.clone());
                new = sqlite3_expr_add_collate_string(p, new, &name);
            }
        }
        p.em(new).flags &= !EP_Collate;
        if p.e(e).has(EP_OuterON | EP_InnerON) {
            let (w, f) = (p.e(e).w, p.e(e).flags & (EP_OuterON | EP_InnerON));
            crate::select::sqlite3_set_join_expr(p, new, w, f);
        }
        return new;
    }
    if p.e(e).op == TK_IF_NULL_ROW && p.e(e).i_table == x.i_table {
        p.em(e).i_table = x.i_new_table;
    }
    if p.e(e).op == TK_AGG_FUNCTION && p.e(e).op2 as i32 >= x.n_sel_depth {
        p.em(e).op2 -= 1;
    }
    let l = p.e(e).p_left;
    let l = subst_expr(p, x, l);
    p.em(e).p_left = l;
    let r = p.e(e).p_right;
    let r = subst_expr(p, x, r);
    p.em(e).p_right = r;
    if p.e(e).use_x_select() {
        let s = p.e(e).x_select();
        subst_select(p, x, s, true);
    } else {
        let l = p.e(e).x_list();
        subst_expr_list(p, x, l);
    }
    // (EP_WinFunc: window functions are reported as unsupported before any flattening)
    e
}

/// `substExprList(pSubst, pList)`
fn subst_expr_list(p: &mut Parse, x: &mut SubstContext, list: ExprListId) {
    if list.is_null() {
        return;
    }
    for i in 0..p.l(list).a.len() {
        let e = p.l(list).a[i].p_expr;
        let n = subst_expr(p, x, e);
        p.lm(list).a[i].p_expr = n;
    }
}

/// `substSelect(pSubst, p, doPrior)`
fn subst_select(p: &mut Parse, x: &mut SubstContext, mut sel: SelectId, do_prior: bool) {
    if sel.is_null() {
        return;
    }
    x.n_sel_depth += 1;
    loop {
        let (el, gb, ob, hv, wh, src) = {
            let s = p.s(sel);
            (s.p_e_list, s.p_group_by, s.p_order_by, s.p_having, s.p_where, s.p_src)
        };
        subst_expr_list(p, x, el);
        subst_expr_list(p, x, gb);
        subst_expr_list(p, x, ob);
        let h = subst_expr(p, x, hv);
        p.sm(sel).p_having = h;
        let w = subst_expr(p, x, wh);
        p.sm(sel).p_where = w;
        for i in 0..p.src(src).a.len() {
            let (sub, is_tab_func, func_arg) = {
                let it = &p.src(src).a[i];
                (if it.is_subquery { it.p_subq.as_ref().map(|q| q.p_select) } else { None }, it.is_tab_func, it.p_func_arg)
            };
            if let Some(sub) = sub {
                subst_select(p, x, sub, true);
            }
            if is_tab_func {
                subst_expr_list(p, x, func_arg);
            }
        }
        if !do_prior {
            break;
        }
        sel = p.s(sel).p_prior;
        if sel.is_null() {
            break;
        }
    }
    x.n_sel_depth -= 1;
}

/// `recomputeColumnsUsedExpr(pWalker, pExpr)`: this is part of the flattening procedure. The
/// walker carries the FROM-clause item as (`p_src_list`, `n`).
fn recompute_columns_used_expr(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).op != TK_COLUMN {
        return WRC_Continue;
    }
    let (src, i) = (w.p_src_list, w.n as usize);
    if p.src(src).a[i].i_cursor != p.e(e).i_table {
        return WRC_Continue;
    }
    if p.e(e).i_column < 0 {
        return WRC_Continue;
    }
    let m = crate::resolve::sqlite3_expr_col_used(p, e);
    p.srcm(src).a[i].col_used |= m;
    WRC_Continue
}

/// `recomputeColumnsUsed(pSelect, pSrcItem)`: after a flattening, the columns used by the
/// FROM-clause item are recomputed from the complete SELECT statement.
fn recompute_columns_used(p: &mut Parse, sel: SelectId, src: SrcListId, i: usize) {
    if p.src(src).a[i].p_s_tab.is_null() {
        return;
    }
    let mut w = Walker {
        x_expr_callback: Some(recompute_columns_used_expr),
        x_select_callback: Some(sqlite3_select_walk_noop),
        p_src_list: src,
        n: i as i32,
        ..Walker::default()
    };
    p.srcm(src).a[i].col_used = 0;
    sqlite3_walk_select(p, &mut w, sel);
}

/// `srclistRenumberCursors(pParse, aCsrMap, pSrc, iExcept)`: assign new cursor numbers to
/// each of the items in pSrc. For each new cursor number assigned, set an entry in the
/// aCsrMap[] array to map the old cursor number to the new:
///
///     aCsrMap[iOld+1] = iNew;
///
/// The array is guaranteed by the caller to be large enough for all existing cursor numbers
/// in pSrc. aCsrMap[0] is the array size.
///
/// If pSrc contains any sub-selects, call this routine recursively on the FROM clause of each
/// such sub-select, with iExcept set to -1.
fn srclist_renumber_cursors(p: &mut Parse, a_csr_map: &mut [i32], src: SrcListId, i_except: i32) {
    for i in 0..p.src(src).a.len() {
        if i as i32 == i_except {
            continue;
        }
        let (is_recursive, cur) = (p.src(src).a[i].is_recursive, p.src(src).a[i].i_cursor);
        if !is_recursive || a_csr_map[(cur + 1) as usize] == 0 {
            a_csr_map[(cur + 1) as usize] = p.n_tab;
            p.n_tab += 1;
        }
        p.srcm(src).a[i].i_cursor = a_csr_map[(cur + 1) as usize];
        if p.src(src).a[i].is_subquery {
            let mut s = p.src(src).a[i].p_subq.as_ref().map_or(SelectId::NULL, |q| q.p_select);
            while s.nn() {
                let ss = p.s(s).p_src;
                srclist_renumber_cursors(p, a_csr_map, ss, -1);
                s = p.s(s).p_prior;
            }
        }
    }
}

/// `renumberCursorDoMapping(pWalker, piCursor)`: *piCursor is a cursor number. Change it if
/// it needs to be mapped.
fn renumber_cursor_do_mapping(a_csr_map: &[i32], i_csr: i32) -> i32 {
    if i_csr < a_csr_map[0] && a_csr_map[(i_csr + 1) as usize] > 0 { a_csr_map[(i_csr + 1) as usize] } else { i_csr }
}

/// `renumberCursorsCb(pWalker, pExpr)`: expression walker callback used by
/// renumberCursors() to update Expr objects to match newly assigned cursor numbers.
fn renumber_cursors_cb(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    let op = p.e(e).op;
    if op == TK_COLUMN || op == TK_IF_NULL_ROW {
        let n = renumber_cursor_do_mapping(&w.v_int, p.e(e).i_table);
        p.em(e).i_table = n;
    }
    if p.e(e).has(EP_OuterON | EP_InnerON) {
        let n = renumber_cursor_do_mapping(&w.v_int, p.e(e).w);
        p.em(e).w = n;
    }
    WRC_Continue
}

/// `renumberCursors(pParse, p, iExcept, aCsrMap)`: assign a new cursor number to each cursor
/// in the FROM clause (Select.pSrc) of the SELECT statement passed as the second argument,
/// and to each cursor in the FROM clause of any FROM clause sub-selects, recursively. Except,
/// do not assign a new cursor number to the iExcept'th element in the FROM clause of p.
/// Update all expressions and other references to refer to the new cursor numbers.
///
/// Argument aCsrMap is an array that may be used for temporary working space. Two guarantees
/// are made by the caller: aCsrMap[0] is the array size, and at least the first
/// (pParse->nTab+1) entries of the array are 0 on entry.
fn renumber_cursors(p: &mut Parse, sel: SelectId, i_except: i32, a_csr_map: &mut Vec<i32>) {
    let src = p.s(sel).p_src;
    srclist_renumber_cursors(p, a_csr_map, src, i_except);
    let mut w = Walker {
        x_expr_callback: Some(renumber_cursors_cb),
        x_select_callback: Some(sqlite3_select_walk_noop),
        v_int: std::mem::take(a_csr_map),
        ..Walker::default()
    };
    sqlite3_walk_select(p, &mut w, sel);
    *a_csr_map = w.v_int;
}

/// `findLeftmostExprlist(pSel)`: if pSel is not part of a compound SELECT, return a pointer
/// to its expression list. Otherwise, return a pointer to the expression list of the leftmost
/// SELECT in the compound.
fn find_leftmost_exprlist(p: &Parse, mut sel: SelectId) -> ExprListId {
    while p.s(sel).p_prior.nn() {
        sel = p.s(sel).p_prior;
    }
    p.s(sel).p_e_list
}

/// `compoundHasDifferentAffinities(p)`: return true if any of the result-set columns in the
/// compound query have incompatible affinities on one or more arms of the compound.
fn compound_has_different_affinities(p: &Parse, sel: SelectId) -> bool {
    let list = p.s(sel).p_e_list;
    for ii in 0..p.l(list).a.len() {
        let aff = sqlite3_expr_affinity(p, p.l(list).a[ii].p_expr);
        let mut sub1 = p.s(sel).p_prior;
        while sub1.nn() {
            if sqlite3_expr_affinity(p, p.l(p.s(sub1).p_e_list).a[ii].p_expr) != aff {
                return true;
            }
            sub1 = p.s(sub1).p_prior;
        }
    }
    false
}

/// `sqlite3SubqueryDetach(db, pItem)`: for a FROM-clause item that is a subquery, remove the
/// subquery from the item and return it.
fn sqlite3_subquery_detach(p: &mut Parse, src: SrcListId, i: usize) -> SelectId {
    let it = &mut p.srcm(src).a[i];
    let sel = it.p_subq.take().map_or(SelectId::NULL, |q| q.p_select);
    it.is_subquery = false;
    it.z_database = None;
    sel
}

/// `flattenSubquery(pParse, p, iFrom, isAgg)`: this routine attempts to flatten subqueries as
/// a performance optimization. This routine returns 1 if it makes changes and 0 if no
/// flattening occurs.
///
/// To understand the concept of flattening, consider the following query:
///
///     SELECT a FROM (SELECT x+y AS a FROM t1 WHERE z<100) WHERE a>5
///
/// The default way of implementing this query is to execute the subquery first and store the
/// results in a temporary table, then run the outer query on that temporary table. This
/// requires two passes over the data. Furthermore, because the temporary table has no
/// indices, the WHERE clause on the outer query cannot be optimized.
///
/// This routine attempts to rewrite queries such as the above into a single flat select,
/// like this:
///
///     SELECT x+y AS a FROM t1 WHERE z<100 AND a>5
///
/// The code generated for this simplification gives the same result but only has to scan the
/// data once. And because indices might exist on the table t1, a complete scan of the data
/// might be avoided.
///
/// Flattening is subject to the restrictions (1) to (28) listed in `select.c` (the numbers
/// are quoted next to the tests below). In this routine, the "p" parameter is a pointer to
/// the outer query. The subquery is p->pSrc->a[iFrom]. isAgg is true if the outer query uses
/// aggregates.
///
/// If flattening is not attempted, this routine is a no-op and returns 0. If flattening is
/// attempted this routine returns 1.
///
/// All of the expression analysis must occur on both the outer query and the subquery before
/// this routine runs.
pub fn flatten_subquery(p: &mut Parse, sel: SelectId, i_from: usize, is_agg: bool) -> bool {
    let mut i_new_parent = -1; // Replacement table for iParent
    let mut is_outer_join = false; // True if pSub is the right side of a LEFT JOIN
    let mut a_csr_map: Option<Vec<i32>> = None;

    // Check to see if flattening is permitted. Return 0 if not.
    if p.db.optimization_disabled(SQLITE_QueryFlattener) {
        return false;
    }
    let mut src = p.s(sel).p_src;
    let i_parent = p.src(src).a[i_from].i_cursor;
    let mut sub = p.src(src).a[i_from].p_subq.as_ref().expect("subquery").p_select;

    if p.s(sel).p_win.nn() || p.s(sub).p_win.nn() {
        return false; // Restriction (25)
    }

    let sub_src = p.s(sub).p_src;
    // Prior to version 3.1.2, when LIMIT and OFFSET had to be simple constants, not arbitrary
    // expressions, we allowed some combining of LIMIT and OFFSET because they could be
    // computed at compile-time. But when LIMIT and OFFSET became arbitrary expressions, we
    // were forced to add restrictions (13) and (14).
    if p.s(sub).p_limit.nn() && p.s(sel).p_limit.nn() {
        return false; // Restriction (13)
    }
    if p.s(sub).p_limit.nn() && p.e(p.s(sub).p_limit).p_right.nn() {
        return false; // Restriction (14)
    }
    if (p.s(sel).sel_flags & SF_Compound) != 0 && p.s(sub).p_limit.nn() {
        return false; // Restriction (15)
    }
    if p.src(sub_src).a.is_empty() {
        return false; // Restriction (7)
    }
    if p.s(sub).sel_flags & SF_Distinct != 0 {
        return false; // Restriction (4)
    }
    if p.s(sub).p_limit.nn() && (p.src(src).a.len() > 1 || is_agg) {
        return false; // Restrictions (8)(9)
    }
    if p.s(sel).p_order_by.nn() && p.s(sub).p_order_by.nn() {
        return false; // Restriction (11)
    }
    if is_agg && p.s(sub).p_order_by.nn() {
        return false; // Restriction (16)
    }
    if p.s(sub).p_limit.nn() && p.s(sel).p_where.nn() {
        return false; // Restriction (19)
    }
    if p.s(sub).p_limit.nn() && (p.s(sel).sel_flags & SF_Distinct) != 0 {
        return false; // Restriction (21)
    }
    if p.s(sub).sel_flags & SF_Recursive != 0 {
        return false; // Restrictions (22)
    }

    // If the subquery is the right operand of a LEFT JOIN, then the subquery may not be a join
    // itself (3a). Example of why this is not allowed:
    //
    //      t1 LEFT OUTER JOIN (t2 JOIN t3)
    //
    // If we flatten the above, we would get
    //
    //         (t1 LEFT OUTER JOIN t2) JOIN t3
    //
    // which is not at all the same thing.
    //
    // See also tickets #306, #350, and #3300.
    let sub_jointype = p.src(src).a[i_from].jointype as i32;
    if (sub_jointype & (JT_OUTER | JT_LTORJ)) != 0 {
        if p.src(sub_src).a.len() > 1 // (3a)
            || (p.s(sel).sel_flags & SF_Distinct) != 0 // (3d)
            || (sub_jointype & JT_RIGHT) != 0
        // (26)
        {
            return false;
        }
        is_outer_join = true;
    }

    if i_from > 0 && (p.src(sub_src).a[0].jointype as i32 & JT_LTORJ) != 0 {
        return false; // Restriction (27a)
    }

    // Condition (28) is blocked by the caller

    // Restriction (17): If the sub-query is a compound SELECT, then it must use only the
    // UNION ALL operator. And none of the simple select queries that make up the compound
    // SELECT are allowed to be aggregate or distinct queries.
    if p.s(sub).p_prior.nn() {
        if p.s(sub).p_order_by.nn() {
            return false; // Restriction (20)
        }
        if is_agg || (p.s(sel).sel_flags & SF_Distinct) != 0 || is_outer_join {
            return false; // (17d1), (17d2), or (17f)
        }
        let mut sub1 = sub;
        while sub1.nn() {
            let s1 = p.s(sub1);
            if (s1.sel_flags & (SF_Distinct | SF_Aggregate)) != 0 // (17b)
                || (s1.p_prior.nn() && s1.op != TK_ALL) // (17a)
                || p.src(s1.p_src).a.is_empty() // (17c)
                || s1.p_win.nn()
            // (17e)
            {
                return false;
            }
            if i_from > 0 && (p.src(s1.p_src).a[0].jointype as i32 & JT_LTORJ) != 0 {
                // Without this restriction, the JT_LTORJ flag would end up being omitted on
                // left-hand tables of the right join that is the first join term of a
                // compound-subquery arm.
                return false; // Restrictions (17g), (27b)
            }
            sub1 = s1.p_prior;
        }

        // Restriction (18).
        let ob = p.s(sel).p_order_by;
        if ob.nn() {
            for ii in 0..p.l(ob).a.len() {
                if p.l(ob).a[ii].i_order_by_col == 0 {
                    return false;
                }
            }
        }

        // Restriction (23)
        if p.s(sel).sel_flags & SF_Recursive != 0 {
            return false;
        }

        // Restriction (17h)
        if compound_has_different_affinities(p, sub) {
            return false;
        }

        if p.src(src).a.len() > 1 {
            if p.n_select > 500 {
                return false;
            }
            if p.db.optimization_disabled(SQLITE_FlttnUnionAll) {
                return false;
            }
            let mut m = vec![0i32; p.n_tab as usize + 1];
            m[0] = p.n_tab;
            a_csr_map = Some(m);
        }
    }

    // ***** If we reach this point, flattening is permitted. *****

    // (Authorize the subquery: the engine authorizes)

    // Delete the transient structures associated with the subquery
    let sub1 = sqlite3_subquery_detach(p, src, i_from);
    {
        let it = &mut p.srcm(src).a[i_from];
        it.z_name = None;
        it.z_alias = None;
    }

    // If the sub-query is a compound SELECT statement, then (by restrictions 17 and 18 above)
    // it must be a UNION ALL and the parent query must be of the form:
    //
    //     SELECT <expr-list> FROM (<sub-query>) <where-clause>
    //
    // followed by any ORDER BY, LIMIT and/or OFFSET clauses. This block creates N-1 copies of
    // the parent query without any ORDER BY, LIMIT or OFFSET clauses and joins them to the
    // left-hand-side of the original using UNION ALL operators. In this case the returned
    // statement is of the form:
    //
    //     SELECT <expr-list> FROM <1st sub-query> <where-clause>
    //       UNION ALL SELECT <expr-list> FROM <2nd sub-query> <where-clause>
    //       ...
    //       UNION ALL SELECT <expr-list> FROM <Nth sub-query> <where-clause>
    //       <order-by-clause> <limit-clause>
    //
    // We call this the "compound-subquery flattening".
    sub = p.s(sub).p_prior;
    while sub.nn() {
        let order_by = p.s(sel).p_order_by;
        let limit = p.s(sel).p_limit;
        let prior = p.s(sel).p_prior;
        let item_tab = p.src(src).a[i_from].p_s_tab;
        p.srcm(src).a[i_from].p_s_tab = TableId::NULL;
        {
            let s = p.sm(sel);
            s.p_order_by = ExprListId::NULL;
            s.p_prior = SelectId::NULL;
            s.p_limit = ExprId::NULL;
        }
        let new = sqlite3_select_dup(p, sel, 0);
        {
            let s = p.sm(sel);
            s.p_limit = limit;
            s.p_order_by = order_by;
            s.op = TK_ALL;
        }
        p.srcm(src).a[i_from].p_s_tab = item_tab;
        if new.is_null() {
            p.sm(sel).p_prior = prior;
        } else {
            p.n_select += 1;
            p.sm(new).sel_id = p.n_select as u32;
            if let Some(m) = a_csr_map.as_mut() {
                renumber_cursors(p, new, i_from as i32, m);
            }
            p.sm(new).p_prior = prior;
            if prior.nn() {
                p.sm(prior).p_next = new;
            }
            p.sm(new).p_next = sel;
            p.sm(sel).p_prior = new;
        }
        sub = p.s(sub).p_prior;
    }

    // Defer deleting the Table object associated with the subquery until code generation is
    // complete, since there may still exist Expr.pTab entries that refer to the subquery even
    // after flattening. (Nothing is deleted in the arenas.)
    p.srcm(src).a[i_from].p_s_tab = TableId::NULL;

    // The following loop runs once for each term in a compound-subquery flattening (as
    // described above). If we are doing a different kind of flattening - a flattening other
    // than a compound-subquery flattening - then this loop only runs once.
    //
    // This loop moves all of the FROM elements of the subquery into the the FROM clause of
    // the outer query. Before doing this, remember the cursor number for the original
    // subquery term. After all subquery terms have been moved, convert expressions that refer
    // to the subquery into references to the tables of the FROM clause of the subquery.
    sub = sub1;
    let mut parent = sel;
    while parent.nn() {
        let jointype = p.src(p.s(parent).p_src).a[i_from].jointype;
        let sub_src = p.s(sub).p_src; // FROM clause of subquery
        let n_sub_src = p.src(sub_src).a.len(); // Number of terms in subquery FROM clause
        src = p.s(parent).p_src; // FROM clause of the outer query

        // The subquery uses a single slot of the FROM clause of the outer query. If the
        // subquery has more than one element in its FROM clause, then expand the outer
        // query to make space for it to hold all elements of the subquery.
        //
        // Example:
        //
        //    SELECT * FROM tabA, (SELECT * FROM sub1, sub2), tabB;
        //
        // The outer query has 3 slots in its FROM clause. One slot of the outer query (the
        // middle slot) is used by the subquery. The next block of code will expand the
        // outer query FROM clause to 4 slots. The middle slot is expanded to two slots in
        // order to make space for the two elements in the FROM clause of the subquery.
        if n_sub_src > 1 {
            // sqlite3SrcListEnlarge(pParse, pSrc, nSubSrc-1, iFrom+1)
            if p.src(src).a.len() + n_sub_src - 1 >= crate::build::SQLITE_MAX_SRCLIST as usize {
                let msg = format!("too many FROM clause terms, max: {}", crate::build::SQLITE_MAX_SRCLIST);
                p.error_msg(msg);
                break;
            }
            for _ in 0..n_sub_src - 1 {
                p.srcm(src).a.insert(i_from + 1, SrcItem { i_cursor: -1, ..SrcItem::default() });
            }
        }

        // Transfer the FROM clause terms from the subquery into the outer query.
        i_new_parent = p.src(sub_src).a[0].i_cursor;
        for i in 0..n_sub_src {
            let mut moved = std::mem::take(&mut p.srcm(sub_src).a[i]);
            moved.jointype |= jointype & JT_LTORJ as u8;
            p.srcm(src).a[i + i_from] = moved;
        }
        p.srcm(src).a[i_from].jointype |= jointype;

        // Begin substituting subquery result set expressions for references to the iParent
        // cursor in the outer query.
        //
        // Example:
        //
        //   SELECT a+5, b*10 FROM (SELECT x*3 AS a, y+10 AS b FROM t1) WHERE a>b;
        //   \                     \_____________ subquery __________/          /
        //    \_____________________ outer query ______________________________/
        //
        // We look at every expression in the outer query and every place we see "a" we
        // substitute "x*3" and every place we see "b" we substitute "y+10".
        if p.s(sub).p_order_by.nn() {
            // At this point, any non-zero iOrderByCol values indicate that the ORDER BY
            // column expression is identical to the iOrderByCol'th expression returned by
            // SELECT statement pSub. Since these values do not necessarily correspond to
            // columns in SELECT statement pParent, zero them before transferring the ORDER
            // BY clause.
            //
            // Not doing this may cause an error if a subsequent call to this function
            // attempts to flatten a compound sub-query into pParent (the only way this can
            // happen is if pSub was a view).
            let order_by = p.s(sub).p_order_by;
            for it in p.lm(order_by).a.iter_mut() {
                it.i_order_by_col = 0;
            }
            p.sm(parent).p_order_by = order_by;
            p.sm(sub).p_order_by = ExprListId::NULL;
        }
        let wh = p.s(sub).p_where;
        p.sm(sub).p_where = ExprId::NULL;
        if is_outer_join {
            crate::select::sqlite3_set_join_expr(p, wh, i_new_parent, EP_OuterON);
        }
        if wh.nn() {
            if p.s(parent).p_where.nn() {
                let pw = p.s(parent).p_where;
                let n = sqlite3_p_expr(p, TK_AND, wh, pw);
                p.sm(parent).p_where = n;
            } else {
                p.sm(parent).p_where = wh;
            }
        }
        {
            let mut x = SubstContext {
                i_table: i_parent,
                i_new_table: i_new_parent,
                is_outer_join,
                n_sel_depth: 0,
                p_e_list: p.s(sub).p_e_list,
                p_c_list: find_leftmost_exprlist(p, sub),
            };
            subst_select(p, &mut x, parent, false);
        }

        // The flattened query is a compound if either the inner or the outer query is a
        // compound.
        let f = p.s(sub).sel_flags & SF_Compound;
        p.sm(parent).sel_flags |= f;

        //   SELECT ... FROM (SELECT ... FROM t1 LIMIT :1) LIMIT :2;
        //
        // One is tempted to try to add a and b to combine the limits. But this does not work
        // if either limit is negative.
        if p.s(sub).p_limit.nn() {
            let l = p.s(sub).p_limit;
            p.sm(parent).p_limit = l;
            p.sm(sub).p_limit = ExprId::NULL;
        }

        // Recompute the SrcItem.colUsed masks for the flattened tables.
        for i in 0..n_sub_src {
            recompute_columns_used(p, parent, src, i + i_from);
        }

        parent = p.s(parent).p_prior;
        sub = p.s(sub).p_prior;
    }
    let _ = i_new_parent;

    // Finally, delete what is left of the subquery and return success. (The AggInfo of the
    // subquery lives in the arena: sqlite3AggInfoPersistWalkerInit() has nothing to do.)
    true
}

// ---------------------------------------------------------------------------------------------
// Predicate push-down, unused columns of a subquery
// ---------------------------------------------------------------------------------------------

/// `pushDownWhereTerms(pParse, pSubq, pWhere, pSrcList, iSrc)`: make copies of relevant
/// WHERE clause terms of the outer query into the WHERE clause of subquery. Example:
///
///    SELECT * FROM (SELECT a AS x, c-d AS y FROM t1) WHERE x=5 AND y=10;
///
/// Transformed into:
///
///    SELECT * FROM (SELECT a AS x, c-d AS y FROM t1 WHERE a=5 AND c-d=10)
///     WHERE x=5 AND y=10;
///
/// The hope is that the terms added to the inner query will make it more efficient.
///
/// The restrictions (1) to (12) of `select.c` apply (the numbers are quoted next to the
/// tests). Return 0 if no changes are made and non-zero if one or more WHERE clause terms
/// were pushed down.
pub fn push_down_where_terms(p: &mut Parse, mut subq: SelectId, mut wh: ExprId, src_list: SrcListId, i_src: usize) -> i32 {
    let mut n_chng = 0;
    if wh.is_null() {
        return 0;
    }
    if p.s(subq).sel_flags & (SF_Recursive | SF_MultiPart) != 0 {
        return 0; // restrictions (2) and (11)
    }
    if p.src(src_list).a[i_src].jointype as i32 & (JT_LTORJ | JT_RIGHT) != 0 {
        return 0; // restrictions (10)
    }

    if p.s(subq).p_prior.nn() {
        let mut not_union_all = false;
        let mut s = subq;
        while s.nn() {
            let op = p.s(s).op;
            if op != TK_ALL && op != TK_SELECT {
                not_union_all = true;
            }
            if p.s(s).p_win.nn() {
                return 0; // restriction (6b)
            }
            s = p.s(s).p_prior;
        }
        if not_union_all {
            // If any of the compound arms are connected using UNION, INTERSECT, or EXCEPT,
            // then we must ensure that none of the columns use a non-BINARY collation.
            // tag-select-0450
            let mut s = subq;
            while s.nn() {
                let list = p.s(s).p_e_list;
                for ii in 0..p.l(list).a.len() {
                    let e = p.l(list).a[ii].p_expr;
                    let coll = sqlite3_expr_coll_seq(p, e);
                    if !sqlite3_is_binary(coll.as_deref()) {
                        return 0; // Restriction (8)
                    }
                }
                s = p.s(s).p_prior;
            }
        }
    } else if p.s(subq).p_win.nn() {
        // (pSubq->pWin && pSubq->pWin->pPartition==0: window functions are unsupported)
        return 0;
    }

    if p.s(subq).p_limit.nn() {
        return 0; // restriction (3)
    }
    while p.e(wh).op == TK_AND {
        let r = p.e(wh).p_right;
        n_chng += push_down_where_terms(p, subq, r, src_list, i_src);
        wh = p.e(wh).p_left;
    }

    if sqlite3_expr_is_single_table_constraint(p, wh, src_list, i_src, true) {
        n_chng += 1;
        p.sm(subq).sel_flags |= SF_PushDown;
        let i_cursor = p.src(src_list).a[i_src].i_cursor;
        while subq.nn() {
            let mut new = sqlite3_expr_dup(p, wh, 0);
            crate::select::unset_join_expr(p, new, -1, true);
            let mut x = SubstContext {
                i_table: i_cursor,
                i_new_table: i_cursor,
                is_outer_join: false,
                n_sel_depth: 0,
                p_e_list: p.s(subq).p_e_list,
                p_c_list: find_leftmost_exprlist(p, subq),
            };
            new = subst_expr(p, &mut x, new);
            if p.n_err == 0 && p.e(new).op == TK_IN && p.e(new).use_x_select() {
                let s1 = p.e(new).x_select();
                p.sm(s1).sel_flags |= SF_ClonedRhsIn;
                let s2 = p.e(wh).x_select();
                p.sm(s2).sel_flags |= SF_ClonedRhsIn;
            }
            if p.s(subq).sel_flags & SF_Aggregate != 0 {
                let h = p.s(subq).p_having;
                let n = sqlite3_expr_and(p, h, new);
                p.sm(subq).p_having = n;
            } else {
                let w = p.s(subq).p_where;
                let n = sqlite3_expr_and(p, w, new);
                p.sm(subq).p_where = n;
            }
            subq = p.s(subq).p_prior;
        }
    }
    n_chng
}

/// `disableUnusedSubqueryResultColumns(pItem)`: check to see if a subquery contains result-set
/// columns that are never used. If it does, change the value of those result-set columns to
/// NULL so that they do not cause unnecessary work to compute.
///
/// Return the number of column that were changed to NULL.
pub fn disable_unused_subquery_result_columns(p: &mut Parse, src: SrcListId, i: usize) -> i32 {
    let mut n_chng = 0;
    let (is_correlated, is_cte, tab, sub, mut col_used) = {
        let it = &p.src(src).a[i];
        (it.is_correlated, it.is_cte, it.p_s_tab, it.p_subq.as_ref().expect("subquery").p_select, it.col_used)
    };
    if is_correlated || is_cte {
        return 0;
    }
    let mut x = sub;
    while x.nn() {
        if (p.s(x).sel_flags & (SF_Distinct | SF_Aggregate)) != 0 {
            return 0;
        }
        if p.s(x).p_prior.nn() && p.s(x).op != TK_ALL {
            // This optimization does not work for compound subqueries that use UNION,
            // INTERSECT, or EXCEPT. Only UNION ALL.
            return 0;
        }
        if p.s(x).p_win.nn() {
            // This optimization does not work for subqueries that use window functions.
            return 0;
        }
        x = p.s(x).p_prior;
    }
    let ob = p.s(sub).p_order_by;
    if ob.nn() {
        for j in 0..p.l(ob).a.len() {
            let mut i_col = p.l(ob).a[j].i_order_by_col as i32;
            if i_col > 0 {
                i_col -= 1;
                col_used |= 1u64 << (if i_col >= BMS { BMS - 1 } else { i_col });
            }
        }
    }
    let n_col = p.tab(tab).a_col.len();
    for j in 0..n_col {
        let m: Bitmask = if (j as i32) < BMS - 1 { 1u64 << j } else { 1u64 << (BMS - 1) };
        if (m & col_used) != 0 {
            continue;
        }
        let mut x = sub;
        while x.nn() {
            let y = p.l(p.s(x).p_e_list).a[j].p_expr;
            if p.e(y).op != TK_NULL {
                p.em(y).op = TK_NULL;
                p.em(y).flags &= !(EP_Skip | EP_Unlikely);
                p.sm(x).sel_flags |= SF_PushDown;
                n_chng += 1;
            }
            x = p.s(x).p_prior;
        }
    }
    n_chng
}

// ---------------------------------------------------------------------------------------------
// count(*) of a compound view, EXISTS-to-JOIN
// ---------------------------------------------------------------------------------------------

/// `countOfViewOptimization(pParse, p)`: attempt to transform a query of the form
///
///    SELECT count(*) FROM (SELECT x FROM t1 UNION ALL SELECT y FROM t2)
///
/// Into this:
///
///    SELECT (SELECT count(*) FROM t1)+(SELECT count(*) FROM t2)
///
/// The transformation only works if all of the following are true:
///
///   *  The subquery is a UNION ALL of two or more terms
///   *  The subquery does not have a LIMIT clause
///   *  There is no WHERE or GROUP BY or HAVING clauses on the subqueries
///   *  The outer query is a simple count(*) with no WHERE clause or other extraneous
///      syntax.
///   *  None of the subqueries are DISTINCT (forumpost/a860f5fb2e 2025-03-10)
///
/// Return TRUE if the optimization is undertaken.
pub fn count_of_view_optimization(p: &mut Parse, sel: SelectId) -> bool {
    {
        let s = p.s(sel);
        if (s.sel_flags & SF_Aggregate) == 0 {
            return false; // This is an aggregate
        }
        if p.l(s.p_e_list).a.len() != 1 {
            return false; // Single result column
        }
        if s.p_where.nn() || s.p_having.nn() || s.p_group_by.nn() || s.p_order_by.nn() {
            return false;
        }
    }
    let count = p.l(p.s(sel).p_e_list).a[0].p_expr;
    if p.e(count).op != TK_AGG_FUNCTION {
        return false; // Result is an aggregate
    }
    if !p.e(count).tok().eq_ignore_ascii_case("count") {
        return false; // Is count()
    }
    if p.e(count).x_list().nn() {
        return false; // Must be count(*)
    }
    let src = p.s(sel).p_src;
    if p.src(src).a.len() != 1 {
        return false; // One table in FROM
    }
    if p.e(count).has(EP_WinFunc) {
        return false; // Not a window function
    }
    if !p.src(src).a[0].is_subquery {
        return false; // FROM is a subquery
    }
    if (p.s(sel).sel_flags & SF_Correlated) != 0 {
        return false; // Not a correlated subq
    }
    let mut sub = p.src(src).a[0].p_subq.as_ref().expect("subquery").p_select;
    if p.s(sub).p_prior.is_null() {
        return false; // Must be a compound
    }
    if p.s(sub).sel_flags & SF_CopyCte != 0 {
        return false; // Not a CTE
    }
    loop {
        let s = p.s(sub);
        if s.op != TK_ALL && s.p_prior.nn() {
            return false; // Must be UNION ALL
        }
        if s.p_where.nn() {
            return false; // No WHERE clause
        }
        if s.p_limit.nn() {
            return false; // No LIMIT clause
        }
        if s.sel_flags & (SF_Aggregate | SF_Distinct) != 0 {
            return false; // Not an aggregate nor DISTINCT
        }
        sub = s.p_prior; // Repeat over compound
        if sub.is_null() {
            break;
        }
    }

    // If we reach this point then it is OK to perform the transformation
    let mut e = ExprId::NULL;
    sub = sqlite3_subquery_detach(p, src, 0);
    let new_src = p.new_src(SrcList::default());
    p.sm(sel).p_src = new_src;
    while sub.nn() {
        let prior = p.s(sub).p_prior;
        {
            let s = p.sm(sub);
            s.p_prior = SelectId::NULL;
            s.p_next = SelectId::NULL;
            s.sel_flags |= SF_Aggregate;
            s.sel_flags &= !SF_Compound;
            s.n_select_row = 0;
        }
        let mut term = if prior.nn() { sqlite3_expr_dup(p, count, 0) } else { count };
        let el = sqlite3_expr_list_append(p, ExprListId::NULL, term);
        p.sm(sub).p_e_list = el;
        term = sqlite3_p_expr(p, TK_SELECT, ExprId::NULL, ExprId::NULL);
        sqlite3_p_expr_add_select(p, term, sub);
        if e.is_null() {
            e = term;
        } else {
            e = sqlite3_p_expr(p, TK_PLUS, term, e);
        }
        if p.n_err != 0 {
            e = ExprId::NULL;
        }
        sub = prior;
    }
    let el = p.s(sel).p_e_list;
    p.lm(el).a[0].p_expr = e;
    p.sm(sel).sel_flags &= !SF_Aggregate;
    true
}

/// `existsToJoin(pParse, p, pWhere)`: if any term of the form "EXISTS (SELECT ...)" is found
/// among the top-level AND-connected terms of the WHERE clause and the subquery is a simple
/// one-table query, the subquery is merged into the FROM clause of the outer query as a join
/// term flagged `fromExists` (at most one row of it is used per outer row).
pub fn exists_to_join(p: &mut Parse, sel: SelectId, wh: ExprId) {
    if p.n_err == 0
        && wh.nn()
        && !p.e(wh).has(EP_OuterON | EP_InnerON)
        && p.s(sel).p_src.nn()
        && (p.src(p.s(sel).p_src).a.len() as i32) < BMS
        && (p.s(sel).p_limit.is_null() || p.e(p.s(sel).p_limit).p_right.is_null())
    {
        if p.e(wh).op == TK_AND {
            let (l, r) = (p.e(wh).p_left, p.e(wh).p_right);
            exists_to_join(p, sel, l);
            exists_to_join(p, sel, r);
        } else if p.e(wh).op == TK_EXISTS {
            let sub = p.e(wh).x_select();
            let sub_where = p.s(sub).p_where;
            let sub_src = p.s(sub).p_src;
            if p.src(sub_src).a.len() == 1
                && (p.s(sub).sel_flags & SF_Aggregate) == 0
                && !p.src(sub_src).a[0].is_subquery
                && p.s(sub).p_limit.is_null()
                && p.s(sub).p_prior.is_null()
            {
                // Before combining the sub-select with the parent, renumber the cursor used
                // by the subselect. This is because the EXISTS expression might be a copy of
                // another EXISTS expression from somewhere else in the tree, and in this case
                // it is important that it use a unique cursor number.
                let mut a_csr_map = vec![0i32; p.n_tab as usize + 2];
                a_csr_map[0] = p.n_tab + 1;
                renumber_cursors(p, sub, -1, &mut a_csr_map);

                *p.em(wh) = Expr { op: TK_INTEGER, i_value: 1, flags: EP_IntValue, ..Expr::default() };
                p.srcm(sub_src).a[0].from_exists = true;
                let s0 = p.s(sel).p_src;
                let ns = crate::build::sqlite3_src_list_append_list(p, s0, sub_src);
                p.sm(sel).p_src = ns;
                if sub_where.nn() {
                    let w0 = p.s(sel).p_where;
                    let n = sqlite3_p_expr(p, TK_AND, w0, sub_where);
                    p.sm(sel).p_where = n;
                    p.sm(sub).p_where = ExprId::NULL;
                }
                p.sm(sub).p_src = SrcListId::NULL;
                let ps = p.s(sel).p_src;
                let last = p.src(ps).a.len() - 1;
                recompute_columns_used(p, sel, ps, last);
            }
        }
    }
}
