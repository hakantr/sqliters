//! `window.c`: window functions. The SELECT rewrite (`sqlite3WindowRewrite`), the parser
//! helpers (`sqlite3WindowAlloc` / `Assemble` / `Chain` / `Attach`), the resolver helpers
//! (`sqlite3WindowUpdate` / `Link` / `Compare` / `UnlinkFromSelect`), the copies
//! (`sqlite3WindowDup` / `ListDup`) and the code generator (`sqlite3WindowCodeInit`,
//! `sqlite3WindowCodeStep` and its helpers). A FILTER clause of an ordinary aggregate is also
//! stored in a `Window` object, whose `eFrmType` is TK_FILTER.
//!
//! The built-in window functions themselves (their xStep / xValue / xInverse) are implemented by
//! the machine (`sqliters-vdbe`); the code generator recognizes them as C does by the identity of
//! the `FuncDef`: C compares `pFunc->zName` with the static name arrays of window.c, which only
//! the FuncDefs registered by `sqlite3WindowFunctions()` carry. Those are exactly the FuncDefs
//! with `SQLITE_FUNC_BUILTIN|SQLITE_FUNC_WINDOW` and that name (an application-defined function
//! never has SQLITE_FUNC_BUILTIN), see [`is_win_func`].
#![allow(clippy::too_many_arguments)]

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{FuncDef, KEYINFO_ORDER_BIGNULL, KEYINFO_ORDER_DESC, P4};
use sqliters_vdbe::vdbemem::Mem;

use crate::build::sqlite3_may_abort;
use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::select::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::walker::*;
use crate::whereint::WhereInfo;

// ---------------------------------------------------------------------------------------------
// Identity of the built-in window functions (C: `pFunc->zName==xxxName`, `xSFunc==noopStepFunc`)
// ---------------------------------------------------------------------------------------------

/// `pFunc->zName==<name>Name`: `f` is the FuncDef `sqlite3WindowFunctions()` registers under
/// `name` (row_number, dense_rank, rank, percent_rank, cume_dist, ntile, last_value, nth_value,
/// first_value, lead, lag).
pub fn is_win_func(f: &FuncDef, name: &str) -> bool {
    (f.func_flags & (SQLITE_FUNC_BUILTIN | SQLITE_FUNC_WINDOW)) == (SQLITE_FUNC_BUILTIN | SQLITE_FUNC_WINDOW)
        && f.name == name
}

/// `pFunc->xSFunc==noopStepFunc`: the `WINDOWFUNCNOOP` functions (lead and lag), implemented by
/// in-line VDBE code.
fn is_noop_step(f: &FuncDef) -> bool {
    is_win_func(f, "lead") || is_win_func(f, "lag")
}

fn wfunc(p: &Parse, w: WindowId) -> Arc<FuncDef> {
    p.w(w).p_w_func.clone().expect("window function without a FuncDef")
}

// ---------------------------------------------------------------------------------------------
// Named windows, frame coercion
// ---------------------------------------------------------------------------------------------

/// `windowFind(pParse, pList, zName)`
fn window_find(p: &mut Parse, list: WindowId, z_name: &str) -> WindowId {
    let mut w = list;
    while w.nn() {
        if p.w(w).z_name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(z_name)) {
            break;
        }
        w = p.w(w).p_next_win;
    }
    if w.is_null() {
        p.error_msg(format!("no such window: {z_name}"));
    }
    w
}

/// `sqlite3WindowUpdate(pParse, pList, pWin, pFunc)`: called immediately after resolving the
/// function name of a window function. Resolves a reference to a named window and coerces the
/// frame of the built-in window functions that require it.
pub fn sqlite3_window_update(p: &mut Parse, list: WindowId, win: WindowId, func: &Arc<FuncDef>) {
    if p.w(win).z_name.is_some() && p.w(win).e_frm_type == 0 {
        let name = p.w(win).z_name.clone().unwrap();
        let x = window_find(p, list, &name);
        if x.is_null() {
            return;
        }
        let src = p.w(x).clone();
        let part = sqlite3_expr_list_dup(p, src.p_partition, 0);
        let ob = sqlite3_expr_list_dup(p, src.p_order_by, 0);
        let st = sqlite3_expr_dup(p, src.p_start, 0);
        let en = sqlite3_expr_dup(p, src.p_end, 0);
        let w = p.wm(win);
        w.p_partition = part;
        w.p_order_by = ob;
        w.p_start = st;
        w.p_end = en;
        w.e_start = src.e_start;
        w.e_end = src.e_end;
        w.e_frm_type = src.e_frm_type;
        w.e_exclude = src.e_exclude;
    } else {
        sqlite3_window_chain(p, win, list);
    }
    let w = p.w(win);
    if w.e_frm_type == TK_RANGE
        && (w.p_start.nn() || w.p_end.nn())
        && (w.p_order_by.is_null() || p.l(w.p_order_by).n_expr() != 1)
    {
        p.error_msg("RANGE with offset PRECEDING/FOLLOWING requires one ORDER BY expression");
    } else if func.func_flags & SQLITE_FUNC_WINDOW != 0 {
        if p.w(win).p_filter.nn() {
            p.error_msg("FILTER clause may only be used with aggregate window functions");
        } else {
            const A_UP: [(&str, u8, u8, u8); 8] = [
                ("row_number", TK_ROWS, TK_UNBOUNDED, TK_CURRENT),
                ("dense_rank", TK_RANGE, TK_UNBOUNDED, TK_CURRENT),
                ("rank", TK_RANGE, TK_UNBOUNDED, TK_CURRENT),
                ("percent_rank", TK_GROUPS, TK_CURRENT, TK_UNBOUNDED),
                ("cume_dist", TK_GROUPS, TK_FOLLOWING, TK_UNBOUNDED),
                ("ntile", TK_ROWS, TK_CURRENT, TK_UNBOUNDED),
                ("lead", TK_ROWS, TK_UNBOUNDED, TK_UNBOUNDED),
                ("lag", TK_ROWS, TK_UNBOUNDED, TK_CURRENT),
            ];
            for (z_func, e_frm_type, e_start, e_end) in A_UP {
                if is_win_func(func, z_func) {
                    let one = if e_start == TK_FOLLOWING { sqlite3_expr_int32(p, 1) } else { ExprId::NULL };
                    let w = p.wm(win);
                    w.p_end = ExprId::NULL;
                    w.p_start = ExprId::NULL;
                    w.e_frm_type = e_frm_type;
                    w.e_start = e_start;
                    w.e_end = e_end;
                    w.e_exclude = 0;
                    if e_start == TK_FOLLOWING {
                        w.p_start = one;
                    }
                    break;
                }
            }
        }
    }
    p.wm(win).p_w_func = Some(func.clone());
}

// ---------------------------------------------------------------------------------------------
// sqlite3WindowRewrite
// ---------------------------------------------------------------------------------------------

/// `selectWindowRewriteExprCb(pWalker, pExpr)`: callback of selectWindowRewriteEList(). If
/// necessary, appends to the output expression-list and updates the expression in place.
/// Walker fields: `p_win` = WindowRewrite.pWin, `p_src_list` = .pSrc, `p_sub` = .pSub,
/// `p_tab` = .pTab, `p_select` = .pSubSelect.
fn select_window_rewrite_expr_cb(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    // If this function is being called from within a scalar sub-select that used by the
    // SELECT statement being processed, only process TK_COLUMN expressions that refer to it
    // (the outer SELECT). Do not process aggregates or window functions at all, as they
    // belong to the scalar sub-select.
    if w.p_select.nn() {
        if p.e(e).op != TK_COLUMN {
            return WRC_Continue;
        } else {
            let src = w.p_src_list;
            let n_src = p.src(src).a.len();
            let mut i = 0;
            while i < n_src {
                if p.e(e).i_table == p.src(src).a[i].i_cursor {
                    break;
                }
                i += 1;
            }
            if i == n_src {
                return WRC_Continue;
            }
        }
    }

    let op = p.e(e).op;
    let mut fall = false;
    if op == TK_FUNCTION {
        if !p.e(e).has(EP_WinFunc) {
            return WRC_Continue;
        }
        let mut win = w.p_win;
        while win.nn() {
            if p.e(e).y_win == win {
                debug_assert!(p.w(win).p_owner == e);
                return WRC_Prune;
            }
            win = p.w(win).p_next_win;
        }
        fall = true;
    }
    if fall || op == TK_IF_NULL_ROW || op == TK_AGG_FUNCTION || op == TK_COLUMN {
        let mut i_col: i32 = -1;
        if w.p_sub.nn() {
            for i in 0..p.l(w.p_sub).a.len() {
                let x = p.l(w.p_sub).a[i].p_expr;
                if sqlite3_expr_compare(p, false, x, e, -1) == 0 {
                    i_col = i as i32;
                    break;
                }
            }
        }
        if i_col < 0 {
            let dup = sqlite3_expr_dup(p, e, 0);
            if dup.nn() && p.e(dup).op == TK_AGG_FUNCTION {
                p.em(dup).op = TK_FUNCTION;
            }
            w.p_sub = sqlite3_expr_list_append(p, w.p_sub, dup);
        }
        if w.p_sub.nn() {
            let f = p.e(e).flags & (EP_Collate | EP_OuterON | EP_InnerON);
            let i_join = p.e(e).w;
            // sqlite3ExprDelete(pParse->db, pExpr) (with EP_Static): the Window objects of the
            // deleted tree are unlinked from their SELECT
            expr_delete_unlink_windows(p, e);
            let n = p.l(w.p_sub).n_expr();
            let i_eph = p.w(w.p_win).i_eph_csr;
            *p.em(e) = Expr {
                op: TK_COLUMN,
                i_column: (if i_col < 0 { n - 1 } else { i_col }) as i16,
                i_table: i_eph,
                y_tab: w.p_tab,
                flags: f,
                w: if f & (EP_OuterON | EP_InnerON) != 0 { i_join } else { 0 },
                ..Expr::default()
            };
        }
    }
    WRC_Continue
}

/// The part of `sqlite3ExprDelete()` that is visible in the arena: every Window object of the
/// deleted tree is unlinked from the `Select.pWin` list it is in (`sqlite3WindowDelete` ->
/// `sqlite3WindowUnlinkFromSelect`). The sub-selects of the tree are deleted with it; the
/// windows linked to them belong to the deleted SELECT and nobody reads them again.
pub fn expr_delete_unlink_windows(p: &mut Parse, e: ExprId) {
    if e.is_null() {
        return;
    }
    let x = p.e(e).clone();
    if x.has(EP_TokenOnly | EP_Leaf) {
        return;
    }
    expr_delete_unlink_windows(p, x.p_left);
    expr_delete_unlink_windows(p, x.p_right);
    if !x.use_x_select() && x.x_list().nn() {
        for i in 0..p.l(x.x_list()).a.len() {
            let a = p.l(x.x_list()).a[i].p_expr;
            expr_delete_unlink_windows(p, a);
        }
    }
    if x.has(EP_WinFunc) && x.y_win.nn() {
        sqlite3_window_unlink_from_select(p, x.y_win);
    }
}

