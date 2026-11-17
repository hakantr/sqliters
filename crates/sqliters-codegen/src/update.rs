//! Port of `update.c`: the UPDATE statement (`sqlite3Update`, incl. UPDATE ... FROM through
//! `updateFromSelect`, the one-pass strategies, the UPSERT update and `updateVirtualTable`).

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::P4;

use crate::build::*;
use crate::consts::*;
use crate::delete::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::insert::*;
use crate::schema::*;
use crate::select::*;
use crate::sqliteint::*;
use crate::tk::*;

/// `indexColumnIsBeingUpdated(pIdx, iCol, aXRef, chngRowid)`: check to see if column iCol
/// of index pIdx references any of the columns defined by aXRef and chngRowid.
fn index_column_is_being_updated(p: &mut Parse, idx: IndexId, i_col: usize, a_x_ref: &[i32], chng_rowid: bool) -> bool {
    let i_idx_col = p.idx(idx).ai_column[i_col];
    if i_idx_col >= 0 {
        return a_x_ref[i_idx_col as usize] >= 0;
    }
    let e = p.l(p.idx(idx).a_col_expr).a[i_col].p_expr;
    sqlite3_expr_references_updated_column(p, e, a_x_ref, chng_rowid)
}

/// `indexWhereClauseMightChange(pIdx, aXRef, chngRowid)`: check to see if index pIdx is a
/// partial index whose conditional expression might change values due to an UPDATE.
fn index_where_clause_might_change(p: &mut Parse, idx: IndexId, a_x_ref: &[i32], chng_rowid: bool) -> bool {
    let part = p.idx(idx).p_part_idx_where;
    if part.is_null() {
        return false;
    }
    sqlite3_expr_references_updated_column(p, part, a_x_ref, chng_rowid)
}

/// `exprRowColumn(pParse, iCol)`: allocate and return a pointer to an expression of type
/// TK_ROW with iColumn set to iCol+1.
fn expr_row_column(p: &mut Parse, i_col: i32) -> ExprId {
    let r = sqlite3_p_expr(p, TK_ROW, ExprId::NULL, ExprId::NULL);
    if r.nn() {
        p.em(r).i_column = (i_col + 1) as i16;
    }
    r
}

/// `updateFromSelect(pParse, iEph, pPk, pChanges, pTabList, pWhere, pOrderBy, pLimit)`:
/// assuming both the pLimit and pOrderBy parameters are NULL, this function generates code
/// equivalent to
///
/// ```text
///     CREATE TEMP TABLE xxx AS SELECT <rowid/pk>, <changes> FROM <tables> WHERE <where>
/// ```
///
/// stored in the ephemeral table iEph (SRT_Upfrom, or SRT_Table for a view / virtual
/// table).
#[allow(clippy::too_many_arguments)]
fn update_from_select(p: &mut Parse, i_eph: i32, pk: IndexId, changes: ExprListId, tab_list: SrcListId, where_: ExprId) {
    let tab = p.src(tab_list).a[0].p_s_tab;
    let mut list = ExprListId::NULL;
    let src = sqlite3_src_list_dup(p, tab_list, 0);
    let where2 = sqlite3_expr_dup(p, where_, 0);
    if src.nn() {
        let st = p.src(src).a[0].p_s_tab;
        p.srcm(src).a[0].i_cursor = -1;
        if st.nn() {
            p.tabm(st).n_tab_ref -= 1;
        }
        p.srcm(src).a[0].p_s_tab = TableId::NULL;
    }
    let e_dest;
    if pk.nn() {
        for i in 0..p.idx(pk).n_key_col as usize {
            let c = p.idx(pk).ai_column[i] as i32;
            let new = expr_row_column(p, c);
            list = sqlite3_expr_list_append(p, list, new);
        }
        e_dest = if p.tab(tab).is_virtual() { SRT_Table } else { SRT_Upfrom };
    } else if p.tab(tab).is_view() {
        for i in 0..p.tab(tab).n_col() as i32 {
            let e = expr_row_column(p, i);
            list = sqlite3_expr_list_append(p, list, e);
        }
        e_dest = SRT_Table;
    } else {
        e_dest = if p.tab(tab).is_virtual() { SRT_Table } else { SRT_Upfrom };
        let e = sqlite3_p_expr(p, TK_ROW, ExprId::NULL, ExprId::NULL);
        list = sqlite3_expr_list_append(p, ExprListId::NULL, e);
    }
    if changes.nn() {
        for i in 0..p.l(changes).a.len() {
            let x = p.l(changes).a[i].p_expr;
            let d = sqlite3_expr_dup(p, x, 0);
            list = sqlite3_expr_list_append(p, list, d);
        }
    }
    let sel = sqlite3_select_new(
        p,
        list,
        src,
        where2,
        ExprListId::NULL,
        ExprId::NULL,
        ExprListId::NULL,
        SF_UFSrcCheck | SF_IncludeHidden | SF_UpdateFrom,
        ExprId::NULL,
    );
    if sel.nn() {
        p.sm(sel).sel_flags |= SF_OrderByReqd;
    }
    let mut dest = sqlite3_select_dest_init(e_dest, i_eph);
    dest.i_sd_parm2 = if pk.nn() { p.idx(pk).n_key_col as i32 } else { -1 };
    sqlite3_select(p, sel, &mut dest);
}

