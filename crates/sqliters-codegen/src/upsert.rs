//! Port of `upsert.c`: the ON CONFLICT clause of INSERT (`sqlite3UpsertNew`,
//! `sqlite3UpsertDup`, `sqlite3UpsertAnalyzeTarget`, `sqlite3UpsertOfIndex`,
//! `sqlite3UpsertNextIsIPK`, `sqlite3UpsertDoUpdate`).

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::P4;

use crate::consts::*;
use crate::expr::*;
use crate::sqliteint::*;
use crate::tk::*;

/// `sqlite3UpsertNew(db, pTarget, pTargetWhere, pSet, pWhere, pNext)`: allocate a new
/// Upsert object.
pub fn sqlite3_upsert_new(p: &mut Parse, target: ExprListId, target_where: ExprId, set: ExprListId, where_: ExprId, next: UpsertId) -> UpsertId {
    p.new_upsert(Upsert {
        p_upsert_target: target,
        p_upsert_target_where: target_where,
        p_upsert_set: set,
        p_upsert_where: where_,
        is_do_update: set.nn(),
        p_next_upsert: next,
        ..Upsert::default()
    })
}

/// `sqlite3UpsertDup(db, p)`: duplicate an Upsert object.
pub fn sqlite3_upsert_dup(p: &mut Parse, u: UpsertId) -> UpsertId {
    if u.is_null() {
        return UpsertId::NULL;
    }
    let x = p.up(u).clone();
    let target = sqlite3_expr_list_dup(p, x.p_upsert_target, 0);
    let tw = sqlite3_expr_dup(p, x.p_upsert_target_where, 0);
    let set = sqlite3_expr_list_dup(p, x.p_upsert_set, 0);
    let w = sqlite3_expr_dup(p, x.p_upsert_where, 0);
    let next = sqlite3_upsert_dup(p, x.p_next_upsert);
    sqlite3_upsert_new(p, target, tw, set, w, next)
}