/// `selectWindowRewriteSelectCb(pWalker, pSelect)`
fn select_window_rewrite_select_cb(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let save = w.p_select;
    if save == sel {
        WRC_Continue
    } else {
        w.p_select = sel;
        sqlite3_walk_select(p, w, sel);
        w.p_select = save;
        WRC_Prune
    }
}

/// `selectWindowRewriteEList(pParse, pWin, pSrc, pEList, pTab, ppSub)`: iterate through each
/// expression in expression-list pEList. For each TK_COLUMN, aggregate function, or window
/// function with a Window object that is not a member of the Window list passed as the second
/// argument, append the node to output expression-list (*ppSub) and replace it with a
/// TK_COLUMN that reads the (N-1)th element of table pWin->iEphCsr.
fn select_window_rewrite_e_list(
    p: &mut Parse,
    win: WindowId,
    src: SrcListId,
    e_list: ExprListId,
    tab: TableId,
    pp_sub: &mut ExprListId,
) {
    let mut w = Walker {
        x_expr_callback: Some(select_window_rewrite_expr_cb),
        x_select_callback: Some(select_window_rewrite_select_cb),
        p_sub: *pp_sub,
        p_win: win,
        p_src_list: src,
        p_tab: tab,
        p_select: SelectId::NULL,
        ..Walker::default()
    };
    sqlite3_walk_expr_list(p, &mut w, e_list);
    *pp_sub = w.p_sub;
}

/// `exprListAppendList(pParse, pList, pAppend, bIntToNull)`: append a copy of each expression
/// in expression-list pAppend to expression list pList. Return a pointer to the result list.
fn expr_list_append_list(p: &mut Parse, mut list: ExprListId, append: ExprListId, b_int_to_null: bool) -> ExprListId {
    if append.nn() {
        let n_init = if list.nn() { p.l(list).n_expr() } else { 0 };
        for i in 0..p.l(append).a.len() {
            let src = p.l(append).a[i].p_expr;
            let dup = sqlite3_expr_dup(p, src, 0);
            if b_int_to_null {
                let sub = sqlite3_expr_skip_collate_and_likely(p, dup);
                if sqlite3_expr_is_integer(p, sub, false, false).is_some() {
                    let x = p.em(sub);
                    x.op = TK_NULL;
                    x.flags &= !(EP_IntValue | EP_IsTrue | EP_IsFalse);
                    x.z_token = None;
                }
            }
            list = sqlite3_expr_list_append(p, list, dup);
            if list.nn() {
                let sf = p.l(append).a[i].sort_flags;
                p.lm(list).a[n_init as usize + i].sort_flags = sf;
            }
        }
    }
    list
}

/// `sqlite3WindowExtraAggFuncDepth(pWalker, pExpr)`: when rewriting a query, if the new
/// subquery in the FROM clause contains TK_AGG_FUNCTION nodes that refer to an outer query,
/// then the Expr->op2 values of those nodes are increased due to the extra subquery layer.
fn sqlite3_window_extra_agg_func_depth(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).op == TK_AGG_FUNCTION && p.e(e).op2 as i32 >= w.walker_depth {
        p.em(e).op2 += 1;
    }
    WRC_Continue
}

/// `disallowAggregatesInOrderByCb(pWalker, pExpr)`
fn disallow_aggregates_in_order_by_cb(p: &mut Parse, _w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).op == TK_AGG_FUNCTION && p.e(e).p_agg_info.is_null() {
        let msg = format!("misuse of aggregate: {}()", p.e(e).tok());
        p.error_msg(msg);
    }
    WRC_Continue
}

/// `agginfoPersistExprCb(pWalker, pExpr)` (expr.c): an AggInfo entry that refers to this node
/// is given its own copy of the node, because the node is about to be changed in place.
fn agginfo_persist_expr_cb(p: &mut Parse, _w: &mut Walker, e: ExprId) -> i32 {
    let x = p.e(e);
    if !x.has(EP_TokenOnly | EP_Reduced) && x.p_agg_info.nn() {
        let agg = x.p_agg_info;
        let i_agg = x.i_agg as i32;
        if x.op != TK_AGG_FUNCTION {
            if i_agg < p.agg(agg).n_column() && p.agg(agg).a_col[i_agg as usize].p_c_expr == e {
                let dup = sqlite3_expr_dup(p, e, 0);
                p.aggm(agg).a_col[i_agg as usize].p_c_expr = dup;
            }
        } else if i_agg < p.agg(agg).n_func() && p.agg(agg).a_func[i_agg as usize].p_f_expr == e {
            let dup = sqlite3_expr_dup(p, e, 0);
            p.aggm(agg).a_func[i_agg as usize].p_f_expr = dup;
        }
    }
    WRC_Continue
}

/// `sqlite3WindowRewrite(pParse, p)`: if the SELECT statement passed as the second argument
/// does not invoke any SQL window functions, this function is a no-op. Otherwise, it rewrites
/// the SELECT statement so that window function xStep functions are invoked in the correct
/// order as described under "SELECT REWRITING" at the top of window.c.
pub fn sqlite3_window_rewrite(p: &mut Parse, sel: SelectId) -> i32 {
    let mut rc = SQLITE_OK;
    if p.s(sel).p_win.nn() && p.s(sel).p_prior.is_null() && (p.s(sel).sel_flags & SF_WinRewrite) == 0 && !p.in_rename_object()
    {
        sqlite3_get_vdbe(p);
        let src = p.s(sel).p_src;
        let wh = p.s(sel).p_where;
        let group_by = p.s(sel).p_group_by;
        let having = p.s(sel).p_having;
        let m_win = p.s(sel).p_win; // Main window object
        let mut sublist = ExprListId::NULL; // Expression list for sub-query
        let sel_flags = p.s(sel).sel_flags;

        let tab = p.new_tab(Table::default());
        {
            // sqlite3AggInfoPersistWalkerInit(&w, pParse); sqlite3WalkSelect(&w, p);
            let mut w = Walker {
                x_expr_callback: Some(agginfo_persist_expr_cb),
                x_select_callback: Some(sqlite3_select_walk_noop),
                ..Walker::default()
            };
            sqlite3_walk_select(p, &mut w, sel);
            if (p.s(sel).sel_flags & SF_Aggregate) == 0 {
                w.x_expr_callback = Some(disallow_aggregates_in_order_by_cb);
                w.x_select_callback = None;
                let ob = p.s(sel).p_order_by;
                sqlite3_walk_expr_list(p, &mut w, ob);
            }
        }

        {
            let s = p.sm(sel);
            s.p_src = SrcListId::NULL;
            s.p_where = ExprId::NULL;
            s.p_group_by = ExprListId::NULL;
            s.p_having = ExprId::NULL;
            s.sel_flags &= !SF_Aggregate;
            s.sel_flags |= SF_WinRewrite;
        }

        // Create the ORDER BY clause for the sub-select. This is the concatenation of the
        // window PARTITION and ORDER BY clauses. Then, if this makes it redundant, remove the
        // ORDER BY from the parent SELECT.
        let (part, ob) = (p.w(m_win).p_partition, p.w(m_win).p_order_by);
        let mut sort = expr_list_append_list(p, ExprListId::NULL, part, true);
        sort = expr_list_append_list(p, sort, ob, true);
        let p_ob = p.s(sel).p_order_by;
        if sort.nn() && p_ob.nn() && p.l(p_ob).n_expr() <= p.l(sort).n_expr() {
            let n = p.l(p_ob).a.len();
            let saved = p.lm(sort).a.split_off(n);
            let same = sqlite3_expr_list_compare(p, sort, p_ob, -1) == 0;
            p.lm(sort).a.extend(saved);
            if same {
                p.sm(sel).p_order_by = ExprListId::NULL;
            }
        }

        // Assign a cursor number for the ephemeral table used to buffer rows. The
        // OpenEphemeral instruction is coded later, after it is known how many columns the
        // table will have.
        p.wm(m_win).i_eph_csr = p.n_tab;
        p.n_tab += 1;
        p.n_tab += 3;

        let el = p.s(sel).p_e_list;
        select_window_rewrite_e_list(p, m_win, src, el, tab, &mut sublist);
        let ob = p.s(sel).p_order_by;
        select_window_rewrite_e_list(p, m_win, src, ob, tab, &mut sublist);
        p.wm(m_win).n_buffer_col = if sublist.nn() { p.l(sublist).n_expr() } else { 0 };

        // Append the PARTITION BY and ORDER BY expressions to the to the sub-select expression
        // list. They are required to figure out where boundaries for partitions and sets of
        // peer rows lie.
        let (part, ob) = (p.w(m_win).p_partition, p.w(m_win).p_order_by);
        sublist = expr_list_append_list(p, sublist, part, false);
        sublist = expr_list_append_list(p, sublist, ob, false);

        // Append the arguments passed to each window function to the sub-select expression
        // list. Also allocate two registers for each window function - one for the
        // accumulator, another for interim results.
        let mut win = m_win;
        while win.nn() {
            let owner = p.w(win).p_owner;
            let args = p.e(owner).x_list();
            let func = wfunc(p, win);
            if func.func_flags & SQLITE_SUBTYPE as u32 != 0 {
                select_window_rewrite_e_list(p, m_win, src, args, tab, &mut sublist);
                let n = if sublist.nn() { p.l(sublist).n_expr() } else { 0 };
                p.wm(win).i_arg_col = n;
                p.wm(win).b_expr_args = 1;
            } else {
                let n = if sublist.nn() { p.l(sublist).n_expr() } else { 0 };
                p.wm(win).i_arg_col = n;
                sublist = expr_list_append_list(p, sublist, args, false);
            }
            let filter = p.w(win).p_filter;
            if filter.nn() {
                let f = sqlite3_expr_dup(p, filter, 0);
                sublist = sqlite3_expr_list_append(p, sublist, f);
            }
            p.n_mem += 1;
            p.wm(win).reg_accum = p.n_mem;
            p.n_mem += 1;
            p.wm(win).reg_result = p.n_mem;
            let r = p.w(win).reg_accum;
            p.v.add_op2(OP_Null, 0, r);
            win = p.w(win).p_next_win;
        }

        // If there is no ORDER BY or PARTITION BY clause, and the window function accepts zero
        // arguments, and there are no other columns selected (e.g. "SELECT row_number() OVER ()
        // FROM t1"), it is possible that pSublist is still NULL here. Add a constant expression
        // here to keep everything legal in this case.
        if sublist.is_null() {
            let z = sqlite3_expr_int32(p, 0);
            sublist = sqlite3_expr_list_append(p, ExprListId::NULL, z);
        }

        let sub = sqlite3_select_new(p, sublist, src, wh, group_by, having, sort, 0, ExprId::NULL);
        let new_src = crate::build::sqlite3_src_list_append(p, SrcListId::NULL, None, None);
        p.sm(sel).p_src = new_src;
        if new_src.nn() && crate::build::sqlite3_src_item_attach_subquery(p, new_src, 0, sub, false) {
            p.srcm(new_src).a[0].is_correlated = true;
            sqlite3_src_list_assign_cursors(p, new_src);
            p.sm(sub).sel_flags |= SF_Expanded | SF_OrderByReqd;
            let tab2 = sqlite3_result_set_of_select(p, sub, SQLITE_AFF_NONE);
            p.sm(sub).sel_flags |= sel_flags & SF_Aggregate;
            if tab2.is_null() {
                // Might actually be some other kind of error, but in that case pParse->nErr
                // will be set, so if SQLITE_NOMEM is set, we will get the correct error message
                // regardless.
                rc = SQLITE_NOMEM;
            } else {
                let mut t = p.tab(tab2).clone();
                t.tab_flags |= TF_Ephemeral;
                *p.tabm(tab) = t;
                p.srcm(new_src).a[0].p_s_tab = tab;
                let mut w = Walker {
                    x_expr_callback: Some(sqlite3_window_extra_agg_func_depth),
                    x_select_callback: Some(sqlite3_walker_depth_increase),
                    x_select_callback2: Some(sqlite3_walker_depth_decrease),
                    ..Walker::default()
                };
                sqlite3_walk_select(p, &mut w, sub);
            }
        }
    }
    rc
}

