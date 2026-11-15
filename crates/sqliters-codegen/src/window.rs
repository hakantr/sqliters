//! `window.c`: the parts of the window-function machinery that aggregate functions with a
//! FILTER clause share with window functions (a FILTER clause is stored in a `Window` object
//! whose `eFrmType` is TK_FILTER). Window functions themselves (OVER) are not ported: the
//! parser reports them as unsupported.
#![allow(clippy::too_many_arguments)]

use crate::consts::*;
use crate::expr::*;
use crate::sqliteint::*;
use crate::tk::*;

/// `sqlite3WindowAttach(pParse, p, pWin)`: attach window object pWin to expression p.
pub fn sqlite3_window_attach(p: &mut Parse, e: ExprId, win: WindowId) {
    // (an OVER clause was reported as unsupported by its grammar rule: no Window object)
    if p.unsupported.is_some() || win.ix() >= p.a_win.len() {
        return;
    }
    if e.nn() && win.nn() {
        p.em(e).y_win = win;
        p.em(e).flags |= EP_WinFunc | EP_FullSize;
        p.a_win[win.ix()].p_owner = e;
        if p.e(e).has(EP_Distinct) && p.a_win[win.ix()].e_frm_type != TK_FILTER as u8 {
            p.error_msg("DISTINCT is not supported for window functions".to_owned());
        }
    }
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
        p_partition: sqlite3_expr_list_dup(p, old.p_partition, 0),
        p_order_by: sqlite3_expr_list_dup(p, old.p_order_by, 0),
        e_frm_type: old.e_frm_type,
        e_end: old.e_end,
        e_start: old.e_start,
        e_exclude: old.e_exclude,
        p_start: sqlite3_expr_dup(p, old.p_start, 0),
        p_end: sqlite3_expr_dup(p, old.p_end, 0),
        p_owner: owner,
        b_implicit_frame: old.b_implicit_frame,
        p_next_win: WindowId::NULL,
    };
    p.a_win.push(new);
    WindowId((p.a_win.len() - 1) as u32)
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
