//! Port of `delete.c`: the DELETE statement (`sqlite3DeleteFrom`), the row and index-entry
//! deletion code shared with INSERT / UPDATE (`sqlite3GenerateRowDelete`,
//! `sqlite3GenerateRowIndexDelete`, `sqlite3GenerateIndexKey`) and the helpers
//! `sqlite3SrcListLookup`, `sqlite3IsReadOnly`, `sqlite3MaterializeView`,
//! `sqlite3CodeChangeCount`.

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{COLNAME_NAME, P4};

use crate::build::*;
use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::schema::*;
use crate::select::*;
use crate::sqliteint::*;
use crate::tk::*;

/// `sqlite3SrcListLookup(pParse, pSrc)`: while a SrcList can in general represent multiple
/// tables and subqueries in a join, the SrcList passed to this routine is the target of an
/// INSERT, UPDATE or DELETE: a single table. Look it up and return it (NULL on error).
pub fn sqlite3_src_list_lookup(p: &mut Parse, src: SrcListId) -> TableId {
    let mut tab = crate::schema::sqlite3_locate_table_item(p, 0, src, 0);
    {
        let it = &mut p.srcm(src).a[0];
        it.p_s_tab = tab;
        it.not_cte = true;
    }
    if tab.nn() {
        p.tabm(tab).n_tab_ref += 1;
        if p.src(src).a[0].is_indexed_by && sqlite3_indexed_by_lookup(p, src, 0) != 0 {
            tab = TableId::NULL;
        }
    }
    tab
}

/// `sqlite3CodeChangeCount(v, regCounter, zColName)`: generate code that will increment the
/// "changes" counter (PRAGMA count_changes) and return it as the result row.
pub fn sqlite3_code_change_count(p: &mut Parse, reg_counter: i32, z_col_name: &str) {
    p.v.add_op0(OP_FkCheck);
    p.v.add_op2(OP_ResultRow, reg_counter, 1);
    p.v.set_num_cols(1);
    p.v.set_col_name(0, COLNAME_NAME, Some(z_col_name));
}

/// `vtabIsReadOnly(pParse, pTab)`: return true if the virtual table pTab cannot be written
/// by this statement.
fn vtab_is_read_only(p: &mut Parse, tab: TableId) -> bool {
    let (has_update, risk) = match crate::vtab::sqlite3_get_vtable(p, tab) {
        Some(vt) => (vt.has_update, vt.e_vtab_risk),
        None => (false, 0),
    };
    if !has_update {
        return true;
    }
    // Within triggers: a virtual table with a high risk factor can only be modified by a
    // trigger or a view if PRAGMA trusted_schema=ON.
    if (p.is_nested_trigger || (p.prep_flags & SQLITE_PREPARE_FROM_DDL as u8) != 0)
        && risk > ((p.db.flags & SQLITE_TrustedSchema) != 0) as u8
    {
        let msg = format!("unsafe use of virtual table \"{}\"", p.tab(tab).z_name);
        p.error_msg(msg);
    }
    false
}

/// `tabIsReadOnly(pParse, pTab)`
fn tab_is_read_only(p: &mut Parse, tab: TableId) -> bool {
    if p.tab(tab).is_virtual() {
        return vtab_is_read_only(p, tab);
    }
    let flags = p.tab(tab).tab_flags;
    if (flags & (TF_Readonly | TF_Shadow)) == 0 {
        return false;
    }
    if (flags & TF_Readonly) != 0 {
        return !sqlite3_writable_schema(p) && p.nested == 0;
    }
    // sqlite3ReadOnlyShadowTables(db): SQLITE_Defensive, no xConnect running, no VDBE
    // running (prepare time), not in an xSync
    (p.db.flags & SQLITE_Defensive) != 0 && p.vtab_ctx.is_empty()
}