// ---------------------------------------------------------------------------------------------
// Window object list maintenance
// ---------------------------------------------------------------------------------------------

/// `sqlite3WindowUnlinkFromSelect(p)`: unlink the Window object from the Select to which it is
/// attached, if it is attached.
pub fn sqlite3_window_unlink_from_select(p: &mut Parse, win: WindowId) {
    let pp = p.w(win).pp_this;
    if pp != WinPpThis::None {
        let next = p.w(win).p_next_win;
        match pp {
            WinPpThis::Select(s) => p.sm(s).p_win = next,
            WinPpThis::Win(w) => p.wm(w).p_next_win = next,
            WinPpThis::None => {}
        }
        if next.nn() {
            p.wm(next).pp_this = pp;
        }
        p.wm(win).pp_this = WinPpThis::None;
    }
}

/// `sqlite3WindowOffsetExpr(pParse, pExpr)`: the argument expression is an PRECEDING or
/// FOLLOWING offset. If the value is not a constant, change it to NULL.
fn sqlite3_window_offset_expr(p: &mut Parse, e: ExprId) -> ExprId {
    if sqlite3_expr_is_constant_no_parse(p, e) == 0 {
        // (IN_RENAME_OBJECT: sqlite3RenameExprUnmap() - rename is not ported)
        return sqlite3_expr_alloc(p, TK_NULL, None, 0);
    }
    e
}

/// `sqlite3WindowAlloc(pParse, eType, eStart, pStart, eEnd, pEnd, eExclude)`: allocate and
/// return a new Window object describing a Window Definition.
pub fn sqlite3_window_alloc(
    p: &mut Parse,
    mut e_type: u8,
    e_start: u8,
    p_start: ExprId,
    e_end: u8,
    p_end: ExprId,
    mut e_exclude: u8,
) -> WindowId {
    let mut b_implicit_frame = 0;
    if e_type == 0 {
        b_implicit_frame = 1;
        e_type = TK_RANGE;
    }

    // Additionally, the starting boundary type may not occur earlier in the following list
    // than the ending boundary type:
    //
    //   UNBOUNDED PRECEDING
    //   <expr> PRECEDING
    //   CURRENT ROW
    //   <expr> FOLLOWING
    //   UNBOUNDED FOLLOWING
    //
    // The parser ensures that "UNBOUNDED PRECEDING" cannot be used as an ending boundary, and
    // than "UNBOUNDED FOLLOWING" cannot be used as a starting frame boundary.
    if (e_start == TK_CURRENT && e_end == TK_PRECEDING)
        || (e_start == TK_FOLLOWING && (e_end == TK_PRECEDING || e_end == TK_CURRENT))
    {
        p.error_msg("unsupported frame specification");
        return WindowId::NULL;
    }

    if e_exclude == 0 && p.db.optimization_disabled(SQLITE_WindowFunc) {
        e_exclude = TK_NO;
    }
    let end = if p_end.nn() { sqlite3_window_offset_expr(p, p_end) } else { ExprId::NULL };
    let start = if p_start.nn() { sqlite3_window_offset_expr(p, p_start) } else { ExprId::NULL };
    p.new_win(Window {
        e_frm_type: e_type,
        e_start,
        e_end,
        e_exclude,
        b_implicit_frame,
        p_end: end,
        p_start: start,
        ..Window::default()
    })
}

/// `sqlite3WindowAssemble(pParse, pWin, pPartition, pOrderBy, pBase)`: attach PARTITION and
/// ORDER BY clauses pPartition and pOrderBy to window pWin. Also, if parameter pBase is not
/// NULL, set pWin->zBase to the equivalent nul-terminated string.
pub fn sqlite3_window_assemble(
    p: &mut Parse,
    win: WindowId,
    partition: ExprListId,
    order_by: ExprListId,
    base: Option<&Token>,
) -> WindowId {
    if win.nn() {
        let z_base = base.map(|t| p.tok_str(t).to_owned());
        let w = p.wm(win);
        w.p_partition = partition;
        w.p_order_by = order_by;
        if z_base.is_some() {
            w.z_base = z_base;
        }
    }
    win
}

/// `sqlite3WindowChain(pParse, pWin, pList)`: Window *pWin has just been created from a WINDOW
/// clause. Token pBase is the base window. Earlier windows from the same WINDOW clause are
/// stored in the linked list starting at pWin->pNextWin. This function either updates *pWin
/// according to the base specification, or else leaves an error in pParse.
pub fn sqlite3_window_chain(p: &mut Parse, win: WindowId, list: WindowId) {
    if let Some(z_base) = p.w(win).z_base.clone() {
        let exist = window_find(p, list, &z_base);
        if exist.nn() {
            let z_err = if p.w(win).p_partition.nn() {
                Some("PARTITION clause")
            } else if p.w(exist).p_order_by.nn() && p.w(win).p_order_by.nn() {
                Some("ORDER BY clause")
            } else if p.w(exist).b_implicit_frame == 0 {
                Some("frame specification")
            } else {
                None
            };
            if let Some(z_err) = z_err {
                p.error_msg(format!("cannot override {z_err} of window: {z_base}"));
            } else {
                let part = p.w(exist).p_partition;
                let part = sqlite3_expr_list_dup(p, part, 0);
                p.wm(win).p_partition = part;
                let ob = p.w(exist).p_order_by;
                if ob.nn() {
                    let ob = sqlite3_expr_list_dup(p, ob, 0);
                    p.wm(win).p_order_by = ob;
                }
                p.wm(win).z_base = None;
            }
        }
    }
}

/// `sqlite3WindowAttach(pParse, p, pWin)`: attach window object pWin to expression p.
pub fn sqlite3_window_attach(p: &mut Parse, e: ExprId, win: WindowId) {
    if e.nn() {
        p.em(e).y_win = win;
        p.em(e).flags |= EP_WinFunc | EP_FullSize;
        if win.nn() {
            p.wm(win).p_owner = e;
            if p.e(e).has(EP_Distinct) && p.w(win).e_frm_type != TK_FILTER {
                p.error_msg("DISTINCT is not supported for window functions");
            }
        }
    }
    // else: sqlite3WindowDelete(pParse->db, pWin)
}

/// `sqlite3WindowLink(pSel, pWin)`: possibly link window pWin into the list at pSel->pWin
/// (window functions to be processed as part of SELECT statement pSel). The window is linked in
/// if either (a) there are no other windows already linked to this SELECT, or (b) the windows
/// already linked use a compatible window frame.
pub fn sqlite3_window_link(p: &mut Parse, sel: SelectId, win: WindowId) {
    if sel.nn() {
        let head = p.s(sel).p_win;
        if head.is_null() || sqlite3_window_compare(p, false, head, win, false) == 0 {
            p.wm(win).p_next_win = head;
            if head.nn() {
                p.wm(head).pp_this = WinPpThis::Win(win);
            }
            p.sm(sel).p_win = win;
            p.wm(win).pp_this = WinPpThis::Select(sel);
        } else {
            let (a, b) = (p.w(win).p_partition, p.w(head).p_partition);
            if sqlite3_expr_list_compare(p, a, b, -1) != 0 {
                p.sm(sel).sel_flags |= SF_MultiPart;
            }
        }
    }
}

/// `sqlite3WindowCompare(pParse, p1, p2, bFilter)`: return 0 if the two window objects are
/// identical, 1 if they are different, or 2 if it cannot be determined if the objects are
/// identical or not. Identical window objects can be processed in a single scan.
pub fn sqlite3_window_compare(p: &Parse, use_parse: bool, w1: WindowId, w2: WindowId, b_filter: bool) -> i32 {
    if w1.is_null() || w2.is_null() {
        return 1;
    }
    let (a, b) = (&p.a_win[w1.ix()], &p.a_win[w2.ix()]);
    if a.e_frm_type != b.e_frm_type || a.e_start != b.e_start || a.e_end != b.e_end || a.e_exclude != b.e_exclude {
        return 1;
    }
    if sqlite3_expr_compare(p, use_parse, a.p_start, b.p_start, -1) != 0 {
        return 1;
    }
    if sqlite3_expr_compare(p, use_parse, a.p_end, b.p_end, -1) != 0 {
        return 1;
    }
    let res = sqlite3_expr_list_compare(p, a.p_partition, b.p_partition, -1);
    if res != 0 {
        return res;
    }
    let res = sqlite3_expr_list_compare(p, a.p_order_by, b.p_order_by, -1);
    if res != 0 {
        return res;
    }
    if b_filter {
        let res = sqlite3_expr_compare(p, use_parse, a.p_filter, b.p_filter, -1);
        if res != 0 {
            return res;
        }
    }
    0
}