/// `sqlite3Update(pParse, pTabList, pChanges, pWhere, onError, pOrderBy, pLimit, pUpsert)`:
/// process an UPDATE statement.
///
/// ```text
///   UPDATE OR IGNORE tbl SET a=b, c=d FROM tbl2... WHERE e<5 AND f NOT NULL;
///          \_______/ \_/     \______/      \_____/       \________________/
///           onError   |      pChanges         |                pWhere
///                     \_______________________/
///                               pTabList
/// ```
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_update(
    p: &mut Parse,
    tab_list: SrcListId,
    changes: ExprListId,
    where_: ExprId,
    on_error: i32,
    order_by: ExprListId,
    limit: ExprId,
    upsert: UpsertId,
) {
    let _ = (order_by, limit);
    let mut addr_top = 0; // VDBE instruction address of the start of the loop
    let mut w_info: Option<crate::whereint::WhereInfo> = None; // Information about the WHERE clause
    let mut chng_pk = false; // PRIMARY KEY changed in a WITHOUT ROWID table
    let mut chng_rowid = false; // Rowid changed in a normal table
    let mut row_id_expr = ExprId::NULL; // Expression defining the new record number
    let mut i_rowid_expr: i32 = -1; // Index of "rowid=" (or IPK) assignment in pChanges
    let e_one_pass: i32; // ONEPASS_XXX value from where.c
    let mut tmask = 0; // Mask of TRIGGER_BEFORE|TRIGGER_AFTER
    let mut i_eph = 0; // Ephemeral table holding all primary key values
    let mut n_key = 0; // Number of elements in regKey for WITHOUT ROWID
    let mut ai_cur_one_pass = [-1i32; 2]; // The write cursors opened by WHERE_ONEPASS
    let mut addr_open = 0; // Address of OP_OpenEphemeral
    let mut i_pk = 0; // First of nPk cells holding PRIMARY KEY value
    let mut n_pk: i32 = 0; // Number of components of the PRIMARY KEY
    let mut b_replace = false; // True if REPLACE conflict resolution might happen
    let mut b_finish_seek = true; // The OP_FinishSeek opcode is needed
    let mut reg_row_count = 0; // A count of rows changed
    let mut reg_old = 0; // Content of OLD.* table in triggers
    let mut reg_key = 0; // composite PRIMARY KEY value
    let mut label_continue; // Jump here to continue next step of UPDATE loop

    if p.n_err != 0 {
        return;
    }

    // Locate the table which we want to update.
    let tab = sqlite3_src_list_lookup(p, tab_list);
    if tab.is_null() {
        return;
    }
    let i_db = p.tab(tab).i_db;

    // Figure out if we have any triggers and if the table being updated is a view.
    let trigger = crate::trigger::sqlite3_triggers_exist(p, tab, TK_UPDATE, changes, Some(&mut tmask));
    if p.unsupported.is_some() {
        return;
    }
    let is_view = p.tab(tab).is_view();

    // If there was a FROM clause, set nChangeFrom to the number of expressions in the
    // change-list. Otherwise, set it to 0. There cannot be a FROM clause if this is a
    // virtual table or an UPSERT.
    let n_change_from = if p.src(tab_list).n_src() > 1 { p.l(changes).n_expr() } else { 0 };

    if sqlite3_view_get_column_names(p, tab) != 0 {
        return;
    }
    if sqlite3_is_read_only(p, tab, &trigger) {
        return;
    }

    // Allocate a cursors for the main database table and for all indices. The index
    // cursors might not be used, but if they are used they need to occur right after the
    // database cursor. So go ahead and allocate enough space, just in case.
    let i_base_cur = p.n_tab;
    let mut i_data_cur = p.n_tab;
    p.n_tab += 1;
    let mut i_idx_cur = i_data_cur + 1;
    let pk = if p.tab(tab).has_rowid() { IndexId::NULL } else { sqlite3_primary_key_index(p, tab) };
    let idxs = p.tab(tab).p_index.clone();
    let n_idx = idxs.len();
    for &ix in &idxs {
        if pk == ix {
            i_data_cur = p.n_tab;
        }
        p.n_tab += 1;
    }
    if upsert.nn() {
        // On an UPSERT, reuse the same cursors already opened by INSERT
        i_data_cur = p.up(upsert).i_data_cur;
        i_idx_cur = p.up(upsert).i_idx_cur;
        p.n_tab = i_base_cur;
    }
    p.srcm(tab_list).a[0].i_cursor = i_data_cur;

    // Allocate space for aXRef[], aRegIdx[], and aToOpen[]. Initialize aXRef[] and
    // aToOpen[] to their default values.
    let n_col = p.tab(tab).n_col() as usize;
    let mut a_x_ref: Vec<i32> = vec![-1; n_col];
    let mut a_reg_idx: Vec<i32> = vec![0; n_idx + 1];
    let mut a_to_open: Vec<u8> = vec![1; n_idx + 2];
    a_to_open[n_idx + 1] = 0;

    // Initialize the name-context
    let snc = p.new_nc(NameContext { p_src_list: tab_list, p_upsert: upsert, nc_flags: NC_UUpsert, ..NameContext::default() });

    // Begin generating code.
    sqlite3_get_vdbe(p);

    // Resolve the column names in all the expressions of the of the UPDATE statement. Also
    // find the column index for each column to be updated in the pChanges array. For each
    // column to be updated, make sure we have authorization to change that column.
    for i in 0..p.l(changes).a.len() {
        // If this is an UPDATE with a FROM clause, do not resolve expressions here. The
        // call to sqlite3Select() in updateFromSelect() will do that.
        let e = p.l(changes).a[i].p_expr;
        if n_change_from == 0 && crate::resolve::sqlite3_resolve_expr_names(p, snc, e) != 0 {
            return;
        }
        let z_ename = p.l(changes).a[i].z_e_name.clone().unwrap_or_default();
        let j = crate::resolve::sqlite3_column_index(p, tab, &z_ename);
        if j >= 0 {
            let cf = p.tab(tab).a_col[j as usize].col_flags;
            if j == p.tab(tab).i_p_key as i32 {
                chng_rowid = true;
                row_id_expr = e;
                i_rowid_expr = i as i32;
            } else if pk.nn() && (cf & COLFLAG_PRIMKEY as u16) != 0 {
                chng_pk = true;
            } else if cf & COLFLAG_GENERATED as u16 != 0 {
                let msg = format!("cannot UPDATE generated column \"{}\"", p.tab(tab).a_col[j as usize].z_cn_name);
                p.error_msg(msg);
                return;
            }
            a_x_ref[j as usize] = i as i32;
        } else if pk.is_null() && sqlite3_is_rowid(&z_ename) {
            chng_rowid = true;
            row_id_expr = e;
            i_rowid_expr = i as i32;
        } else {
            p.error_msg(format!("no such column: {z_ename}"));
            p.check_schema = true;
            return;
        }
        // (sqlite3AuthCheck(pParse, SQLITE_UPDATE, ...): no authorizer)
    }
    let chng_key = chng_rowid || chng_pk;

    // Mark generated columns as changing if their generator expressions reference any of
    // the changing columns.
    if p.tab(tab).tab_flags & TF_HasGenerated != 0 {
        loop {
            let mut b_progress = false;
            for i in 0..n_col {
                if a_x_ref[i] >= 0 {
                    continue;
                }
                if p.tab(tab).a_col[i].col_flags & COLFLAG_GENERATED as u16 == 0 {
                    continue;
                }
                let x = crate::build_tab::sqlite3_column_expr(p, tab, i);
                if sqlite3_expr_references_updated_column(p, x, &a_x_ref, chng_rowid) {
                    a_x_ref[i] = 99999;
                    b_progress = true;
                }
            }
            if !b_progress {
                break;
            }
        }
    }

    // The SET expressions are not actually used inside the WHERE loop. So reset the
    // colUsed mask. Unless this is a virtual table. In that case, set all bits of the
    // colUsed mask, to ensure that the virtual table implementation never tries to optimize
    // on the basis that the column is unused.
    p.srcm(tab_list).a[0].col_used = if p.tab(tab).is_virtual() { ALLBITS } else { 0 };

    let has_fk = crate::fkey::sqlite3_fk_required(p, tab, Some(&a_x_ref), chng_key);

    // There is one entry in the aRegIdx[] array for each index on the table being updated.
    // Fill in aRegIdx[] with a register number that will hold the key for that index or to
    // 0 if the index is unchanged.
    if on_error == OE_Replace {
        b_replace = true;
    }
    for (n_all_idx, &ix) in idxs.iter().enumerate() {
        let mut reg = 0;
        if chng_key || has_fk > 1 || ix == pk || index_where_clause_might_change(p, ix, &a_x_ref, chng_rowid) {
            p.n_mem += 1;
            reg = p.n_mem;
            p.n_mem += p.idx(ix).n_column as i32;
        } else {
            for i in 0..p.idx(ix).n_key_col as usize {
                if index_column_is_being_updated(p, ix, i, &a_x_ref, chng_rowid) {
                    p.n_mem += 1;
                    reg = p.n_mem;
                    p.n_mem += p.idx(ix).n_column as i32;
                    if on_error == OE_Default && p.idx(ix).on_error as i32 == OE_Replace {
                        b_replace = true;
                    }
                    break;
                }
            }
        }
        if reg == 0 {
            a_to_open[n_all_idx + 1] = 0;
        }
        a_reg_idx[n_all_idx] = reg;
    }
    p.n_mem += 1;
    a_reg_idx[n_idx] = p.n_mem; // Register storing the table record
    if b_replace {
        // If REPLACE conflict resolution might be invoked, open cursors on all indexes in
        // case they are needed to delete records.
        for x in a_to_open.iter_mut().take(n_idx + 1) {
            *x = 1;
        }
    }

    if p.nested == 0 {
        p.v.change_cnt_on = true;
    }
    sqlite3_begin_write_operation(p, !trigger.is_empty() || has_fk != 0, i_db);

    // Allocate required registers.
    let mut reg_new_rowid = 0;
    let mut reg_old_rowid = 0;
    let mut reg_new = 0;
    let mut reg_row_set = 0;
    if !p.tab(tab).is_virtual() {
        // For now, regRowSet and aRegIdx[nAllIdx] share the same register. If
        // regRowSet is used, the aRegIdx[nAllIdx] register is reassigned to a new register
        // later.
        reg_row_set = a_reg_idx[n_idx];
        p.n_mem += 1;
        reg_old_rowid = p.n_mem;
        reg_new_rowid = reg_old_rowid;
        if chng_pk || !trigger.is_empty() || has_fk != 0 {
            reg_old = p.n_mem + 1;
            p.n_mem += n_col as i32;
        }
        if chng_key || !trigger.is_empty() || has_fk != 0 {
            p.n_mem += 1;
            reg_new_rowid = p.n_mem;
        }
        reg_new = p.n_mem + 1;
        p.n_mem += n_col as i32;
    }

    // If we are trying to update a view, realize that view into an ephemeral table.
    if n_change_from == 0 && is_view {
        sqlite3_materialize_view(p, tab, where_, ExprListId::NULL, ExprId::NULL, i_data_cur);
    }

    // Resolve the column names in all the expressions in the WHERE clause.
    if n_change_from == 0 && crate::resolve::sqlite3_resolve_expr_names(p, snc, where_) != 0 {
        return;
    }

    if p.tab(tab).is_virtual() {
        update_virtual_table(p, tab_list, tab, changes, row_id_expr, &a_x_ref, where_, on_error);
        return;
    }

    // Jump to labelBreak to abandon further processing of this UPDATE
    let label_break = p.v.make_label();
    label_continue = label_break;

    // Not an UPSERT. Normal processing. Begin by initialize the count of updated rows
    if (p.db.flags & SQLITE_CountRows) != 0 && p.p_trigger_tab.is_null() && p.nested == 0 && !p.b_returning && upsert.is_null() {
        p.n_mem += 1;
        reg_row_count = p.n_mem;
        p.v.add_op2(OP_Integer, 0, reg_row_count);
    }

    if n_change_from == 0 && p.tab(tab).has_rowid() {
        p.v.add_op3(OP_Null, 0, reg_row_set, reg_old_rowid);
        i_eph = p.n_tab;
        p.n_tab += 1;
        addr_open = p.v.add_op3(OP_OpenEphemeral, i_eph, 0, reg_row_set);
    } else {
        n_pk = if pk.nn() { p.idx(pk).n_key_col as i32 } else { 0 };
        i_pk = p.n_mem + 1;
        p.n_mem += n_pk;
        p.n_mem += n_change_from;
        p.n_mem += 1;
        reg_key = p.n_mem;
        if upsert.is_null() {
            let n_eph_col = n_pk + n_change_from + if is_view { n_col as i32 } else { 0 };
            i_eph = p.n_tab;
            p.n_tab += 1;
            if pk.nn() {
                p.v.add_op3(OP_Null, 0, i_pk, i_pk + n_pk - 1);
            }
            addr_open = p.v.add_op2(OP_OpenEphemeral, i_eph, n_eph_col);
            if pk.nn() {
                if let Some(mut ki) = sqlite3_key_info_of_index(p, pk) {
                    ki.n_all_field = n_eph_col as u16;
                    p.v.append_p4(P4::KeyInfo(std::sync::Arc::new(ki)));
                }
            }
            if n_change_from != 0 {
                update_from_select(p, i_eph, pk, changes, tab_list, where_);
                if is_view {
                    i_data_cur = i_eph;
                }
            }
        }
    }

    if n_change_from != 0 {
        sqlite3_multi_write(p);
        e_one_pass = ONEPASS_OFF;
        n_key = n_pk;
        reg_key = i_pk;
    } else {
        if upsert.nn() {
            // If this is an UPSERT, then all cursors have already been opened by the outer
            // INSERT and the data cursor should be pointing at the row that is to be
            // updated. So bypass the code that searches for the row(s) to be updated.
            e_one_pass = ONEPASS_SINGLE;
            sqlite3_expr_if_false(p, where_, label_break, SQLITE_JUMPIFNULL);
            b_finish_seek = false;
        } else {
            // Begin the database scan.
            //
            // Do not consider a single-pass strategy for a multi-row update if there is
            // anything that might disrupt the cursor being used to scan the table or index
            // (triggers, FK, rowid or PK changes, REPLACE, subqueries in the WHERE).
            let mut flags = WHERE_ONEPASS_DESIRED as u16;
            if p.nested == 0
                && trigger.is_empty()
                && has_fk == 0
                && !chng_key
                && !b_replace
                && (where_.is_null() || !p.e(where_).has(EP_Subquery))
            {
                flags |= WHERE_ONEPASS_MULTIROW as u16;
            }
            let Some(w) = crate::where_::sqlite3_where_begin(p, tab_list, where_, ExprListId::NULL, ExprListId::NULL, SelectId::NULL, flags, i_idx_cur) else {
                return;
            };

            // A one-pass strategy that might update more than one row may not be used if
            // the index used to scan the table might be modified by the UPDATE.
            let mut eop = crate::where_::sqlite3_where_ok_one_pass(&w, &mut ai_cur_one_pass);
            b_finish_seek = w.b_deferred_seek;
            if eop != ONEPASS_SINGLE {
                sqlite3_multi_write(p);
                if eop == ONEPASS_MULTI {
                    let i_cur = ai_cur_one_pass[1];
                    if i_cur >= 0 && i_cur != i_data_cur && a_to_open[(i_cur - i_base_cur) as usize] != 0 {
                        eop = ONEPASS_OFF;
                    }
                }
            }
            e_one_pass = eop;
            w_info = Some(w);
        }

        if p.tab(tab).has_rowid() {
            // Read the rowid of the current row of the WHERE scan. In ONEPASS_OFF mode this
            // is the rowid of the row being updated, which is added to the RowSet.
            p.v.add_op2(OP_Rowid, i_data_cur, reg_old_rowid);
            if e_one_pass == ONEPASS_OFF {
                p.n_mem += 1;
                a_reg_idx[n_idx] = p.n_mem;
                p.v.add_op3(OP_Insert, i_eph, reg_row_set, reg_old_rowid);
            } else if addr_open != 0 {
                p.v.change_to_noop(addr_open);
            }
        } else {
            // Read the PK of the current row into an array of registers. In ONEPASS_OFF
            // mode, serialize the array into a record and store it in the ephemeral table.
            // Or, in ONEPASS_SINGLE or MULTI mode, change the OP_OpenEphemeral instruction
            // to OP_Noop.
            for i in 0..n_pk {
                let c = p.idx(pk).ai_column[i as usize] as i32;
                sqlite3_expr_code_get_column_of_table(p, tab, i_data_cur, c, i_pk + i);
            }
            if e_one_pass != 0 {
                if addr_open != 0 {
                    p.v.change_to_noop(addr_open);
                }
                n_key = n_pk;
                reg_key = i_pk;
            } else {
                let aff = sqlite3_index_affinity_str(p, pk);
                p.v.add_op4(OP_MakeRecord, i_pk, n_pk, reg_key, P4::Dynamic(aff[..n_pk as usize].to_vec()));
                p.v.add_op4_int(OP_IdxInsert, i_eph, reg_key, i_pk, n_pk);
            }
        }
    }

    if upsert.is_null() {
        if n_change_from == 0 && e_one_pass != ONEPASS_MULTI {
            if let Some(w) = w_info.take() {
                crate::where_::sqlite3_where_end(p, w);
            }
        }

        if !is_view {
            let mut addr_once = 0;
            let mut i_not_used1 = 0;
            let mut i_not_used2 = 0;

            // Open every index that needs updating.
            if e_one_pass != ONEPASS_OFF {
                if ai_cur_one_pass[0] >= 0 {
                    a_to_open[(ai_cur_one_pass[0] - i_base_cur) as usize] = 0;
                }
                if ai_cur_one_pass[1] >= 0 {
                    a_to_open[(ai_cur_one_pass[1] - i_base_cur) as usize] = 0;
                }
            }

            if e_one_pass == ONEPASS_MULTI && (n_idx as i32 - (ai_cur_one_pass[1] >= 0) as i32) > 0 {
                addr_once = p.v.add_op0(OP_Once);
            }
            sqlite3_open_table_and_indices(p, tab, OP_OpenWrite, 0, i_base_cur, Some(&a_to_open), &mut i_not_used1, &mut i_not_used2);
            if addr_once != 0 {
                p.v.jump_here_or_pop_inst(addr_once);
            }
        }

        // Top of the update loop
        if e_one_pass != ONEPASS_OFF {
            if ai_cur_one_pass[0] != i_data_cur && ai_cur_one_pass[1] != i_data_cur {
                p.v.add_op4_int(OP_NotFound, i_data_cur, label_break, reg_key, n_key);
            }
            if e_one_pass != ONEPASS_SINGLE {
                label_continue = p.v.make_label();
            }
            p.v.add_op2(OP_IsNull, if pk.nn() { reg_key } else { reg_old_rowid }, label_break);
        } else if pk.nn() || n_change_from != 0 {
            label_continue = p.v.make_label();
            p.v.add_op2(OP_Rewind, i_eph, label_break);
            addr_top = p.v.current_addr();
            if n_change_from != 0 {
                if !is_view {
                    if pk.nn() {
                        for i in 0..n_pk {
                            p.v.add_op3(OP_Column, i_eph, i, i_pk + i);
                        }
                        p.v.add_op4_int(OP_NotFound, i_data_cur, label_continue, i_pk, n_pk);
                    } else {
                        p.v.add_op2(OP_Rowid, i_eph, reg_old_rowid);
                        p.v.add_op3(OP_NotExists, i_data_cur, label_continue, reg_old_rowid);
                    }
                }
            } else {
                p.v.add_op2(OP_RowData, i_eph, reg_key);
                p.v.add_op4_int(OP_NotFound, i_data_cur, label_continue, reg_key, 0);
            }
        } else {
            p.v.add_op2(OP_Rewind, i_eph, label_break);
            label_continue = p.v.make_label();
            addr_top = p.v.add_op2(OP_Rowid, i_eph, reg_old_rowid);
            p.v.add_op3(OP_NotExists, i_data_cur, label_continue, reg_old_rowid);
        }
    }

    // If the rowid value will change, set register regNewRowid to contain the new value.
    // If the rowid is not being modified, then regNewRowid is the same register as
    // regOldRowid, which is already populated.
    if chng_rowid {
        if n_change_from == 0 {
            sqlite3_expr_code(p, row_id_expr, reg_new_rowid);
        } else {
            p.v.add_op3(OP_Column, i_eph, i_rowid_expr, reg_new_rowid);
        }
        p.v.add_op1(OP_MustBeInt, reg_new_rowid);
        sqlite3_may_abort(p);
    }

    // Compute the old pre-UPDATE content of the row being changed, if that information is
    // needed
    if chng_pk || has_fk != 0 || !trigger.is_empty() {
        let mut oldmask = if has_fk != 0 { crate::fkey::sqlite3_fk_oldmask(p, tab) } else { 0 };
        oldmask |= crate::trigger::sqlite3_trigger_colmask(p, &trigger, changes, 0, TRIGGER_BEFORE | TRIGGER_AFTER, tab, on_error);
        for i in 0..n_col as i32 {
            let col_flags = p.tab(tab).a_col[i as usize].col_flags;
            let k = sqlite3_table_column_to_storage(p.tab(tab), i) + reg_old;
            if oldmask == 0xffff_ffff || (i < 32 && (oldmask & (1u32 << i)) != 0) || (col_flags & COLFLAG_PRIMKEY as u16) != 0 {
                sqlite3_expr_code_get_column_of_table(p, tab, i_data_cur, i, k);
            } else {
                p.v.add_op2(OP_Null, 0, k);
            }
        }
        if !chng_rowid && pk.is_null() {
            p.v.add_op2(OP_Copy, reg_old_rowid, reg_new_rowid);
        }
    }

    // Populate the array of registers beginning at regNew with the new row data. This array
    // is used to check constants, create the new table and index keys, and to pass the new
    // row data to triggers.
    //
    // If there are one or more BEFORE triggers, then do not populate the registers
    // associated with columns that are (a) not modified by this UPDATE statement and (b)
    // not accessed by new.* references. The values for those columns are loaded after the
    // BEFORE triggers are run.
    let newmask = crate::trigger::sqlite3_trigger_colmask(p, &trigger, changes, 1, TRIGGER_BEFORE, tab, on_error);
    let mut k = reg_new;
    for i in 0..n_col as i32 {
        if i == p.tab(tab).i_p_key as i32 {
            p.v.add_op2(OP_Null, 0, k);
        } else if (p.tab(tab).a_col[i as usize].col_flags & COLFLAG_GENERATED as u16) != 0 {
            if p.tab(tab).a_col[i as usize].col_flags & COLFLAG_VIRTUAL as u16 != 0 {
                k -= 1;
            }
        } else {
            let j = a_x_ref[i as usize];
            if j >= 0 {
                if n_change_from != 0 {
                    let n_off = if is_view { n_col as i32 } else { n_pk };
                    p.v.add_op3(OP_Column, i_eph, n_off + j, k);
                } else {
                    let x = p.l(changes).a[j as usize].p_expr;
                    sqlite3_expr_code(p, x, k);
                }
            } else if 0 == (tmask & TRIGGER_BEFORE) || i > 31 || (newmask & (1u32 << i)) != 0 {
                // This branch loads the value of a column that will not be changed into a
                // register. This is done if there are no BEFORE triggers, or if there are
                // one or more BEFORE triggers that use this value via a new.* reference in a
                // trigger program.
                sqlite3_expr_code_get_column_of_table(p, tab, i_data_cur, i, k);
                b_finish_seek = false;
            } else {
                p.v.add_op2(OP_Null, 0, k);
            }
        }
        k += 1;
    }
    if p.tab(tab).tab_flags & TF_HasGenerated != 0 {
        sqlite3_compute_generated_columns(p, reg_new, tab);
    }

    // Fire any BEFORE UPDATE triggers. This happens before constraints are verified. One
    // could argue that this is wrong.
    if tmask & TRIGGER_BEFORE != 0 {
        sqlite3_table_affinity(p, tab, reg_new);
        crate::trigger::sqlite3_code_row_trigger(p, &trigger, TK_UPDATE, changes, TRIGGER_BEFORE, tab, reg_old_rowid, on_error, label_continue);

        if !is_view {
            // The row-trigger may have deleted the row being updated. In this case, jump
            // to the next row. No updates or AFTER triggers are required. This behavior
            // is what happens when the row is deleted by a BEFORE trigger.
            if pk.nn() {
                p.v.add_op4_int(OP_NotFound, i_data_cur, label_continue, reg_key, n_key);
            } else {
                p.v.add_op3(OP_NotExists, i_data_cur, label_continue, reg_old_rowid);
            }

            // After-BEFORE-trigger-reload-loop: if it did not delete it, the BEFORE trigger
            // may still have modified some of the columns of the row being updated. Load
            // the values for all columns not modified by the update statement into their
            // registers in case this has happened. Also reload the generated columns.
            let mut k = reg_new;
            for i in 0..n_col as i32 {
                let cf = p.tab(tab).a_col[i as usize].col_flags;
                if cf & COLFLAG_GENERATED as u16 != 0 {
                    if cf & COLFLAG_VIRTUAL as u16 != 0 {
                        k -= 1;
                    }
                } else if a_x_ref[i as usize] < 0 && i != p.tab(tab).i_p_key as i32 {
                    sqlite3_expr_code_get_column_of_table(p, tab, i_data_cur, i, k);
                }
                k += 1;
            }
            if p.tab(tab).tab_flags & TF_HasGenerated != 0 {
                sqlite3_compute_generated_columns(p, reg_new, tab);
            }
        }
    }

    if !is_view {
        // Do constraint checks.
        sqlite3_generate_constraint_checks(
            p,
            tab,
            &mut a_reg_idx,
            i_data_cur,
            i_idx_cur,
            reg_new_rowid,
            reg_old_rowid,
            chng_key,
            on_error,
            label_continue,
            &mut b_replace,
            Some(&a_x_ref),
            UpsertId::NULL,
        );

        // If REPLACE conflict handling may have been used, or if the PK of the row is
        // changing, then the GenerateConstraintChecks() above may have moved cursor
        // iDataCur. Reseek it.
        if b_replace || chng_key {
            if pk.nn() {
                p.v.add_op4_int(OP_NotFound, i_data_cur, label_continue, reg_key, n_key);
            } else {
                p.v.add_op3(OP_NotExists, i_data_cur, label_continue, reg_old_rowid);
            }
        }

        // Do FK constraint checks.
        if has_fk != 0 {
            crate::fkey::sqlite3_fk_check(p, tab, reg_old_rowid, 0, Some(&a_x_ref), chng_key);
        }

        // Delete the index entries associated with the current record.
        sqlite3_generate_row_index_delete(p, tab, i_data_cur, i_idx_cur, Some(&a_reg_idx), -1);

        // We must run the OP_FinishSeek opcode to resolve a prior OP_DeferredSeek if there
        // is any possibility that there have been no OP_Column opcodes since the
        // OP_DeferredSeek was issued, and the OP_DeferredSeek was issued by the WHERE
        // clause loop.
        if b_finish_seek {
            p.v.add_op1(OP_FinishSeek, i_data_cur);
        }

        // If changing the rowid value, or if there are foreign key constraints to process,
        // delete the old record.
        // (SQLITE_ENABLE_PREUPDATE_HOOK is not part of the build)
        if has_fk > 1 || chng_key {
            p.v.add_op2(OP_Delete, i_data_cur, 0);
        }

        if has_fk != 0 {
            crate::fkey::sqlite3_fk_check(p, tab, 0, reg_new_rowid, Some(&a_x_ref), chng_key);
        }

        // Insert the new index entries and the new record.
        let flags = OPFLAG_ISUPDATE | if e_one_pass == ONEPASS_MULTI { OPFLAG_SAVEPOSITION } else { 0 };
        sqlite3_complete_insertion(p, tab, i_data_cur, i_idx_cur, reg_new_rowid, &a_reg_idx, flags, false, false);

        // Do any ON CASCADE, SET NULL or SET DEFAULT operations required to handle rows
        // (possibly in other tables) that refer via a foreign key to the row just updated.
        if has_fk != 0 {
            crate::fkey::sqlite3_fk_actions(p, tab, changes, reg_old_rowid, Some(&a_x_ref), chng_key);
        }
    }

    // Increment the row counter
    if reg_row_count != 0 {
        p.v.add_op2(OP_AddImm, reg_row_count, 1);
    }

    if !trigger.is_empty() {
        crate::trigger::sqlite3_code_row_trigger(p, &trigger, TK_UPDATE, changes, TRIGGER_AFTER, tab, reg_old_rowid, on_error, label_continue);
    }

    // Repeat the above with the next record to be updated, until all record selected by
    // the WHERE clause have been updated.
    if e_one_pass == ONEPASS_SINGLE {
        // Nothing to do at end-of-loop for a single-pass
    } else if e_one_pass == ONEPASS_MULTI {
        p.v.resolve_label(label_continue);
        if let Some(w) = w_info.take() {
            crate::where_::sqlite3_where_end(p, w);
        }
    } else {
        p.v.resolve_label(label_continue);
        p.v.add_op2(OP_Next, i_eph, addr_top);
    }
    p.v.resolve_label(label_break);

    // Update the sqlite_sequence table by storing the content of the maximum rowid counter
    // values recorded while inserting into autoincrement tables.
    if p.nested == 0 && p.p_trigger_tab.is_null() && upsert.is_null() {
        sqlite3_autoincrement_end(p);
    }

    // Return the number of rows that were changed, if we are tracking that information.
    if reg_row_count != 0 {
        crate::delete::sqlite3_code_change_count(p, reg_row_count, "rows updated");
    }
}