/// `sqlite3IsReadOnly(pParse, pTab, pTrigger)`: check to make sure the given table is
/// writable. If it is not writable, generate an error message and return 1. If it is
/// writable return 0.
pub fn sqlite3_is_read_only(p: &mut Parse, tab: TableId, triggers: &[TriggerId]) -> bool {
    if tab_is_read_only(p, tab) {
        let msg = format!("table {} may not be modified", p.tab(tab).z_name);
        p.error_msg(msg);
        return true;
    }
    if p.tab(tab).is_view() && (triggers.is_empty() || (p.trig(triggers[0]).b_returning && triggers.len() == 1)) {
        let msg = format!("cannot modify {} because it is a view", p.tab(tab).z_name);
        p.error_msg(msg);
        return true;
    }
    false
}

/// `sqlite3MaterializeView(pParse, pView, pWhere, pOrderBy, pLimit, iCur)`: evaluate a view
/// and store its result in an ephemeral table. The pWhere argument is an optional WHERE
/// clause that restricts the set of rows in the view that are to be added to the ephemeral
/// table.
pub fn sqlite3_materialize_view(p: &mut Parse, view: TableId, where_: ExprId, order_by: ExprListId, limit: ExprId, i_cur: i32) {
    let i_db = p.tab(view).i_db;
    let where_ = sqlite3_expr_dup(p, where_, 0);
    let from = crate::build::sqlite3_src_list_append(p, SrcListId::NULL, None, None);
    if from.nn() {
        let (name, dbn) = (p.tab(view).z_name.clone(), p.db.a_db[i_db as usize].z_db_s_name.clone());
        let it = &mut p.srcm(from).a[0];
        it.z_name = Some(name);
        it.z_database = Some(dbn);
    }
    let sel = sqlite3_select_new(p, ExprListId::NULL, from, where_, ExprListId::NULL, ExprId::NULL, order_by, SF_IncludeHidden, limit);
    let mut dest = sqlite3_select_dest_init(SRT_EphemTab, i_cur);
    sqlite3_select(p, sel, &mut dest);
}