/// `sqlite3WindowDup(db, pOwner, p)`: allocate and return a duplicate of the Window object
/// indicated by the third argument. Set the Window.pOwner field of the new object to pOwner.
pub fn sqlite3_window_dup(p: &mut Parse, owner: ExprId, win: WindowId) -> WindowId {
    if win.is_null() {
        return WindowId::NULL;
    }
    let old = p.a_win[win.ix()].clone();
    let new = Window {
        z_name: old.z_name.clone(),
        z_base: old.z_base.clone(),
        p_filter: sqlite3_expr_dup(p, old.p_filter, 0),
        p_w_func: old.p_w_func.clone(),
        p_partition: sqlite3_expr_list_dup(p, old.p_partition, 0),
        p_order_by: sqlite3_expr_list_dup(p, old.p_order_by, 0),
        e_frm_type: old.e_frm_type,
        e_end: old.e_end,
        e_start: old.e_start,
        e_exclude: old.e_exclude,
        reg_result: old.reg_result,
        reg_accum: old.reg_accum,
        i_arg_col: old.i_arg_col,
        i_eph_csr: old.i_eph_csr,
        b_expr_args: old.b_expr_args,
        p_start: sqlite3_expr_dup(p, old.p_start, 0),
        p_end: sqlite3_expr_dup(p, old.p_end, 0),
        p_owner: owner,
        b_implicit_frame: old.b_implicit_frame,
        ..Window::default()
    };
    p.new_win(new)
}

/// `sqlite3WindowListDup(db, p)`: return a copy of the linked list of Window objects passed as
/// the second argument.
pub fn sqlite3_window_list_dup(p: &mut Parse, list: WindowId) -> WindowId {
    let mut ret = WindowId::NULL;
    let mut prev = WindowId::NULL;
    let mut win = list;
    while win.nn() {
        let n = sqlite3_window_dup(p, ExprId::NULL, win);
        if prev.nn() {
            p.wm(prev).p_next_win = n;
        } else {
            ret = n;
        }
        prev = n;
        win = p.w(win).p_next_win;
    }
    ret
}

// ---------------------------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------------------------

/// `sqlite3WindowCodeInit(pParse, pSelect)`: called by code in select.c before it calls
/// sqlite3WhereBegin() to begin iterating through the sub-query results. It is used to allocate
/// and initialize registers and cursors used by sqlite3WindowCodeStep().
pub fn sqlite3_window_code_init(p: &mut Parse, sel: SelectId) {
    let src = p.s(sel).p_src;
    let sub = p.src(src).a[0].p_subq.as_ref().expect("window sub-query").p_select;
    let n_eph_expr = p.l(p.s(sub).p_e_list).n_expr();
    let m_win = p.s(sel).p_win;
    sqlite3_get_vdbe(p);

    let eph = p.w(m_win).i_eph_csr;
    p.v.add_op2(OP_OpenEphemeral, eph, n_eph_expr);
    p.v.add_op2(OP_OpenDup, eph + 1, eph);
    p.v.add_op2(OP_OpenDup, eph + 2, eph);
    p.v.add_op2(OP_OpenDup, eph + 3, eph);

    // Allocate registers to use for PARTITION BY values, if any. Initialize said registers to
    // NULL.
    let part = p.w(m_win).p_partition;
    if part.nn() {
        let n_expr = p.l(part).n_expr();
        let reg_part = p.n_mem + 1;
        p.wm(m_win).reg_part = reg_part;
        p.n_mem += n_expr;
        p.v.add_op3(OP_Null, 0, reg_part, reg_part + n_expr - 1);
    }

    p.n_mem += 1;
    let reg_one = p.n_mem;
    p.wm(m_win).reg_one = reg_one;
    p.v.add_op2(OP_Integer, 1, reg_one);

    if p.w(m_win).e_exclude != 0 {
        p.n_mem += 1;
        let rs = p.n_mem;
        p.n_mem += 1;
        let re = p.n_mem;
        let csr = p.n_tab;
        p.n_tab += 1;
        {
            let w = p.wm(m_win);
            w.reg_start_rowid = rs;
            w.reg_end_rowid = re;
            w.csr_app = csr;
        }
        p.v.add_op2(OP_Integer, 1, rs);
        p.v.add_op2(OP_Integer, 0, re);
        p.v.add_op2(OP_OpenDup, csr, eph);
        return;
    }

    let mut win = m_win;
    while win.nn() {
        let f = wfunc(p, win);
        if (f.func_flags & SQLITE_FUNC_MINMAX) != 0 && p.w(win).e_start != TK_UNBOUNDED {
            // The inline versions of min() and max() require a single ephemeral table and 3
            // registers. The registers are used as follows:
            //
            //   regApp+0: slot to copy min()/max() argument to for MakeRecord
            //   regApp+1: integer value used to ensure keys are unique
            //   regApp+2: output of MakeRecord
            let list = p.e(p.w(win).p_owner).x_list();
            let mut key_info = sqlite3_key_info_from_expr_list(p, list, 0, 0);
            let csr = p.n_tab;
            p.n_tab += 1;
            let reg_app = p.n_mem + 1;
            p.n_mem += 3;
            {
                let w = p.wm(win);
                w.csr_app = csr;
                w.reg_app = reg_app;
            }
            if f.name.as_bytes()[1] == b'i' {
                key_info.a_sort_flags[0] = KEYINFO_ORDER_DESC;
            }
            p.v.add_op2(OP_OpenEphemeral, csr, 2);
            p.v.append_p4(P4::KeyInfo(Arc::new(key_info)));
            p.v.add_op2(OP_Integer, 0, reg_app + 1);
        } else if is_win_func(&f, "nth_value") || is_win_func(&f, "first_value") {
            // Allocate two registers at pWin->regApp. These will be used to store the start
            // and end index of the current frame.
            let reg_app = p.n_mem + 1;
            let csr = p.n_tab;
            p.n_tab += 1;
            p.n_mem += 2;
            {
                let w = p.wm(win);
                w.reg_app = reg_app;
                w.csr_app = csr;
            }
            p.v.add_op2(OP_OpenDup, csr, eph);
        } else if is_win_func(&f, "lead") || is_win_func(&f, "lag") {
            let csr = p.n_tab;
            p.n_tab += 1;
            p.wm(win).csr_app = csr;
            p.v.add_op2(OP_OpenDup, csr, eph);
        }
        win = p.w(win).p_next_win;
    }
}

const WINDOW_STARTING_INT: i32 = 0;
const WINDOW_ENDING_INT: i32 = 1;
const WINDOW_NTH_VALUE_INT: i32 = 2;
const WINDOW_STARTING_NUM: i32 = 3;
#[allow(dead_code)]
const WINDOW_ENDING_NUM: i32 = 4;

/// `windowCheckValue(pParse, reg, eCond)`: a "PRECEDING <expr>" (eCond==0) or "FOLLOWING
/// <expr>" (eCond==1) or the value of the second argument to nth_value() (eCond==2) has just
/// been evaluated and the result left in register reg. This function generates VM code to
/// check that the value is a non-negative integer and throws an exception if it is not.
fn window_check_value(p: &mut Parse, reg: i32, e_cond: i32) {
    const AZ_ERR: [&str; 5] = [
        "frame starting offset must be a non-negative integer",
        "frame ending offset must be a non-negative integer",
        "second argument to nth_value must be a positive integer",
        "frame starting offset must be a non-negative number",
        "frame ending offset must be a non-negative number",
    ];
    const A_OP: [u8; 5] = [OP_Ge, OP_Ge, OP_Gt, OP_Ge, OP_Ge];
    sqlite3_get_vdbe(p);
    let reg_zero = sqlite3_get_temp_reg(p);
    p.v.add_op2(OP_Integer, 0, reg_zero);
    if e_cond >= WINDOW_STARTING_NUM {
        let reg_string = sqlite3_get_temp_reg(p);
        p.v.add_op4(OP_String8, 0, reg_string, 0, P4::Static(""));
        let a = p.v.current_addr() + 2;
        p.v.add_op3(OP_Ge, reg_string, a, reg);
        p.v.change_p5((SQLITE_AFF_NUMERIC as i32 | SQLITE_JUMPIFNULL) as u16);
    } else {
        let a = p.v.current_addr() + 2;
        p.v.add_op2(OP_MustBeInt, reg, a);
    }
    let a = p.v.current_addr() + 2;
    p.v.add_op3(A_OP[e_cond as usize], reg_zero, a, reg);
    p.v.change_p5(SQLITE_AFF_NUMERIC as u16);
    sqlite3_may_abort(p);
    p.v.add_op2(OP_Halt, SQLITE_ERROR, OE_Abort);
    p.v.append_p4(P4::Static(AZ_ERR[e_cond as usize]));
    sqlite3_release_temp_reg(p, reg_zero);
}

/// `windowArgCount(pWin)`: the number of arguments passed to the window-function associated
/// with the object passed as the only argument to this function.
fn window_arg_count(p: &Parse, win: WindowId) -> i32 {
    let list = p.e(p.w(win).p_owner).x_list();
    if list.nn() { p.l(list).n_expr() } else { 0 }
}

/// `struct WindowCsrAndReg`
#[derive(Debug, Clone, Copy, Default)]
struct WindowCsrAndReg {
    /// Cursor number
    csr: i32,
    /// First in array of peer values
    reg: i32,
}

/// `struct WindowCodeArg`: the context of sqlite3WindowCodeStep() and its helper routines.
#[derive(Debug, Clone, Copy, Default)]
struct WindowCodeArg {
    /// First in list of functions being processed
    m_win: WindowId,
    /// OP_Gosub to this address to return one row
    addr_gosub: i32,
    /// Register used with OP_Gosub(addrGosub)
    reg_gosub: i32,
    /// First in array of accumulator registers
    reg_arg: i32,
    /// See above
    e_delete: i32,
    reg_rowid: i32,
    start: WindowCsrAndReg,
    current: WindowCsrAndReg,
    end: WindowCsrAndReg,
}

/// `windowReadPeerValues(p, csr, reg)`: generate VM code to read the window frames peer values
/// from cursor csr into an array of registers starting at reg.
fn window_read_peer_values(p: &mut Parse, s: &WindowCodeArg, csr: i32, reg: i32) {
    let m_win = s.m_win;
    let ob = p.w(m_win).p_order_by;
    if ob.nn() {
        let part = p.w(m_win).p_partition;
        let i_col_off = p.w(m_win).n_buffer_col + if part.nn() { p.l(part).n_expr() } else { 0 };
        for i in 0..p.l(ob).n_expr() {
            p.v.add_op3(OP_Column, csr, i_col_off + i, reg + i);
        }
    }
}