/// `updateVirtualTable(pParse, pSrc, pTab, pChanges, pRowid, aXRef, pWhere, onError)`:
/// generate code for an UPDATE of a virtual table.
#[allow(clippy::too_many_arguments)]
fn update_virtual_table(
    p: &mut Parse,
    src: SrcListId,
    tab: TableId,
    changes: ExprListId,
    rowid: ExprId,
    a_x_ref: &[i32],
    where_: ExprId,
    on_error: i32,
) {
    let vt = crate::vtab::sqlite3_get_vtable(p, tab).map(|v| v.id).unwrap_or(0);
    let n_col = p.tab(tab).n_col() as i32;
    let n_arg = 2 + n_col; // Number of arguments to VUpdate
    let i_csr = p.src(src).a[0].i_cursor; // Cursor used for virtual table scan
    let mut ai_dummy = [-1i32; 2];
    let e_one_pass;
    let mut w_info = None;

    // Allocate nArg registers in which to gather the arguments for VUpdate. Then create
    // and open the ephemeral table in which the records created from these arguments will
    // be temporarily stored.
    let ephem_tab = p.n_tab;
    p.n_tab += 1;
    let mut addr = p.v.add_op2(OP_OpenEphemeral, ephem_tab, n_arg);
    let reg_arg = p.n_mem + 1;
    p.n_mem += n_arg;
    if p.src(src).n_src() > 1 {
        let mut pk = IndexId::NULL;
        let row;
        if p.tab(tab).has_rowid() {
            row = if rowid.nn() { sqlite3_expr_dup(p, rowid, 0) } else { sqlite3_p_expr(p, TK_ROW, ExprId::NULL, ExprId::NULL) };
        } else {
            pk = sqlite3_primary_key_index(p, tab);
            let i_pk = p.idx(pk).ai_column[0] as i32;
            if a_x_ref[i_pk as usize] >= 0 {
                let x = p.l(changes).a[a_x_ref[i_pk as usize] as usize].p_expr;
                row = sqlite3_expr_dup(p, x, 0);
            } else {
                row = expr_row_column(p, i_pk);
            }
        }
        let mut list = sqlite3_expr_list_append(p, ExprListId::NULL, row);
        for i in 0..n_col as usize {
            if a_x_ref[i] >= 0 {
                let x = p.l(changes).a[a_x_ref[i] as usize].p_expr;
                let d = sqlite3_expr_dup(p, x, 0);
                list = sqlite3_expr_list_append(p, list, d);
            } else {
                let re = expr_row_column(p, i as i32);
                if re.nn() {
                    p.em(re).op2 = OPFLAG_NOCHNG as u8;
                }
                list = sqlite3_expr_list_append(p, list, re);
            }
        }
        update_from_select(p, ephem_tab, pk, list, src, where_);
        e_one_pass = ONEPASS_OFF;
    } else {
        p.n_mem += 1;
        let reg_rec = p.n_mem;
        p.n_mem += 1;
        let reg_rowid = p.n_mem;

        // Start scanning the virtual table
        let Some(w) = crate::where_::sqlite3_where_begin(p, src, where_, ExprListId::NULL, ExprListId::NULL, SelectId::NULL, WHERE_ONEPASS_DESIRED as u16, 0) else {
            return;
        };

        // Populate the argument registers.
        for i in 0..n_col {
            if a_x_ref[i as usize] >= 0 {
                let x = p.l(changes).a[a_x_ref[i as usize] as usize].p_expr;
                sqlite3_expr_code(p, x, reg_arg + 2 + i);
            } else {
                p.v.add_op3(OP_VColumn, i_csr, i, reg_arg + 2 + i);
                p.v.change_p5(OPFLAG_NOCHNG as u16); // For sqlite3_vtab_nochange()
            }
        }
        if p.tab(tab).has_rowid() {
            p.v.add_op2(OP_Rowid, i_csr, reg_arg);
            if rowid.nn() {
                sqlite3_expr_code(p, rowid, reg_arg + 1);
            } else {
                p.v.add_op2(OP_Rowid, i_csr, reg_arg + 1);
            }
        } else {
            let pk = sqlite3_primary_key_index(p, tab);
            let i_pk = p.idx(pk).ai_column[0] as i32;
            p.v.add_op3(OP_VColumn, i_csr, i_pk, reg_arg);
            p.v.add_op2(OP_SCopy, reg_arg + 2 + i_pk, reg_arg + 1);
        }

        e_one_pass = crate::where_::sqlite3_where_ok_one_pass(&w, &mut ai_dummy);
        if e_one_pass != 0 {
            // If using the onepass strategy, no-op out the OP_OpenEphemeral coded above.
            // And close the virtual table cursor: the VUpdate will open its own.
            p.v.change_to_noop(addr);
            p.v.add_op1(OP_Close, i_csr);
        } else {
            // Create a record from the argument register contents and insert it into the
            // ephemeral table.
            sqlite3_multi_write(p);
            p.v.add_op3(OP_MakeRecord, reg_arg, n_arg, reg_rec);
            p.v.add_op2(OP_NewRowid, ephem_tab, reg_rowid);
            p.v.add_op3(OP_Insert, ephem_tab, reg_rec, reg_rowid);
        }
        w_info = Some(w);
    }

    if e_one_pass == ONEPASS_OFF {
        // End the virtual table scan
        if p.src(src).n_src() == 1 {
            if let Some(w) = w_info.take() {
                crate::where_::sqlite3_where_end(p, w);
            }
        }

        // Begin scanning through the ephemeral table.
        addr = p.v.add_op1(OP_Rewind, ephem_tab);

        // Extract arguments from the current row of the ephemeral table and invoke the
        // VUpdate method.
        for i in 0..n_arg {
            p.v.add_op3(OP_Column, ephem_tab, i, reg_arg + i);
        }
    }
    crate::vtab::sqlite3_vtab_make_writable(p, tab);
    let name = p.tab(tab).z_name.clone();
    p.v.add_op4(OP_VUpdate, 0, n_arg, reg_arg, P4::Vtab(std::sync::Arc::new(sqliters_vdbe::vdbeaux::P4Vtab { id: vt, name })));
    p.v.change_p5(if on_error == OE_Default { OE_Abort } else { on_error } as u16);
    sqlite3_may_abort(p);

    // End of the ephemeral table scan. Or, if using the onepass strategy, jump back to the
    // OP_VUpdate (the WHERE loop end).
    if e_one_pass == ONEPASS_OFF {
        p.v.add_op2(OP_Next, ephem_tab, addr + 1);
        p.v.jump_here(addr);
        p.v.add_op2(OP_Close, ephem_tab, 0);
    } else if let Some(w) = w_info.take() {
        crate::where_::sqlite3_where_end(p, w);
    }
}