/// `sqlite3DeleteFrom(pParse, pTabList, pWhere, pOrderBy, pLimit)`: generate code for a
/// DELETE FROM statement.
///
/// ```text
///     DELETE FROM table_name WHERE expr;
///                 \________/       \________/
///                  pTabList              pWhere
/// ```
pub fn sqlite3_delete_from(p: &mut Parse, tab_list: SrcListId, where_: ExprId, order_by: ExprListId, limit: ExprId) {
    let _ = (order_by, limit);
    let mut i_data_cur = 0; // VDBE cursor for the canonical data source
    let mut i_idx_cur = 0; // Cursor number of the first index
    let mut mem_cnt = 0; // Memory cell used for change counting
    let mut ai_cur_one_pass = [-1i32; 2]; // The write cursors opened by WHERE_ONEPASS
    let mut i_pk = 0; // First of nPk registers holding PRIMARY KEY value
    let mut n_pk: i32 = 1; // Number of columns in the PRIMARY KEY
    let mut i_eph_cur = 0; // Ephemeral table holding all primary key values
    let mut i_row_set = 0; // Register for rowset of rows to delete
    let mut addr_bypass = 0; // Address of jump over the delete logic
    let mut addr_loop = 0; // Top of the delete loop
    let mut addr_eph_open = 0; // Instruction to open the Ephemeral table

    if p.n_err != 0 {
        return;
    }

    // Locate the table which we want to delete. This table has to be put in an SrcList
    // structure because some of the subroutines we will be calling are designed to work
    // with multiple tables and expect an SrcList* parameter instead of just a Table*
    // parameter.
    let tab = sqlite3_src_list_lookup(p, tab_list);
    if tab.is_null() {
        return;
    }

    // Figure out if we have any triggers and if the table being deleted from is a view
    let trigger = crate::trigger::sqlite3_triggers_exist(p, tab, TK_DELETE, ExprListId::NULL, None);
    if p.unsupported.is_some() {
        return;
    }
    let is_view = p.tab(tab).is_view();
    let mut b_complex = !trigger.is_empty() || crate::fkey::sqlite3_fk_required(p, tab, None, false) != 0;

    // (sqlite3LimitWhere: SQLITE_ENABLE_UPDATE_DELETE_LIMIT is not part of the reference
    // build; the DELETE grammar has no ORDER BY / LIMIT)

    // If pTab is really a view, make sure it has been initialized.
    if sqlite3_view_get_column_names(p, tab) != 0 {
        return;
    }

    if sqlite3_is_read_only(p, tab, &trigger) {
        return;
    }
    let i_db = p.tab(tab).i_db;
    // (sqlite3AuthCheck(pParse, SQLITE_DELETE, ...): statements of a connection with an
    // authorizer are not given to the generator, rcauth==SQLITE_OK)

    // Assign cursor numbers to the table and all its indices.
    let i_tab_cur = p.n_tab;
    p.srcm(tab_list).a[0].i_cursor = i_tab_cur;
    p.n_tab += 1;
    let n_idx = p.tab(tab).p_index.len() as i32;
    p.n_tab += n_idx;

    // Begin generating code.
    sqlite3_get_vdbe(p);
    // if( pParse->nested==0 ) sqlite3VdbeCountChanges(v);
    sqlite3_begin_write_operation(p, b_complex, i_db);

    // If we are trying to delete from a view, realize that view into an ephemeral table.
    if is_view {
        sqlite3_materialize_view(p, tab, where_, ExprListId::NULL, ExprId::NULL, i_tab_cur);
        i_data_cur = i_tab_cur;
        i_idx_cur = i_tab_cur;
    }

    // Resolve the column names in the WHERE clause.
    let snc = p.new_nc(NameContext { p_src_list: tab_list, ..NameContext::default() });
    if crate::resolve::sqlite3_resolve_expr_names(p, snc, where_) != 0 {
        return;
    }

    // Initialize the counter of the number of rows deleted, if we are counting rows.
    if (p.db.flags & SQLITE_CountRows) != 0 && p.nested == 0 && p.p_trigger_tab.is_null() && !p.b_returning {
        p.n_mem += 1;
        mem_cnt = p.n_mem;
        p.v.add_op2(OP_Integer, 0, mem_cnt);
    }

    // Special case: A DELETE without a WHERE clause deletes everything. It is easier just to
    // erase the whole table.
    if where_.is_null() && !b_complex && !p.tab(tab).is_virtual() {
        debug_assert!(!is_view);
        let (tnum, z_name, has_rowid) = {
            let t = p.tab(tab);
            (t.tnum, t.z_name.clone(), t.has_rowid())
        };
        sqlite3_table_lock(p, i_db, tnum, 1, &z_name);
        if has_rowid {
            p.v.add_op4(OP_Clear, tnum as i32, i_db, if mem_cnt != 0 { mem_cnt } else { -1 }, P4::dynamic(&z_name));
        }
        for ix in p.tab(tab).p_index.clone() {
            let it = p.idx(ix).tnum as i32;
            if p.idx(ix).is_primary_key() && !has_rowid {
                p.v.add_op3(OP_Clear, it, i_db, if mem_cnt != 0 { mem_cnt } else { -1 });
            } else {
                p.v.add_op2(OP_Clear, it, i_db);
            }
        }
    } else {
        let mut wcf = (WHERE_ONEPASS_DESIRED | WHERE_DUPLICATES_OK) as u16;
        if p.nc(snc).nc_flags & NC_Subquery != 0 {
            b_complex = true;
        }
        wcf |= if b_complex { 0 } else { WHERE_ONEPASS_MULTIROW as u16 };
        let pk;
        if p.tab(tab).has_rowid() {
            // For a rowid table, initialize the RowSet to an empty set
            pk = IndexId::NULL;
            p.n_mem += 1;
            i_row_set = p.n_mem;
            p.v.add_op2(OP_Null, 0, i_row_set);
        } else {
            // For a WITHOUT ROWID table, create an ephemeral table used to hold all primary
            // keys for rows to be deleted.
            pk = sqlite3_primary_key_index(p, tab);
            n_pk = p.idx(pk).n_key_col as i32;
            i_pk = p.n_mem + 1;
            p.n_mem += n_pk;
            i_eph_cur = p.n_tab;
            p.n_tab += 1;
            addr_eph_open = p.v.add_op2(OP_OpenEphemeral, i_eph_cur, n_pk);
            sqlite3_vdbe_set_p4_key_info(p, pk);
        }

        // Construct a query to find the rowid or primary key for every row to be deleted,
        // based on the WHERE clause. Set variable eOnePass to indicate the type of
        // ONEPASS strategy, if any.
        let Some(w) = crate::where_::sqlite3_where_begin(p, tab_list, where_, ExprListId::NULL, ExprListId::NULL, SelectId::NULL, wcf, i_tab_cur + 1) else {
            return;
        };
        let e_one_pass = crate::where_::sqlite3_where_ok_one_pass(&w, &mut ai_cur_one_pass);
        if e_one_pass != ONEPASS_SINGLE {
            sqlite3_multi_write(p);
        }
        if w.b_deferred_seek {
            p.v.add_op1(OP_FinishSeek, i_tab_cur);
        }

        // Keep track of the number of rows to be deleted
        if mem_cnt != 0 {
            p.v.add_op2(OP_AddImm, mem_cnt, 1);
        }

        // Extract the rowid or primary key for the current row
        let i_key;
        if pk.nn() {
            for i in 0..n_pk {
                let c = p.idx(pk).ai_column[i as usize] as i32;
                sqlite3_expr_code_get_column_of_table(p, tab, i_tab_cur, c, i_pk + i);
            }
            i_key = i_pk;
        } else {
            p.n_mem += 1;
            i_key = p.n_mem;
            sqlite3_expr_code_get_column_of_table(p, tab, i_tab_cur, -1, i_key);
        }

        let mut a_to_open: Vec<u8> = Vec::new();
        let mut w_opt = Some(w);
        let n_key: i32;
        let mut i_key = i_key;
        if e_one_pass != ONEPASS_OFF {
            // For ONEPASS, no need to store the rowid/primary-key. There is only one, so just
            // keep it in its register(s) and fall through to the delete code below.
            n_key = n_pk; // OP_Found will use an unpacked key
            a_to_open = vec![1u8; n_idx as usize + 2];
            a_to_open[n_idx as usize + 1] = 0;
            if ai_cur_one_pass[0] >= 0 {
                a_to_open[(ai_cur_one_pass[0] - i_tab_cur) as usize] = 0;
            }
            if ai_cur_one_pass[1] >= 0 {
                a_to_open[(ai_cur_one_pass[1] - i_tab_cur) as usize] = 0;
            }
            if addr_eph_open != 0 {
                p.v.change_to_noop(addr_eph_open);
            }
            addr_bypass = p.v.make_label();
        } else {
            if pk.nn() {
                // Add the PK key for this row to the temporary table
                p.n_mem += 1;
                i_key = p.n_mem;
                n_key = 0; // Zero tells OP_Found to use a composite key
                let aff = sqlite3_index_affinity_str(p, pk);
                p.v.add_op4(OP_MakeRecord, i_pk, n_pk, i_key, P4::Dynamic(aff[..n_pk as usize].to_vec()));
                p.v.add_op4_int(OP_IdxInsert, i_eph_cur, i_key, i_pk, n_pk);
            } else {
                // Add the rowid of the row to be deleted to the RowSet
                n_key = 1; // OP_DeferredSeek always uses a single rowid
                p.v.add_op2(OP_RowSetAdd, i_row_set, i_key);
            }
            crate::where_::sqlite3_where_end(p, w_opt.take().expect("where"));
        }

        // Unless this is a view, open cursors for the table we are deleting from and all its
        // indices. If this is a view, then the only effect this statement has is to fire the
        // INSTEAD OF triggers.
        if !is_view {
            let mut i_addr_once = 0;
            if e_one_pass == ONEPASS_MULTI {
                i_addr_once = p.v.add_op0(OP_Once);
            }
            let ato = if a_to_open.is_empty() { None } else { Some(a_to_open.as_slice()) };
            sqlite3_open_table_and_indices(p, tab, OP_OpenWrite, OPFLAG_FORDELETE as u8, i_tab_cur, ato, &mut i_data_cur, &mut i_idx_cur);
            if e_one_pass == ONEPASS_MULTI {
                p.v.jump_here_or_pop_inst(i_addr_once);
            }
        }

        // Set up a loop over the rowids/primary-keys that were found in the where-clause
        // loop above.
        if e_one_pass != ONEPASS_OFF {
            if !p.tab(tab).is_virtual() && a_to_open[(i_data_cur - i_tab_cur) as usize] != 0 {
                p.v.add_op4_int(OP_NotFound, i_data_cur, addr_bypass, i_key, n_key);
            }
        } else if pk.nn() {
            addr_loop = p.v.add_op1(OP_Rewind, i_eph_cur);
            if p.tab(tab).is_virtual() {
                p.v.add_op3(OP_Column, i_eph_cur, 0, i_key);
            } else {
                p.v.add_op2(OP_RowData, i_eph_cur, i_key);
            }
        } else {
            addr_loop = p.v.add_op3(OP_RowSetRead, i_row_set, 0, i_key);
        }

        // Delete the row
        if p.tab(tab).is_virtual() {
            let vt = crate::vtab::sqlite3_get_vtable(p, tab).map(|v| v.id).unwrap_or(0);
            crate::vtab::sqlite3_vtab_make_writable(p, tab);
            sqlite3_may_abort(p);
            if e_one_pass == ONEPASS_SINGLE {
                p.v.add_op1(OP_Close, i_tab_cur);
                if p.is_toplevel() {
                    p.is_multi_write = false;
                }
            }
            let name = p.tab(tab).z_name.clone();
            p.v.add_op4(OP_VUpdate, 0, 1, i_key, P4::Vtab(std::sync::Arc::new(sqliters_vdbe::vdbeaux::P4Vtab { id: vt, name })));
            p.v.change_p5(OE_Abort as u16);
        } else {
            let count = p.nested == 0;
            sqlite3_generate_row_delete(p, tab, &trigger, i_data_cur, i_idx_cur, i_key, n_key, count, OE_Default as u8, e_one_pass as u8, ai_cur_one_pass[1]);
        }

        // End of the loop over all rowids/primary-keys.
        if e_one_pass != ONEPASS_OFF {
            p.v.resolve_label(addr_bypass);
            crate::where_::sqlite3_where_end(p, w_opt.take().expect("where"));
        } else if pk.nn() {
            p.v.add_op2(OP_Next, i_eph_cur, addr_loop + 1);
            p.v.jump_here(addr_loop);
        } else {
            p.v.goto(addr_loop);
            p.v.jump_here(addr_loop);
        }
    } // End non-truncate path

    // Update the sqlite_sequence table by storing the content of the maximum rowid counter
    // values recorded while inserting into autoincrement tables.
    if p.nested == 0 && p.p_trigger_tab.is_null() {
        crate::insert::sqlite3_autoincrement_end(p);
    }

    // Return the number of rows that were deleted. If this routine is generating code
    // because of a call to sqlite3NestedParse(), do not invoke the callback function.
    if mem_cnt != 0 {
        sqlite3_code_change_count(p, mem_cnt, "rows deleted");
    }
}