/// `windowAggStep(p, pMWin, csr, bInverse, reg)`: generate VM code to invoke either xStep()
/// (if bInverse is 0) or xInverse (if bInverse is non-zero) for each window function in the
/// linked list starting at pMWin. Or, for built-in window functions that do not use the
/// standard function API, generate the required inline VM code.
fn window_agg_step(p: &mut Parse, _s: &WindowCodeArg, m_win: WindowId, csr: i32, b_inverse: i32, reg: i32) {
    let mut win = m_win;
    while win.nn() {
        let func = wfunc(p, win);
        let mut n_arg = if p.w(win).b_expr_args != 0 { 0 } else { window_arg_count(p, win) };
        let mut addr_if = 0;
        let i_arg_col = p.w(win).i_arg_col;

        for i in 0..n_arg {
            if i != 1 || !is_win_func(&func, "nth_value") {
                p.v.add_op3(OP_Column, csr, i_arg_col + i, reg + i);
            } else {
                let eph = p.w(m_win).i_eph_csr;
                p.v.add_op3(OP_Column, eph, i_arg_col + i, reg + i);
            }
        }
        let mut reg_arg = reg;

        if p.w(win).p_filter.nn() {
            let reg_tmp = sqlite3_get_temp_reg(p);
            p.v.add_op3(OP_Column, csr, i_arg_col + n_arg, reg_tmp);
            addr_if = p.v.add_op3(OP_IfNot, reg_tmp, 0, 1);
            sqlite3_release_temp_reg(p, reg_tmp);
        }

        if p.w(m_win).reg_start_rowid == 0
            && (func.func_flags & SQLITE_FUNC_MINMAX) != 0
            && p.w(win).e_start != TK_UNBOUNDED
        {
            let (reg_app, csr_app) = (p.w(win).reg_app, p.w(win).csr_app);
            let addr_is_null = p.v.add_op1(OP_IsNull, reg_arg);
            if b_inverse == 0 {
                p.v.add_op2(OP_AddImm, reg_app + 1, 1);
                p.v.add_op2(OP_SCopy, reg_arg, reg_app);
                p.v.add_op3(OP_MakeRecord, reg_app, 2, reg_app + 2);
                p.v.add_op2(OP_IdxInsert, csr_app, reg_app + 2);
            } else {
                p.v.add_op4_int(OP_SeekGE, csr_app, 0, reg_arg, 1);
                p.v.add_op1(OP_Delete, csr_app);
                let a = p.v.current_addr() - 2;
                p.v.jump_here(a);
            }
            p.v.jump_here(addr_is_null);
        } else if p.w(win).reg_app != 0 {
            debug_assert!(is_win_func(&func, "nth_value") || is_win_func(&func, "first_value"));
            let reg_app = p.w(win).reg_app;
            p.v.add_op2(OP_AddImm, reg_app + 1 - b_inverse, 1);
        } else if !is_noop_step(&func) {
            let owner = p.w(win).p_owner;
            let eph = p.w(m_win).i_eph_csr;
            if p.w(win).b_expr_args != 0 {
                let mut i_op = p.v.current_addr();
                let list = p.e(owner).x_list();
                n_arg = p.l(list).n_expr();
                reg_arg = sqlite3_get_temp_range(p, n_arg);
                sqlite3_expr_code_expr_list(p, list, reg_arg, 0, 0);

                let i_end = p.v.current_addr();
                while i_op < i_end {
                    let op = p.v.get_op_mut(i_op);
                    if op.opcode == OP_Column && op.p1 == eph {
                        op.p1 = csr;
                    }
                    i_op += 1;
                }
            }
            if func.func_flags & SQLITE_FUNC_NEEDCOLL != 0 {
                let list = p.e(owner).x_list();
                let a0 = p.l(list).a[0].p_expr;
                let coll = sqlite3_expr_nn_coll_seq(p, a0);
                p.v.add_op4(OP_CollSeq, 0, 0, 0, P4::CollSeq(coll));
            }
            let reg_accum = p.w(win).reg_accum;
            p.v.add_op3(if b_inverse != 0 { OP_AggInverse } else { OP_AggStep }, b_inverse, reg_arg, reg_accum);
            p.v.append_p4(P4::FuncDef(func.clone()));
            p.v.change_p5(n_arg as u16);
            if p.w(win).b_expr_args != 0 {
                sqlite3_release_temp_range(p, reg_arg, n_arg);
            }
        }

        if addr_if != 0 {
            p.v.jump_here(addr_if);
        }
        win = p.w(win).p_next_win;
    }
}

// Values that may be passed as the second argument to windowCodeOp().
const WINDOW_RETURN_ROW: i32 = 1;
const WINDOW_AGGINVERSE: i32 = 2;
const WINDOW_AGGSTEP: i32 = 3;

/// `windowAggFinal(p, bFin)`: generate VM code to invoke either xValue() (bFin==0) or
/// xFinalize() (bFin==1) for each window function in the linked list starting at pMWin. Or,
/// for built-in window-functions that do not use the standard API, generate the equivalent VM
/// code.
fn window_agg_final(p: &mut Parse, s: &WindowCodeArg, b_fin: bool) {
    let m_win = s.m_win;
    let mut win = m_win;
    while win.nn() {
        let func = wfunc(p, win);
        let (reg_result, reg_accum, csr_app) = (p.w(win).reg_result, p.w(win).reg_accum, p.w(win).csr_app);
        if p.w(m_win).reg_start_rowid == 0 && (func.func_flags & SQLITE_FUNC_MINMAX) != 0 && p.w(win).e_start != TK_UNBOUNDED
        {
            p.v.add_op2(OP_Null, 0, reg_result);
            p.v.add_op1(OP_Last, csr_app);
            p.v.add_op3(OP_Column, csr_app, 0, reg_result);
            let a = p.v.current_addr() - 2;
            p.v.jump_here(a);
        } else if p.w(win).reg_app != 0 {
            debug_assert!(p.w(m_win).reg_start_rowid == 0);
        } else {
            let n_arg = window_arg_count(p, win);
            if b_fin {
                p.v.add_op2(OP_AggFinal, reg_accum, n_arg);
                p.v.append_p4(P4::FuncDef(func.clone()));
                p.v.add_op2(OP_Copy, reg_accum, reg_result);
                p.v.add_op2(OP_Null, 0, reg_accum);
            } else {
                p.v.add_op3(OP_AggValue, reg_accum, n_arg, reg_result);
                p.v.append_p4(P4::FuncDef(func.clone()));
            }
        }
        win = p.w(win).p_next_win;
    }
}

/// `windowFullScan(p)`: generate code to calculate the current values of all window functions
/// in the p->pMWin list by doing a full scan of the current window frame. Store the results in
/// the Window.regResult registers, ready to return the upper layer.
fn window_full_scan(p: &mut Parse, s: &WindowCodeArg) {
    let m_win = s.m_win;
    let mut reg_c_peer = 0; // Current peer values
    let mut reg_peer = 0; // AggStep peer values

    let csr = p.w(m_win).csr_app;
    let ob = p.w(m_win).p_order_by;
    let n_peer = if ob.nn() { p.l(ob).n_expr() } else { 0 };

    let lbl_next = p.v.make_label();
    let lbl_brk = p.v.make_label();

    let reg_c_rowid = sqlite3_get_temp_reg(p); // Current rowid value
    let reg_rowid = sqlite3_get_temp_reg(p); // AggStep rowid value
    if n_peer != 0 {
        reg_c_peer = sqlite3_get_temp_range(p, n_peer);
        reg_peer = sqlite3_get_temp_range(p, n_peer);
    }

    let eph = p.w(m_win).i_eph_csr;
    p.v.add_op2(OP_Rowid, eph, reg_c_rowid);
    window_read_peer_values(p, s, eph, reg_c_peer);

    let mut win = m_win;
    while win.nn() {
        let r = p.w(win).reg_accum;
        p.v.add_op2(OP_Null, 0, r);
        win = p.w(win).p_next_win;
    }

    let (rs, re) = (p.w(m_win).reg_start_rowid, p.w(m_win).reg_end_rowid);
    p.v.add_op3(OP_SeekGE, csr, lbl_brk, rs);
    let addr_next = p.v.current_addr();
    p.v.add_op2(OP_Rowid, csr, reg_rowid);
    p.v.add_op3(OP_Gt, re, lbl_brk, reg_rowid);

    let e_exclude = p.w(m_win).e_exclude;
    if e_exclude == TK_CURRENT {
        p.v.add_op3(OP_Eq, reg_c_rowid, lbl_next, reg_rowid);
    } else if e_exclude != TK_NO {
        let mut addr_eq = 0;
        let mut key_info = None;

        if ob.nn() {
            key_info = Some(sqlite3_key_info_from_expr_list(p, ob, 0, 0));
        }
        if e_exclude == TK_TIES {
            addr_eq = p.v.add_op3(OP_Eq, reg_c_rowid, 0, reg_rowid);
        }
        if let Some(ki) = key_info {
            window_read_peer_values(p, s, csr, reg_peer);
            p.v.add_op3(OP_Compare, reg_peer, reg_c_peer, n_peer);
            p.v.append_p4(P4::KeyInfo(Arc::new(ki)));
            let addr = p.v.current_addr() + 1;
            p.v.add_op3(OP_Jump, addr, lbl_next, addr);
        } else {
            p.v.add_op2(OP_Goto, 0, lbl_next);
        }
        if addr_eq != 0 {
            p.v.jump_here(addr_eq);
        }
    }

    window_agg_step(p, s, m_win, csr, 0, s.reg_arg);

    p.v.resolve_label(lbl_next);
    p.v.add_op2(OP_Next, csr, addr_next);
    p.v.jump_here(addr_next - 1);
    p.v.jump_here(addr_next + 1);
    sqlite3_release_temp_reg(p, reg_rowid);
    sqlite3_release_temp_reg(p, reg_c_rowid);
    if n_peer != 0 {
        sqlite3_release_temp_range(p, reg_peer, n_peer);
        sqlite3_release_temp_range(p, reg_c_peer, n_peer);
    }

    window_agg_final(p, s, true);
}

