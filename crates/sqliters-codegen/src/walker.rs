//! `walker.c`: routines for walking the parser tree for an SQL statement.

use crate::consts::*;
use crate::sqliteint::*;

/// `sqlite3WalkExprNN(pWalker, pExpr)`: call the walker's expression callback for each node
/// of the expression tree, parents before children. Returns WRC_Abort if the walk was
/// aborted, else WRC_Continue.
pub fn sqlite3_walk_expr_nn(p: &mut Parse, w: &mut Walker, mut e: ExprId) -> i32 {
    loop {
        let cb = w.x_expr_callback.expect("walker without an expression callback");
        let rc = cb(p, w, e);
        if rc != 0 {
            return rc & WRC_Abort;
        }
        if !p.e(e).has(EP_TokenOnly | EP_Leaf) {
            let left = p.e(e).p_left;
            if left.nn() && sqlite3_walk_expr_nn(p, w, left) != 0 {
                return WRC_Abort;
            }
            let x = p.e(e);
            if x.p_right.nn() {
                e = x.p_right;
                continue;
            } else if x.use_x_select() {
                let s = x.x_select();
                if sqlite3_walk_select(p, w, s) != 0 {
                    return WRC_Abort;
                }
            } else {
                let l = x.x_list();
                if l.nn() && sqlite3_walk_expr_list(p, w, l) != 0 {
                    return WRC_Abort;
                }
                if p.e(e).has(EP_WinFunc) && walk_window_list(p, w, p.e(e).y_win, true) != 0 {
                    return WRC_Abort;
                }
            }
        }
        break;
    }
    WRC_Continue
}

/// `walkWindowList(pWalker, pList, bOneOnly)`: walk all expressions linked into the list of
/// Window objects passed as the second argument.
fn walk_window_list(p: &mut Parse, w: &mut Walker, list: WindowId, b_one_only: bool) -> i32 {
    let mut win = list;
    while win.nn() {
        let x = p.a_win[win.ix()].clone();
        if sqlite3_walk_expr_list(p, w, x.p_order_by) != 0 {
            return WRC_Abort;
        }
        if sqlite3_walk_expr_list(p, w, x.p_partition) != 0 {
            return WRC_Abort;
        }
        if sqlite3_walk_expr(p, w, x.p_filter) != 0 {
            return WRC_Abort;
        }
        if sqlite3_walk_expr(p, w, x.p_start) != 0 {
            return WRC_Abort;
        }
        if sqlite3_walk_expr(p, w, x.p_end) != 0 {
            return WRC_Abort;
        }
        if b_one_only {
            break;
        }
        win = x.p_next_win;
    }
    WRC_Continue
}

/// `sqlite3WalkExpr(pWalker, pExpr)`
pub fn sqlite3_walk_expr(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if e.nn() { sqlite3_walk_expr_nn(p, w, e) } else { WRC_Continue }
}

/// `sqlite3WalkExprList(pWalker, p)`: call sqlite3WalkExpr() for every expression in the list.
pub fn sqlite3_walk_expr_list(p: &mut Parse, w: &mut Walker, list: ExprListId) -> i32 {
    if list.nn() {
        let n = p.l(list).a.len();
        for i in 0..n {
            // (the list may be modified by the callbacks, so re-read each item)
            if i >= p.l(list).a.len() {
                break;
            }
            let e = p.l(list).a[i].p_expr;
            if sqlite3_walk_expr(p, w, e) != 0 {
                return WRC_Abort;
            }
        }
    }
    WRC_Continue
}

/// `sqlite3WalkSelectExpr(pWalker, p)`: walk all expressions associated with the SELECT
/// statement; do not invoke the SELECT callback on `sel`, but do invoke it on any subqueries.
pub fn sqlite3_walk_select_expr(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let l = p.s(sel).p_e_list;
    if sqlite3_walk_expr_list(p, w, l) != 0 {
        return WRC_Abort;
    }
    let e = p.s(sel).p_where;
    if sqlite3_walk_expr(p, w, e) != 0 {
        return WRC_Abort;
    }
    let l = p.s(sel).p_group_by;
    if sqlite3_walk_expr_list(p, w, l) != 0 {
        return WRC_Abort;
    }
    let e = p.s(sel).p_having;
    if sqlite3_walk_expr(p, w, e) != 0 {
        return WRC_Abort;
    }
    let l = p.s(sel).p_order_by;
    if sqlite3_walk_expr_list(p, w, l) != 0 {
        return WRC_Abort;
    }
    let e = p.s(sel).p_limit;
    if sqlite3_walk_expr(p, w, e) != 0 {
        return WRC_Abort;
    }
    // (p->pWinDefn: window definitions are not ported in step V1)
    WRC_Continue
}

/// `sqlite3WalkSelectFrom(pWalker, p)`: walk the parse trees associated with all subqueries in
/// the FROM clause of the SELECT statement.
pub fn sqlite3_walk_select_from(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let src = p.s(sel).p_src;
    if src.nn() {
        for i in 0..p.src(src).a.len() {
            let (sub, is_tab_func, func_arg) = {
                let it = &p.src(src).a[i];
                (it.p_subq.as_ref().map(|s| s.p_select), it.is_tab_func, it.p_func_arg)
            };
            if let Some(s) = sub
                && p.src(src).a[i].is_subquery
                && sqlite3_walk_select(p, w, s) != 0
            {
                return WRC_Abort;
            }
            if is_tab_func && sqlite3_walk_expr_list(p, w, func_arg) != 0 {
                return WRC_Abort;
            }
        }
    }
    WRC_Continue
}

/// `sqlite3WalkSelect(pWalker, p)`: call sqlite3WalkExpr() for every expression in the Select
/// statement and for all sub-selects on its FROM clause; also the compound-select chain
/// through pPrior. If the walker has no select callback, sub-selects are not visited.
pub fn sqlite3_walk_select(p: &mut Parse, w: &mut Walker, mut sel: SelectId) -> i32 {
    if sel.is_null() {
        return WRC_Continue;
    }
    let Some(cb) = w.x_select_callback else { return WRC_Continue };
    loop {
        let rc = cb(p, w, sel);
        if rc != 0 {
            return rc & WRC_Abort;
        }
        if sqlite3_walk_select_expr(p, w, sel) != 0 || sqlite3_walk_select_from(p, w, sel) != 0 {
            return WRC_Abort;
        }
        if let Some(cb2) = w.x_select_callback2 {
            cb2(p, w, sel);
        }
        sel = p.s(sel).p_prior;
        if sel.is_null() {
            break;
        }
    }
    WRC_Continue
}

/// `sqlite3WalkerDepthIncrease(pWalker, pSelect)`
pub fn sqlite3_walker_depth_increase(_p: &mut Parse, w: &mut Walker, _s: SelectId) -> i32 {
    w.walker_depth += 1;
    WRC_Continue
}
/// `sqlite3WalkerDepthDecrease(pWalker, pSelect)`
pub fn sqlite3_walker_depth_decrease(_p: &mut Parse, w: &mut Walker, _s: SelectId) {
    w.walker_depth -= 1;
}

/// `sqlite3ExprWalkNoop(NotUsed, NotUsed2)`
pub fn sqlite3_expr_walk_noop(_p: &mut Parse, _w: &mut Walker, _e: ExprId) -> i32 {
    WRC_Continue
}
/// `sqlite3SelectWalkNoop(NotUsed, NotUsed2)`
pub fn sqlite3_select_walk_noop(_p: &mut Parse, _w: &mut Walker, _s: SelectId) -> i32 {
    WRC_Continue
}