/// `sqlite3GenerateRowDelete(pParse, pTab, pTrigger, iDataCur, iIdxCur, iPk, nPk, count,
/// onconf, eMode, iIdxNoSeek)`: this routine generates VDBE code that causes a single row of
/// a single table to be deleted. Both the original table entry and all indices are removed.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_generate_row_delete(
    p: &mut Parse,
    tab: TableId,
    trigger: &[TriggerId],
    i_data_cur: i32,
    i_idx_cur: i32,
    i_pk: i32,
    n_pk: i32,
    count: bool,
    onconf: u8,
    e_mode: u8,
    mut i_idx_no_seek: i32,
) {
    let mut i_old = 0; // First register in OLD.* array

    // Seek cursor iCur to the row to delete. If this row no longer exists (this can happen
    // if a trigger program has already deleted it), do not attempt to delete it or fire any
    // DELETE triggers.
    let i_label = p.v.make_label();
    let op_seek = if p.tab(tab).has_rowid() { OP_NotExists } else { OP_NotFound };
    if e_mode as i32 == ONEPASS_OFF {
        p.v.add_op4_int(op_seek, i_data_cur, i_label, i_pk, n_pk);
    }

    // If there are any triggers to fire, allocate a range of registers to use for the
    // old.* references in the triggers.
    if crate::fkey::sqlite3_fk_required(p, tab, None, false) != 0 || !trigger.is_empty() {
        // Populate the OLD.* pseudo-table register array. These values will be used by any
        // BEFORE and AFTER triggers that exist.
        let mut mask = crate::trigger::sqlite3_trigger_colmask(p, trigger, ExprListId::NULL, 0, TRIGGER_BEFORE | TRIGGER_AFTER, tab, onconf as i32);
        mask |= crate::fkey::sqlite3_fk_oldmask(p, tab);
        i_old = p.n_mem + 1;
        p.n_mem += 1 + p.tab(tab).n_col() as i32;

        // Populate the OLD.* pseudo-table register array. These values will be used by any
        // BEFORE and AFTER triggers that exist.
        p.v.add_op2(OP_Copy, i_pk, i_old);
        for i_col in 0..p.tab(tab).n_col() as i32 {
            if mask == 0xffff_ffff || (i_col <= 31 && (mask & (1u32 << i_col)) != 0) {
                let kk = sqlite3_table_column_to_storage(p.tab(tab), i_col);
                sqlite3_expr_code_get_column_of_table(p, tab, i_data_cur, i_col, i_old + kk + 1);
            }
        }

        // Invoke BEFORE DELETE trigger programs.
        let addr_start = p.v.current_addr();
        crate::trigger::sqlite3_code_row_trigger(p, trigger, TK_DELETE, ExprListId::NULL, TRIGGER_BEFORE, tab, i_old, onconf as i32, i_label);

        // If any BEFORE triggers were coded, then seek the cursor to the row to be deleted
        // again. It may be that the BEFORE triggers moved the cursor or of the row to be
        // deleted was already deleted.
        if addr_start < p.v.current_addr() {
            p.v.add_op4_int(op_seek, i_data_cur, i_label, i_pk, n_pk);
            i_idx_no_seek = -1;
        }

        // Do FK processing. This call checks that any FK constraints that refer to this
        // table (i.e. constraints attached to other tables) are not violated by deleting
        // this row.
        crate::fkey::sqlite3_fk_check(p, tab, i_old, 0, None, false);
    }

    // Delete the index and table entries. Skip this step if pTab is really a view (in which
    // case the only effect of this call is to fire the INSTEAD OF triggers).
    if !p.tab(tab).is_view() {
        let mut p5: u16 = 0;
        sqlite3_generate_row_index_delete(p, tab, i_data_cur, i_idx_cur, None, i_idx_no_seek);
        p.v.add_op2(OP_Delete, i_data_cur, if count { OPFLAG_NCHANGE } else { 0 });
        if p.nested == 0 || p.tab(tab).z_name.eq_ignore_ascii_case("sqlite_stat1") {
            let p4 = p4_table_of(p, tab);
            p.v.append_p4(p4);
        }
        if e_mode as i32 != ONEPASS_OFF {
            p.v.change_p5(OPFLAG_AUXDELETE as u16);
        }
        if i_idx_no_seek >= 0 && i_idx_no_seek != i_data_cur {
            p.v.add_op1(OP_Delete, i_idx_no_seek);
        }
        if e_mode as i32 == ONEPASS_MULTI {
            p5 |= OPFLAG_SAVEPOSITION as u16;
        }
        p.v.change_p5(p5);
    }

    // Do any ON CASCADE, SET NULL or SET DEFAULT operations required to handle rows (possibly
    // in other tables) that refer via a foreign key to the row just deleted.
    crate::fkey::sqlite3_fk_actions(p, tab, ExprListId::NULL, i_old, None, false);

    // Invoke AFTER DELETE trigger programs.
    if !trigger.is_empty() {
        crate::trigger::sqlite3_code_row_trigger(p, trigger, TK_DELETE, ExprListId::NULL, TRIGGER_AFTER, tab, i_old, onconf as i32, i_label);
    }

    // Jump here if the row had already been deleted before any BEFORE trigger programs were
    // invoked. Or if a trigger program throws a RAISE(IGNORE) exception.
    p.v.resolve_label(i_label);
}