/// `windowReturnOneRow(p)`: invoke the sub-routine at regGosub (generated by code in select.c)
/// to return the current row of Window.iEphCsr. If all window functions are aggregate window
/// functions that use the standard API, a single OP_Gosub instruction is all that this routine
/// generates. Extra VM code for per-row processing is only generated for nth_value(),
/// first_value(), lag() and lead().
fn window_return_one_row(p: &mut Parse, s: &WindowCodeArg) {
    let m_win = s.m_win;
    if p.w(m_win).reg_start_rowid != 0 {
        window_full_scan(p, s);
    } else {
        let mut win = m_win;
        while win.nn() {
            let func = wfunc(p, win);
            let (csr, reg_result, reg_app, i_arg_col) =
                (p.w(win).csr_app, p.w(win).reg_result, p.w(win).reg_app, p.w(win).i_arg_col);
            let eph = p.w(m_win).i_eph_csr;
            if is_win_func(&func, "nth_value") || is_win_func(&func, "first_value") {
                let lbl = p.v.make_label();
                let tmp_reg = sqlite3_get_temp_reg(p);
                p.v.add_op2(OP_Null, 0, reg_result);

                if is_win_func(&func, "nth_value") {
                    p.v.add_op3(OP_Column, eph, i_arg_col + 1, tmp_reg);
                    window_check_value(p, tmp_reg, WINDOW_NTH_VALUE_INT);
                } else {
                    p.v.add_op2(OP_Integer, 1, tmp_reg);
                }
                p.v.add_op3(OP_Add, tmp_reg, reg_app, tmp_reg);
                p.v.add_op3(OP_Gt, reg_app + 1, lbl, tmp_reg);
                p.v.add_op3(OP_SeekRowid, csr, 0, tmp_reg);
                p.v.add_op3(OP_Column, csr, i_arg_col, reg_result);
                p.v.resolve_label(lbl);
                sqlite3_release_temp_reg(p, tmp_reg);
            } else if is_win_func(&func, "lead") || is_win_func(&func, "lag") {
                let n_arg = p.l(p.e(p.w(win).p_owner).x_list()).n_expr();
                let lbl = p.v.make_label();
                let tmp_reg = sqlite3_get_temp_reg(p);
                let is_lead = is_win_func(&func, "lead");

                if n_arg < 3 {
                    p.v.add_op2(OP_Null, 0, reg_result);
                } else {
                    p.v.add_op3(OP_Column, eph, i_arg_col + 2, reg_result);
                }
                p.v.add_op2(OP_Rowid, eph, tmp_reg);
                if n_arg < 2 {
                    let val = if is_lead { 1 } else { -1 };
                    p.v.add_op2(OP_AddImm, tmp_reg, val);
                } else {
                    let op = if is_lead { OP_Add } else { OP_Subtract };
                    let tmp_reg2 = sqlite3_get_temp_reg(p);
                    p.v.add_op3(OP_Column, eph, i_arg_col + 1, tmp_reg2);
                    p.v.add_op3(op, tmp_reg2, tmp_reg, tmp_reg);
                    sqlite3_release_temp_reg(p, tmp_reg2);
                }

                p.v.add_op3(OP_SeekRowid, csr, lbl, tmp_reg);
                p.v.add_op3(OP_Column, csr, i_arg_col, reg_result);
                p.v.resolve_label(lbl);
                sqlite3_release_temp_reg(p, tmp_reg);
            }
            win = p.w(win).p_next_win;
        }
    }
    p.v.add_op2(OP_Gosub, s.reg_gosub, s.addr_gosub);
}

/// `windowInitAccum(pParse, pMWin)`: generate code to set the accumulator register for each
/// window function in the linked list passed as the second argument to NULL. And perform any
/// equivalent initialization required by any built-in window functions in the list.
fn window_init_accum(p: &mut Parse, m_win: WindowId) -> i32 {
    let mut n_arg = 0;
    let mut win = m_win;
    while win.nn() {
        let func = wfunc(p, win);
        let (reg_accum, reg_app, csr_app) = (p.w(win).reg_accum, p.w(win).reg_app, p.w(win).csr_app);
        p.v.add_op2(OP_Null, 0, reg_accum);
        n_arg = n_arg.max(window_arg_count(p, win));
        if p.w(m_win).reg_start_rowid == 0 {
            if is_win_func(&func, "nth_value") || is_win_func(&func, "first_value") {
                p.v.add_op2(OP_Integer, 0, reg_app);
                p.v.add_op2(OP_Integer, 0, reg_app + 1);
            }

            if (func.func_flags & SQLITE_FUNC_MINMAX) != 0 && csr_app != 0 {
                debug_assert!(p.w(win).e_start != TK_UNBOUNDED);
                p.v.add_op1(OP_ResetSorter, csr_app);
                p.v.add_op2(OP_Integer, 0, reg_app + 1);
            }
        }
        win = p.w(win).p_next_win;
    }
    let reg_arg = p.n_mem + 1;
    p.n_mem += n_arg;
    reg_arg
}

/// `windowCacheFrame(pMWin)`: return true if the current frame should be cached in the
/// ephemeral table, even if there are no xInverse() calls required.
fn window_cache_frame(p: &Parse, m_win: WindowId) -> bool {
    if p.w(m_win).reg_start_rowid != 0 {
        return true;
    }
    let mut win = m_win;
    while win.nn() {
        let func = wfunc(p, win);
        if is_win_func(&func, "nth_value")
            || is_win_func(&func, "first_value")
            || is_win_func(&func, "lead")
            || is_win_func(&func, "lag")
        {
            return true;
        }
        win = p.w(win).p_next_win;
    }
    false
}

/// `windowIfNewPeer(pParse, pOrderBy, regNew, regOld, addr)`: regOld and regNew are each the
/// first register in an array of size pOrderBy->nExpr. This function generates code to compare
/// the two arrays of registers using the collation sequences and other comparison parameters
/// specified by pOrderBy. If the two arrays are not equal, the contents of regNew is copied to
/// regOld and control falls through. Otherwise, if the contents of the arrays are equal, an
/// OP_Goto is executed.
fn window_if_new_peer(p: &mut Parse, order_by: ExprListId, reg_new: i32, reg_old: i32, addr: i32) {
    if order_by.nn() {
        let n_val = p.l(order_by).n_expr();
        let key_info = sqlite3_key_info_from_expr_list(p, order_by, 0, 0);
        p.v.add_op3(OP_Compare, reg_old, reg_new, n_val);
        p.v.append_p4(P4::KeyInfo(Arc::new(key_info)));
        let a = p.v.current_addr() + 1;
        p.v.add_op3(OP_Jump, a, addr, a);
        p.v.add_op3(OP_Copy, reg_new, reg_old, n_val - 1);
    } else {
        p.v.add_op2(OP_Goto, 0, addr);
    }
}

/// `windowCodeRangeTest(p, op, csr1, regVal, csr2, lbl)`: called as part of generating VM
/// programs for RANGE offset PRECEDING/FOLLOWING frame boundaries. Assuming "ASC" order for the
/// ORDER BY term in the window, and that argument op is OP_Ge, it generates code equivalent
/// to:
///
///   if( csr1.peerVal + regVal >= csr2.peerVal ) goto lbl;
///
/// The value of parameter op may also be OP_Gt or OP_Le. With DESC sort order the comparison
/// is reversed and regVal is subtracted.
fn window_code_range_test(p: &mut Parse, s: &WindowCodeArg, mut op: u8, csr1: i32, reg_val: i32, csr2: i32, lbl: i32) {
    let order_by = p.w(s.m_win).p_order_by; // ORDER BY clause for window
    let reg1 = sqlite3_get_temp_reg(p); // Reg. for csr1.peerVal+regVal
    let reg2 = sqlite3_get_temp_reg(p); // Reg. for csr2.peerVal
    p.n_mem += 1;
    let reg_string = p.n_mem; // Reg. for constant value ''
    let mut arith = OP_Add; // OP_Add or OP_Subtract
    let addr_done = p.v.make_label(); // Address past OP_Ge

    // Read the peer-value from each cursor into a register
    window_read_peer_values(p, s, csr1, reg1);
    window_read_peer_values(p, s, csr2, reg2);

    let sort_flags = p.l(order_by).a[0].sort_flags;
    if sort_flags & KEYINFO_ORDER_DESC != 0 {
        op = match op {
            OP_Ge => OP_Le,
            OP_Gt => OP_Lt,
            _ => OP_Ge,
        };
        arith = OP_Subtract;
    }

    // If the BIGNULL flag is set for the ORDER BY, then it is required to consider NULL
    // values to be larger than all other values, instead of the usual smaller. The VDBE
    // opcodes OP_Ge and so on do not handle this (and adding that capability causes a
    // performance regression), so instead if the BIGNULL flag is set then cases where either
    // reg1 or reg2 are NULL are handled separately in the following block.
    if sort_flags & KEYINFO_ORDER_BIGNULL != 0 {
        // This block runs if reg1 contains a NULL.
        let addr = p.v.add_op1(OP_NotNull, reg1);
        match op {
            OP_Ge => {
                p.v.add_op2(OP_Goto, 0, lbl);
            }
            OP_Gt => {
                p.v.add_op2(OP_NotNull, reg2, lbl);
            }
            OP_Le => {
                p.v.add_op2(OP_IsNull, reg2, lbl);
            }
            _ => {} // OP_Lt: no-op
        }
        p.v.add_op2(OP_Goto, 0, addr_done);

        // This block runs if reg1 is not NULL, but reg2 is.
        p.v.jump_here(addr);
        p.v.add_op2(OP_IsNull, reg2, if op == OP_Gt || op == OP_Ge { addr_done } else { lbl });
    }

    // Register reg1 currently contains csr1.peerVal (the peer-value from csr1). This block
    // adds (or subtracts for DESC) the numeric value in regVal from it. Or, if reg1 is not
    // numeric (it is a NULL, a text value or a blob), then leave reg1 as it is.
    p.v.add_op4(OP_String8, 0, reg_string, 0, P4::Static(""));
    let addr_ge = p.v.add_op3(OP_Ge, reg_string, 0, reg1);
    if (op == OP_Ge && arith == OP_Add) || (op == OP_Le && arith == OP_Subtract) {
        p.v.add_op3(op, reg2, lbl, reg1);
    }
    p.v.add_op3(arith, reg_val, reg1, reg1);
    p.v.jump_here(addr_ge);

    // Compare registers reg2 and reg1, taking the jump if required. Note that control skips
    // over this test if the BIGNULL flag is set and either reg1 or reg2 contain a NULL value.
    p.v.add_op3(op, reg2, lbl, reg1);
    let e0 = p.l(order_by).a[0].p_expr;
    let coll = sqlite3_expr_nn_coll_seq(p, e0);
    p.v.append_p4(P4::CollSeq(coll));
    p.v.change_p5(SQLITE_NULLEQ as u16);
    p.v.resolve_label(addr_done);

    sqlite3_release_temp_reg(p, reg1);
    sqlite3_release_temp_reg(p, reg2);
}