/// `%r` of `sqlite3_snprintf()`: an ordinal number ("1st", "2nd", "3rd", "4th", ...).
fn ordinal(n: i32) -> String {
    let suffix = if (n % 100) / 10 == 1 {
        "th"
    } else {
        match n % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

/// `sqlite3UpsertAnalyzeTarget(pParse, pTabList, pUpsert, pAll)`: analyze the ON CONFLICT
/// clause described by pUpsert. Resolve all symbols in the conflict-target clause. Then
/// make sure that the conflict-target clause matches a PRIMARY KEY or UNIQUE constraint
/// (index) of the table. Return non-zero on error.
pub fn sqlite3_upsert_analyze_target(p: &mut Parse, tab_list: SrcListId, upsert: UpsertId, all: UpsertId) -> i32 {
    let snc = p.new_nc(NameContext { p_src_list: tab_list, ..NameContext::default() });
    let mut up = upsert;
    let mut n_clause = 0;
    while up.nn() && p.up(up).p_upsert_target.nn() {
        let rc = crate::resolve::sqlite3_resolve_expr_list_names(p, snc, p.up(up).p_upsert_target);
        if rc != 0 {
            return rc;
        }
        let tw = p.up(up).p_upsert_target_where;
        let rc = crate::resolve::sqlite3_resolve_expr_names(p, snc, tw);
        if rc != 0 {
            return rc;
        }

        // Check to see if the conflict target matches the rowid.
        let tab = p.src(tab_list).a[0].p_s_tab;
        let target = p.up(up).p_upsert_target;
        let i_cursor = p.src(tab_list).a[0].i_cursor;
        if p.tab(tab).has_rowid() && p.l(target).n_expr() == 1 {
            let term = p.l(target).a[0].p_expr;
            if p.e(term).op == TK_COLUMN && p.e(term).i_column as i32 == XN_ROWID {
                // The conflict-target is the rowid of the primary table
                up = p.up(up).p_next_upsert;
                n_clause += 1;
                continue;
            }
        }

        // Initialize sCol[0..1] to be an expression parse tree for a single column of an
        // index. The sCol[0] node will be the TK_COLLATE operator that specifies the
        // collating sequence, sCol[1] is the TK_COLUMN.
        let col1 = p.new_expr(Expr { op: TK_COLUMN, i_table: i_cursor, ..Expr::default() });
        let col0 = p.new_expr(Expr { op: TK_COLLATE, p_left: col1, ..Expr::default() });

        // Check for matches against other indexes
        let mut found = IndexId::NULL;
        for ix in p.tab(tab).p_index.clone() {
            if !p.idx(ix).is_unique() {
                continue;
            }
            if p.l(target).n_expr() != p.idx(ix).n_key_col as i32 {
                continue;
            }
            let part = p.idx(ix).p_part_idx_where;
            if part.nn() {
                let tw = p.up(up).p_upsert_target_where;
                if tw.is_null() {
                    continue;
                }
                if sqlite3_expr_compare(p, true, tw, part, i_cursor) != 0 {
                    continue;
                }
            }
            let nn = p.idx(ix).n_key_col as usize;
            let mut ii = 0;
            while ii < nn {
                let coll = p.idx(ix).az_coll[ii].clone();
                p.em(col0).z_token = Some(coll);
                let e;
                if p.idx(ix).ai_column[ii] as i32 == XN_EXPR {
                    let ce = p.l(p.idx(ix).a_col_expr).a[ii].p_expr;
                    if p.e(ce).op != TK_COLLATE {
                        p.em(col0).p_left = ce;
                        e = col0;
                    } else {
                        e = ce;
                    }
                } else {
                    p.em(col0).p_left = col1;
                    p.em(col1).i_column = p.idx(ix).ai_column[ii];
                    e = col0;
                }
                let mut jj = 0;
                while jj < nn {
                    let tj = p.l(target).a[jj].p_expr;
                    if sqlite3_expr_compare(p, false, tj, e, i_cursor) < 2 {
                        break; // Column ii of the index matches column jj of target
                    }
                    jj += 1;
                }
                if jj >= nn {
                    // The target contains no match for column jj of the index
                    break;
                }
                ii += 1;
            }
            if ii < nn {
                // Column ii of the index did not match any term of the conflict target.
                // Continue the search with the next index.
                continue;
            }
            p.upm(up).p_upsert_idx = ix;
            if sqlite3_upsert_of_index(p, all, ix) != up {
                // Really this should be an error. The isDup ON CONFLICT clause will never
                // fire. But this problem was not discovered until three years after
                // upsert was added, and so we silently ignore the problem to prevent
                // breaking applications that might actually have redundant ON CONFLICT
                // clauses.
                p.upm(up).is_dup = true;
            }
            found = ix;
            break;
        }
        if found.is_null() {
            let z_which = if n_clause == 0 && p.up(up).p_next_upsert.is_null() { String::new() } else { format!("{} ", ordinal(n_clause + 1)) };
            p.error_msg(format!("{z_which}ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint"));
            return SQLITE_ERROR;
        }
        up = p.up(up).p_next_upsert;
        n_clause += 1;
    }
    SQLITE_OK
}

/// `sqlite3UpsertNextIsIPK(pUpsert)`: return true if pUpsert is the last ON CONFLICT clause
/// with a conflict target, or if pUpsert is followed by another ON CONFLICT clause that
/// targets the INTEGER PRIMARY KEY.
pub fn sqlite3_upsert_next_is_ipk(p: &Parse, upsert: UpsertId) -> bool {
    if upsert.is_null() {
        return false;
    }
    let mut next = p.up(upsert).p_next_upsert;
    loop {
        if next.is_null() {
            return true;
        }
        if p.up(next).p_upsert_target.is_null() {
            return true;
        }
        if p.up(next).p_upsert_idx.is_null() {
            return true;
        }
        if !p.up(next).is_dup {
            return false;
        }
        next = p.up(next).p_next_upsert;
    }
}

/// `sqlite3UpsertOfIndex(pUpsert, pIdx)`: given the list of ON CONFLICT clauses described
/// by pUpsert, and a particular index pIdx, return a pointer to the particular ON CONFLICT
/// clause that applies to the index. Or, if the index is not subject to any ON CONFLICT
/// clause, return NULL.
pub fn sqlite3_upsert_of_index(p: &Parse, upsert: UpsertId, idx: IndexId) -> UpsertId {
    let mut u = upsert;
    while u.nn() && p.up(u).p_upsert_target.nn() && p.up(u).p_upsert_idx != idx {
        u = p.up(u).p_next_upsert;
    }
    u
}

/// `sqlite3UpsertDoUpdate(pParse, pUpsert, pTab, pIdx, iCur)`: generate bytecode that does
/// an UPDATE as part of an upsert.
///
/// If pIdx is NULL, then the UNIQUE constraint that failed was the IPK. In other words,
/// the conflict was on the rowid. Otherwise pIdx is the index whose constraint failed and
/// iCur is the cursor of that index, pointing at the conflicting row.
pub fn sqlite3_upsert_do_update(p: &mut Parse, upsert: UpsertId, tab: TableId, idx: IndexId, i_cur: i32) {
    let top = upsert;
    let i_data_cur = p.up(upsert).i_data_cur;
    let upsert = sqlite3_upsert_of_index(p, top, idx);
    if idx.nn() && i_cur != i_data_cur {
        if p.tab(tab).has_rowid() {
            let reg_rowid = crate::expr::sqlite3_get_temp_reg(p);
            p.v.add_op2(OP_IdxRowid, i_cur, reg_rowid);
            p.v.add_op3(OP_SeekRowid, i_data_cur, 0, reg_rowid);
            crate::expr::sqlite3_release_temp_reg(p, reg_rowid);
        } else {
            let pk = crate::schema::sqlite3_primary_key_index(p, tab);
            let n_pk = p.idx(pk).n_key_col as i32;
            let i_pk = p.n_mem + 1;
            p.n_mem += n_pk;
            for i in 0..n_pk {
                let c = p.idx(pk).ai_column[i as usize] as i32;
                let k = crate::schema::sqlite3_table_column_to_index(p.idx(idx), c);
                p.v.add_op3(OP_Column, i_cur, k, i_pk + i);
            }
            let i = p.v.add_op4_int(OP_Found, i_data_cur, 0, i_pk, n_pk);
            p.v.add_op4(OP_Halt, SQLITE_CORRUPT, OE_Abort, 0, P4::Static("corrupt database"));
            crate::build::sqlite3_may_abort(p);
            p.v.jump_here(i);
        }
    }
    // pUpsert does not own pTop->pUpsertSrc - the outer INSERT statement does. So we have
    // to make a copy before passing it down into sqlite3Update()
    let src0 = p.up(top).p_upsert_src;
    let src = sqlite3_src_list_dup(p, src0, 0);
    // excluded.* columns of type REAL need to be converted to a hard real
    let reg_data = p.up(top).reg_data;
    for i in 0..p.tab(tab).a_col.len() {
        if p.tab(tab).a_col[i].affinity == SQLITE_AFF_REAL {
            let i_storage = reg_data + crate::schema::sqlite3_table_column_to_storage(p.tab(tab), i as i32);
            p.v.add_op1(OP_RealAffinity, i_storage);
        }
    }
    let set = p.up(upsert).p_upsert_set;
    let set = sqlite3_expr_list_dup(p, set, 0);
    let w = p.up(upsert).p_upsert_where;
    let w = sqlite3_expr_dup(p, w, 0);
    crate::update::sqlite3_update(p, src, set, w, OE_Abort, ExprListId::NULL, ExprId::NULL, upsert);
}