/// `sqlite3GenerateRowIndexDelete(pParse, pTab, iDataCur, iIdxCur, aRegIdx, iIdxNoSeek)`:
/// this routine generates VDBE code that causes the deletion of all index entries
/// associated with a single row of a single table, pTab.
pub fn sqlite3_generate_row_index_delete(p: &mut Parse, tab: TableId, i_data_cur: i32, i_idx_cur: i32, a_reg_idx: Option<&[i32]>, i_idx_no_seek: i32) {
    let mut r1 = -1; // Register holding an index key
    let mut prior = IndexId::NULL; // Prior index
    let pk = if p.tab(tab).has_rowid() { IndexId::NULL } else { sqlite3_primary_key_index(p, tab) };
    let idxs = p.tab(tab).p_index.clone();
    for (i, &ix) in idxs.iter().enumerate() {
        let mut p3 = 0;
        if let Some(a) = a_reg_idx {
            if a[i] == 0 {
                continue;
            }
        }
        if ix == pk {
            continue;
        }
        if i_idx_cur + i as i32 == i_idx_no_seek {
            continue;
        }
        let mut i_part_idx_label = 0;
        r1 = sqlite3_generate_index_key_full(p, ix, i_data_cur, 0, true, Some(&mut i_part_idx_label), prior, r1);
        if p.idx(ix).b_has_expr {
            if let Some(a) = a_reg_idx {
                p3 = a[i];
            }
        }
        p.v.add_op3(OP_IdxDelete, i_idx_cur + i as i32, r1, p3);
        let p4 = p4_index_of(p, ix);
        p.v.change_p4(-1, p4);
        let p5 = if p.idx(ix).uniq_not_null { p.idx(ix).n_key_col } else { p.idx(ix).n_column };
        p.v.change_p5(p5);
        sqlite3_resolve_part_idx_label(p, i_part_idx_label);
        prior = ix;
    }
}