/// `windowCodeOp(p, op, regCountdown, jumpOnEof)`: helper function for
/// sqlite3WindowCodeStep(). Each call to this function generates VM code for a single
/// RETURN_ROW, AGGSTEP or AGGINVERSE operation.
fn window_code_op(p: &mut Parse, s: &WindowCodeArg, op: i32, reg_countdown: i32, jump_on_eof: i32) -> i32 {
    let m_win = s.m_win;
    let mut ret = 0;
    let b_peer = p.w(m_win).e_frm_type != TK_ROWS;
    let (e_start, e_end, e_frm_type) = (p.w(m_win).e_start, p.w(m_win).e_end, p.w(m_win).e_frm_type);

    let lbl_done = p.v.make_label();
    let mut addr_next_range = 0;

    // Special case - WINDOW_AGGINVERSE is always a no-op if the frame starts with UNBOUNDED
    // PRECEDING.
    if op == WINDOW_AGGINVERSE && e_start == TK_UNBOUNDED {
        debug_assert!(reg_countdown == 0 && jump_on_eof == 0);
        return 0;
    }

    if reg_countdown > 0 {
        if e_frm_type == TK_RANGE {
            addr_next_range = p.v.current_addr();
            if op == WINDOW_AGGINVERSE {
                if e_start == TK_FOLLOWING {
                    window_code_range_test(p, s, OP_Le, s.current.csr, reg_countdown, s.start.csr, lbl_done);
                } else {
                    window_code_range_test(p, s, OP_Ge, s.start.csr, reg_countdown, s.current.csr, lbl_done);
                }
            } else {
                window_code_range_test(p, s, OP_Gt, s.end.csr, reg_countdown, s.current.csr, lbl_done);
            }
        } else {
            p.v.add_op3(OP_IfPos, reg_countdown, lbl_done, 1);
        }
    }

    if op == WINDOW_RETURN_ROW && p.w(m_win).reg_start_rowid == 0 {
        window_agg_final(p, s, false);
    }
    let addr_continue = p.v.current_addr();

    // If this is a (RANGE BETWEEN a FOLLOWING AND b FOLLOWING) or (RANGE BETWEEN b PRECEDING
    // AND a PRECEDING) frame, ensure the start cursor does not advance past the end cursor
    // within the temporary table. It otherwise might, if (a>b). Also ensure that, if the input
    // cursor is still finding new rows, that the end cursor does not go past it to EOF.
    if e_start == e_end && reg_countdown != 0 && e_frm_type == TK_RANGE {
        let reg_rowid1 = sqlite3_get_temp_reg(p);
        let reg_rowid2 = sqlite3_get_temp_reg(p);
        if op == WINDOW_AGGINVERSE {
            p.v.add_op2(OP_Rowid, s.start.csr, reg_rowid1);
            p.v.add_op2(OP_Rowid, s.end.csr, reg_rowid2);
            p.v.add_op3(OP_Ge, reg_rowid2, lbl_done, reg_rowid1);
        } else if s.reg_rowid != 0 {
            p.v.add_op2(OP_Rowid, s.end.csr, reg_rowid1);
            p.v.add_op3(OP_Ge, s.reg_rowid, lbl_done, reg_rowid1);
        }
        sqlite3_release_temp_reg(p, reg_rowid1);
        sqlite3_release_temp_reg(p, reg_rowid2);
    }

    let (csr, reg);
    match op {
        WINDOW_RETURN_ROW => {
            csr = s.current.csr;
            reg = s.current.reg;
            window_return_one_row(p, s);
        }
        WINDOW_AGGINVERSE => {
            csr = s.start.csr;
            reg = s.start.reg;
            if p.w(m_win).reg_start_rowid != 0 {
                let r = p.w(m_win).reg_start_rowid;
                p.v.add_op2(OP_AddImm, r, 1);
            } else {
                window_agg_step(p, s, m_win, csr, 1, s.reg_arg);
            }
        }
        _ => {
            debug_assert!(op == WINDOW_AGGSTEP);
            csr = s.end.csr;
            reg = s.end.reg;
            if p.w(m_win).reg_start_rowid != 0 {
                let r = p.w(m_win).reg_end_rowid;
                p.v.add_op2(OP_AddImm, r, 1);
            } else {
                window_agg_step(p, s, m_win, csr, 0, s.reg_arg);
            }
        }
    }

    if op == s.e_delete {
        p.v.add_op1(OP_Delete, csr);
        p.v.change_p5(OPFLAG_SAVEPOSITION as u16);
    }

    if jump_on_eof != 0 {
        let a = p.v.current_addr() + 2;
        p.v.add_op2(OP_Next, csr, a);
        ret = p.v.add_op0(OP_Goto);
    } else {
        let a = p.v.current_addr() + 1 + b_peer as i32;
        p.v.add_op2(OP_Next, csr, a);
        if b_peer {
            p.v.add_op2(OP_Goto, 0, lbl_done);
        }
    }

    if b_peer {
        let ob = p.w(m_win).p_order_by;
        let n_reg = if ob.nn() { p.l(ob).n_expr() } else { 0 };
        let reg_tmp = if n_reg != 0 { sqlite3_get_temp_range(p, n_reg) } else { 0 };
        window_read_peer_values(p, s, csr, reg_tmp);
        window_if_new_peer(p, ob, reg_tmp, reg, addr_continue);
        sqlite3_release_temp_range(p, reg_tmp, n_reg);
    }

    if addr_next_range != 0 {
        p.v.add_op2(OP_Goto, 0, addr_next_range);
    }
    p.v.resolve_label(lbl_done);
    ret
}

/// `windowExprGtZero(pParse, pExpr)`: return true if it can be determined at compile time that
/// expression pExpr evaluates to a value that, when cast to an integer, is greater than zero.
fn window_expr_gt_zero(p: &mut Parse, e: ExprId) -> bool {
    match crate::build_tab::sqlite3_value_from_expr(p, e, SQLITE_AFF_NUMERIC) {
        Some(v) => (Mem::from_value(&v).int_value() as i32) > 0,
        None => false,
    }
}

/// The Window-object dump layer of the V4a differential proof (one `WIN` line per window of
/// the list at the start of sqlite3WindowCodeStep()). Verification aid; C counterpart in the
/// scratch instrumentation `target/perf-work/v4a/wdump/patch_v4a.py`.
fn dump_windows(p: &mut Parse, sel: SelectId, s: &WindowCodeArg) {
    if p.where_dump.is_none() || p.db.init_busy {
        return;
    }
    let mut lines = Vec::new();
    let m_win = s.m_win;
    let mut win = m_win;
    let mut k = 0;
    while win.nn() {
        let w = p.w(win);
        let nl = |l: ExprListId| if l.nn() { p.l(l).n_expr() } else { -1 };
        let f = w.p_w_func.as_ref();
        lines.push(format!(
            "WIN {} sel={} func={}/{} flags={:x} frm={} start={} end={} excl={} implicit={} pStart={} pEnd={} part={} ob={} filter={} eph={} accum={} result={} csrApp={} regApp={} regPart={} nBuf={} argCol={} regOne={} rs={} re={} exprArgs={} eDelete={} regArg={}",
            k,
            p.s(sel).sel_id,
            f.map(|f| f.name.as_str()).unwrap_or("-"),
            f.map(|f| f.n_arg as i32).unwrap_or(0),
            f.map(|f| f.func_flags).unwrap_or(0),
            w.e_frm_type,
            w.e_start,
            w.e_end,
            w.e_exclude,
            w.b_implicit_frame,
            w.p_start.nn() as i32,
            w.p_end.nn() as i32,
            nl(w.p_partition),
            nl(w.p_order_by),
            w.p_filter.nn() as i32,
            w.i_eph_csr,
            w.reg_accum,
            w.reg_result,
            w.csr_app,
            w.reg_app,
            w.reg_part,
            w.n_buffer_col,
            w.i_arg_col,
            w.reg_one,
            w.reg_start_rowid,
            w.reg_end_rowid,
            w.b_expr_args,
            s.e_delete,
            s.reg_arg,
        ));
        k += 1;
        win = w.p_next_win;
    }
    if let Some(d) = p.where_dump.as_mut() {
        d.extend(lines);
    }
}

/// `sqlite3WindowCodeStep(pParse, p, pWInfo, regGosub, addrGosub)`: sqlite3WhereBegin() has
/// already been called for the SELECT statement passed as the second argument when this
/// function is invoked. It generates code to populate the Window.regResult register for each
/// window function and invoke the sub-routine at instruction addrGosub once for each row.
/// sqlite3WhereEnd() is always called before returning. (See the long comment in window.c
/// for the pseudo-code of the frame types.)
pub fn sqlite3_window_code_step(p: &mut Parse, sel: SelectId, w_info: WhereInfo, reg_gosub: i32, addr_gosub: i32) {
    let m_win = p.s(sel).p_win;
    let order_by = p.w(m_win).p_order_by;
    let src = p.s(sel).p_src;
    let csr_input = p.src(src).a[0].i_cursor; // Cursor of sub-select
    let n_input = p.tab(p.src(src).a[0].p_s_tab).a_col.len() as i32; // Number of cols returned by sub
    let mut addr_gosub_flush = 0; // Address of OP_Gosub to flush:
    let mut addr_integer = 0; // Address of OP_Integer
    let mut reg_new_peer = 0; // Peer values for new row (part of regNew)
    let mut reg_peer = 0; // Peer values for current row
    let mut reg_flush_part = 0; // Register for "Gosub flush_partition"
    let mut reg_start = 0; // Value of <expr> PRECEDING
    let mut reg_end = 0; // Value of <expr> FOLLOWING

    let (e_start, e_end, e_frm_type) = (p.w(m_win).e_start, p.w(m_win).e_end, p.w(m_win).e_frm_type);

    let lbl_where_end = p.v.make_label(); // Label just before sqlite3WhereEnd() code

    // Fill in the context object
    let mut s = WindowCodeArg { m_win, reg_gosub, addr_gosub, ..WindowCodeArg::default() };
    s.current.csr = p.w(m_win).i_eph_csr;
    let csr_write = s.current.csr + 1; // Cursor used to write to eph. table
    s.start.csr = s.current.csr + 2;
    s.end.csr = s.current.csr + 3;

    // Figure out when rows may be deleted from the ephemeral table. There are four options -
    // they may never be deleted (eDelete==0), they may be deleted as soon as they are no
    // longer part of the window frame (eDelete==WINDOW_AGGINVERSE), they may be deleted as
    // after the row has been returned to the caller (WINDOW_RETURN_ROW), or they may be
    // deleted after they enter the frame (WINDOW_AGGSTEP).
    match e_start {
        TK_FOLLOWING => {
            let ps = p.w(m_win).p_start;
            if e_frm_type != TK_RANGE && window_expr_gt_zero(p, ps) {
                s.e_delete = WINDOW_RETURN_ROW;
            }
        }
        TK_UNBOUNDED => {
            if !window_cache_frame(p, m_win) {
                if e_end == TK_PRECEDING {
                    let pe = p.w(m_win).p_end;
                    if e_frm_type != TK_RANGE && window_expr_gt_zero(p, pe) {
                        s.e_delete = WINDOW_AGGSTEP;
                    }
                } else {
                    s.e_delete = WINDOW_RETURN_ROW;
                }
            }
        }
        _ => {
            s.e_delete = WINDOW_AGGINVERSE;
        }
    }

    // Allocate registers for the array of values from the sub-query, the same values in
    // record form, and the rowid used to insert said record into the ephemeral table.
    let reg_new = p.n_mem + 1; // Array of registers holding new input row
    p.n_mem += n_input;
    p.n_mem += 1;
    let reg_record = p.n_mem; // regNew array in record form
    p.n_mem += 1;
    s.reg_rowid = p.n_mem;

    // If the window frame contains an "<expr> PRECEDING" or "<expr> FOLLOWING" clause,
    // allocate registers to store the results of evaluating each <expr>.
    if e_start == TK_PRECEDING || e_start == TK_FOLLOWING {
        p.n_mem += 1;
        reg_start = p.n_mem;
    }
    if e_end == TK_PRECEDING || e_end == TK_FOLLOWING {
        p.n_mem += 1;
        reg_end = p.n_mem;
    }

    // If this is not a "ROWS BETWEEN ..." frame, then allocate arrays of registers to store
    // copies of the ORDER BY expressions (peer values) for the main loop, and for each cursor
    // (start, current and end).
    if e_frm_type != TK_ROWS {
        let n_peer = if order_by.nn() { p.l(order_by).n_expr() } else { 0 };
        reg_new_peer = reg_new + p.w(m_win).n_buffer_col;
        let part = p.w(m_win).p_partition;
        if part.nn() {
            reg_new_peer += p.l(part).n_expr();
        }
        reg_peer = p.n_mem + 1;
        p.n_mem += n_peer;
        s.start.reg = p.n_mem + 1;
        p.n_mem += n_peer;
        s.current.reg = p.n_mem + 1;
        p.n_mem += n_peer;
        s.end.reg = p.n_mem + 1;
        p.n_mem += n_peer;
    }

    // Load the column values for the row returned by the sub-select into an array of
    // registers starting at regNew. Assemble them into a record in register regRecord.
    for i_input in 0..n_input {
        p.v.add_op3(OP_Column, csr_input, i_input, reg_new + i_input);
    }
    p.v.add_op3(OP_MakeRecord, reg_new, n_input, reg_record);

    // An input row has just been read into an array of registers starting at regNew. If the
    // window has a PARTITION clause, this block generates VM code to check if the input row is
    // the start of a new partition. If so, it does an OP_Gosub to an address to be filled in
    // later. The address of the OP_Gosub is stored in local variable addrGosubFlush.
    let part = p.w(m_win).p_partition;
    if part.nn() {
        let n_part = p.l(part).n_expr();
        let reg_new_part = reg_new + p.w(m_win).n_buffer_col;
        let key_info = sqlite3_key_info_from_expr_list(p, part, 0, 0);
        let reg_part = p.w(m_win).reg_part;

        p.n_mem += 1;
        reg_flush_part = p.n_mem;
        let addr = p.v.add_op3(OP_Compare, reg_new_part, reg_part, n_part);
        p.v.append_p4(P4::KeyInfo(Arc::new(key_info)));
        p.v.add_op3(OP_Jump, addr + 2, addr + 4, addr + 2);
        addr_gosub_flush = p.v.add_op1(OP_Gosub, reg_flush_part);
        p.v.add_op3(OP_Copy, reg_new_part, reg_part, n_part - 1);
    }

    // Insert the new row into the ephemeral table
    p.v.add_op2(OP_NewRowid, csr_write, s.reg_rowid);
    p.v.add_op3(OP_Insert, csr_write, reg_record, s.reg_rowid);
    let reg_one = p.w(m_win).reg_one;
    let addr_ne = p.v.add_op3(OP_Ne, reg_one, 0, s.reg_rowid);

    // This block is run for the first row of each partition
    s.reg_arg = window_init_accum(p, m_win);
    dump_windows(p, sel, &s);

    if reg_start != 0 {
        let ps = p.w(m_win).p_start;
        sqlite3_expr_code(p, ps, reg_start);
        window_check_value(p, reg_start, WINDOW_STARTING_INT + if e_frm_type == TK_RANGE { 3 } else { 0 });
    }
    if reg_end != 0 {
        let pe = p.w(m_win).p_end;
        sqlite3_expr_code(p, pe, reg_end);
        window_check_value(p, reg_end, WINDOW_ENDING_INT + if e_frm_type == TK_RANGE { 3 } else { 0 });
    }

    if e_frm_type != TK_RANGE && e_start == e_end && reg_start != 0 {
        let op = if e_start == TK_FOLLOWING { OP_Ge } else { OP_Le };
        let addr_ge = p.v.add_op3(op, reg_start, 0, reg_end);
        window_agg_final(p, &s, false);
        p.v.add_op1(OP_Rewind, s.current.csr);
        window_return_one_row(p, &s);
        p.v.add_op1(OP_ResetSorter, s.current.csr);
        p.v.add_op2(OP_Goto, 0, lbl_where_end);
        p.v.jump_here(addr_ge);
    }
    if e_start == TK_FOLLOWING && e_frm_type != TK_RANGE && reg_end != 0 {
        debug_assert!(e_end == TK_FOLLOWING);
        p.v.add_op3(OP_Subtract, reg_start, reg_end, reg_start);
    }

    if e_start != TK_UNBOUNDED {
        p.v.add_op1(OP_Rewind, s.start.csr);
    }
    p.v.add_op1(OP_Rewind, s.current.csr);
    p.v.add_op1(OP_Rewind, s.end.csr);
    if reg_peer != 0 && order_by.nn() {
        let n = p.l(order_by).n_expr();
        p.v.add_op3(OP_Copy, reg_new_peer, reg_peer, n - 1);
        p.v.add_op3(OP_Copy, reg_peer, s.start.reg, n - 1);
        p.v.add_op3(OP_Copy, reg_peer, s.current.reg, n - 1);
        p.v.add_op3(OP_Copy, reg_peer, s.end.reg, n - 1);
    }

    p.v.add_op2(OP_Goto, 0, lbl_where_end);

    p.v.jump_here(addr_ne);

    // Beginning of the block executed for the second and subsequent rows.
    if reg_peer != 0 {
        window_if_new_peer(p, order_by, reg_new_peer, reg_peer, lbl_where_end);
    }
    if e_start == TK_FOLLOWING {
        window_code_op(p, &s, WINDOW_AGGSTEP, 0, 0);
        if e_end != TK_UNBOUNDED {
            if e_frm_type == TK_RANGE {
                let lbl = p.v.make_label();
                let addr_next = p.v.current_addr();
                window_code_range_test(p, &s, OP_Ge, s.current.csr, reg_end, s.end.csr, lbl);
                window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 0);
                window_code_op(p, &s, WINDOW_RETURN_ROW, 0, 0);
                p.v.add_op2(OP_Goto, 0, addr_next);
                p.v.resolve_label(lbl);
            } else {
                window_code_op(p, &s, WINDOW_RETURN_ROW, reg_end, 0);
                window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 0);
            }
        }
    } else if e_end == TK_PRECEDING {
        let b_rps = e_start == TK_PRECEDING && e_frm_type == TK_RANGE;
        window_code_op(p, &s, WINDOW_AGGSTEP, reg_end, 0);
        if b_rps {
            window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 0);
        }
        window_code_op(p, &s, WINDOW_RETURN_ROW, 0, 0);
        if !b_rps {
            window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 0);
        }
    } else {
        let mut addr = 0;
        window_code_op(p, &s, WINDOW_AGGSTEP, 0, 0);
        if e_end != TK_UNBOUNDED {
            if e_frm_type == TK_RANGE {
                let mut lbl = 0;
                addr = p.v.current_addr();
                if reg_end != 0 {
                    lbl = p.v.make_label();
                    window_code_range_test(p, &s, OP_Ge, s.current.csr, reg_end, s.end.csr, lbl);
                }
                window_code_op(p, &s, WINDOW_RETURN_ROW, 0, 0);
                window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 0);
                if reg_end != 0 {
                    p.v.add_op2(OP_Goto, 0, addr);
                    p.v.resolve_label(lbl);
                }
            } else {
                if reg_end != 0 {
                    addr = p.v.add_op3(OP_IfPos, reg_end, 0, 1);
                }
                window_code_op(p, &s, WINDOW_RETURN_ROW, 0, 0);
                window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 0);
                if reg_end != 0 {
                    p.v.jump_here(addr);
                }
            }
        }
    }

    // End of the main input loop
    p.v.resolve_label(lbl_where_end);
    crate::where_::sqlite3_where_end(p, w_info);

    // Fall through
    if part.nn() {
        addr_integer = p.v.add_op2(OP_Integer, 0, reg_flush_part);
        p.v.jump_here(addr_gosub_flush);
    }

    s.reg_rowid = 0;
    let addr_empty = p.v.add_op1(OP_Rewind, csr_write);
    if e_end == TK_PRECEDING {
        let b_rps = e_start == TK_PRECEDING && e_frm_type == TK_RANGE;
        window_code_op(p, &s, WINDOW_AGGSTEP, reg_end, 0);
        if b_rps {
            window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 0);
        }
        window_code_op(p, &s, WINDOW_RETURN_ROW, 0, 0);
    } else if e_start == TK_FOLLOWING {
        let addr_break1;
        let addr_break2;
        window_code_op(p, &s, WINDOW_AGGSTEP, 0, 0);
        let mut addr_start;
        if e_frm_type == TK_RANGE {
            addr_start = p.v.current_addr();
            addr_break2 = window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 1);
            addr_break1 = window_code_op(p, &s, WINDOW_RETURN_ROW, 0, 1);
        } else if e_end == TK_UNBOUNDED {
            addr_start = p.v.current_addr();
            addr_break1 = window_code_op(p, &s, WINDOW_RETURN_ROW, reg_start, 1);
            addr_break2 = window_code_op(p, &s, WINDOW_AGGINVERSE, 0, 1);
        } else {
            debug_assert!(e_end == TK_FOLLOWING);
            // assert( regStart>=0 ); regEnd = regEnd - regStart; regStart = 0;
            p.v.add_op3(OP_Subtract, reg_start, reg_end, reg_end);
            p.v.add_op2(OP_Integer, 0, reg_start);

            addr_start = p.v.current_addr();
            addr_break1 = window_code_op(p, &s, WINDOW_RETURN_ROW, reg_end, 1);
            addr_break2 = window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 1);
        }
        p.v.add_op2(OP_Goto, 0, addr_start);
        p.v.jump_here(addr_break2);
        addr_start = p.v.current_addr();
        let addr_break3 = window_code_op(p, &s, WINDOW_RETURN_ROW, 0, 1);
        p.v.add_op2(OP_Goto, 0, addr_start);
        p.v.jump_here(addr_break1);
        p.v.jump_here(addr_break3);
    } else {
        window_code_op(p, &s, WINDOW_AGGSTEP, 0, 0);
        let addr_start = p.v.current_addr();
        let addr_break = window_code_op(p, &s, WINDOW_RETURN_ROW, 0, 1);
        window_code_op(p, &s, WINDOW_AGGINVERSE, reg_start, 0);
        p.v.add_op2(OP_Goto, 0, addr_start);
        p.v.jump_here(addr_break);
    }
    p.v.jump_here(addr_empty);

    p.v.add_op1(OP_ResetSorter, s.current.csr);
    if part.nn() {
        let (rs, re) = (p.w(m_win).reg_start_rowid, p.w(m_win).reg_end_rowid);
        if rs != 0 {
            p.v.add_op2(OP_Integer, 1, rs);
            p.v.add_op2(OP_Integer, 0, re);
        }
        let a = p.v.current_addr();
        p.v.change_p1(addr_integer, a);
        p.v.add_op1(OP_Return, reg_flush_part);
    }
}