/// `sqlite3GenerateIndexKey(pParse, pIdx, iDataCur, regOut, prefixOnly, piPartIdxLabel,
/// pPrior, regPrior)`: generate code that will assemble an index key and stores it in
/// register regOut. The key with be for index pIdx which is an index on pTab. iCur is the
/// index of a cursor open on the pTab table and pointing to the entry that needs indexing.
/// If pTab is a WITHOUT ROWID table, then iCur must be the cursor of the PRIMARY KEY index.
///
/// If `pi_part_idx_label` is given and pIdx is a partial index, a jump to the returned label
/// skips the key when the row is not in the index. `prior` / `reg_prior`: a previous key
/// built from the same row in the same registers (columns it shares need no reload).
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_generate_index_key_full(
    p: &mut Parse,
    idx: IndexId,
    i_data_cur: i32,
    reg_out: i32,
    prefix_only: bool,
    pi_part_idx_label: Option<&mut i32>,
    mut prior: IndexId,
    reg_prior: i32,
) -> i32 {
    if let Some(lbl) = pi_part_idx_label {
        let part = p.idx(idx).p_part_idx_where;
        if part.nn() {
            *lbl = p.v.make_label();
            p.i_self_tab = i_data_cur + 1;
            sqlite3_expr_if_false_dup(p, part, *lbl, SQLITE_JUMPIFNULL);
            p.i_self_tab = 0;
            prior = IndexId::NULL; // pPrior is not valid for a partial index
        } else {
            *lbl = 0;
        }
    }
    let n_col = {
        let i = p.idx(idx);
        if prefix_only && i.uniq_not_null { i.n_key_col as i32 } else { i.n_column as i32 }
    };
    let reg_base = sqlite3_get_temp_range(p, n_col);
    if prior.nn() && (reg_base != reg_prior || p.idx(prior).p_part_idx_where.nn()) {
        prior = IndexId::NULL;
    }
    for j in 0..n_col {
        if prior.nn() {
            let (a, b) = (p.idx(prior).ai_column.get(j as usize).copied(), p.idx(idx).ai_column[j as usize]);
            if a == Some(b) && b as i32 != XN_EXPR {
                // This column was already computed by the previous index
                continue;
            }
        }
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

/// `sqlite3ResolvePartIdxLabel(pParse, iLabel)`: if a prior call to sqlite3GenerateIndexKey()
/// generated a jump-around label for a partial index, then resolve that label.
pub fn sqlite3_resolve_part_idx_label(p: &mut Parse, i_label: i32) {
    if i_label != 0 {
        p.v.resolve_label(i_label);
    }
}

/// `sqlite3OpenTableAndIndices(pParse, pTab, op, p5, iBase, aToOpen, piDataCur, piIdxCur)`
/// (insert.c): allocate cursors for the pTab table and all its indices and generate code to
/// open and initialized those cursors. Return the number of indices on the table.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_open_table_and_indices(
    p: &mut Parse,
    tab: TableId,
    op: u8,
    mut p5: u8,
    mut i_base: i32,
    a_to_open: Option<&[u8]>,
    pi_data_cur: &mut i32,
    pi_idx_cur: &mut i32,
) -> i32 {
    if p.tab(tab).is_virtual() {
        // This routine is a no-op for virtual tables. Leave the output variables set to
        // illegal values.
        *pi_data_cur = -999;
        *pi_idx_cur = -999;
        return 0;
    }
    let i_db = p.tab(tab).i_db;
    if i_base < 0 {
        i_base = p.n_tab;
    }
    let i_data_cur = i_base;
    i_base += 1;
    *pi_data_cur = i_data_cur;
    if p.tab(tab).has_rowid() && a_to_open.is_none_or(|a| a[0] != 0) {
        sqlite3_open_table(p, i_data_cur, i_db, tab, op);
    } else {
        let (tnum, z_name) = (p.tab(tab).tnum, p.tab(tab).z_name.clone());
        sqlite3_table_lock(p, i_db, tnum, (op == OP_OpenWrite) as u8, &z_name);
    }
    *pi_idx_cur = i_base;
    let idxs = p.tab(tab).p_index.clone();
    let has_rowid = p.tab(tab).has_rowid();
    for (i, &ix) in idxs.iter().enumerate() {
        let i_idx_cur = i_base;
        i_base += 1;
        if p.idx(ix).is_primary_key() && !has_rowid {
            *pi_data_cur = i_idx_cur;
            p5 = 0;
        }
        if a_to_open.is_none_or(|a| a[i + 1] != 0) {
            let tnum = p.idx(ix).tnum as i32;
            p.v.add_op3(op, i_idx_cur, tnum, i_db);
            sqlite3_vdbe_set_p4_key_info(p, ix);
            p.v.change_p5(p5 as u16);
        }
    }
    if i_base > p.n_tab {
        p.n_tab = i_base;
    }
    idxs.len() as i32
}
