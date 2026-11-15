//! Port of `insert.c`: the INSERT statement (`sqlite3Insert`, the transfer optimization
//! `xferOptimization`), AUTOINCREMENT (`sqlite3AutoincrementBegin` / `End`), the constraint
//! checks and row insertion shared with UPDATE (`sqlite3GenerateConstraintChecks`,
//! `sqlite3CompleteInsertion`), table affinity (`sqlite3TableAffinity`) and generated
//! columns (`sqlite3ComputeGeneratedColumns`).

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{VdbeOpList, P4};

use crate::build::*;
use crate::consts::*;
use crate::delete::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::schema::*;
use crate::select::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::upsert::*;
use crate::walker::*;

/// `sqlite3TableAffinityStr(db, pTab)`: compute an affinity string for a table. Space is
/// obtained from sqlite3DbMalloc(). The caller is responsible for freeing the space when
/// done. Trailing BLOB (and NONE) affinities are omitted.
pub fn sqlite3_table_affinity_str(p: &Parse, tab: TableId) -> Vec<u8> {
    let t = p.tab(tab);
    let mut z: Vec<u8> = t.a_col.iter().filter(|c| c.col_flags & COLFLAG_VIRTUAL as u16 == 0).map(|c| c.affinity).collect();
    while z.last().is_some_and(|&a| a <= SQLITE_AFF_BLOB) {
        z.pop();
    }
    z
}

/// `sqlite3TableAffinity(v, pTab, iReg)`: make changes to the evolving bytecode to do
/// affinity transformations on the values of a row of table pTab held in registers
/// starting at iReg (`iReg==0`: the affinity goes into the P4 of the OP_MakeRecord just
/// coded). STRICT tables get an OP_TypeCheck instead.
pub fn sqlite3_table_affinity(p: &mut Parse, tab: TableId, i_reg: i32) {
    if p.tab(tab).tab_flags & TF_Strict != 0 {
        if i_reg == 0 {
            // Move the previous opcode (which should be OP_MakeRecord) forward by one slot
            // and insert a new OP_TypeCheck where the current OP_MakeRecord is.
            let p4 = p4_table_of(p, tab);
            p.v.append_p4(p4);
            let (p1, p2, p3) = {
                let prev = p.v.get_last_op_mut();
                prev.opcode = OP_TypeCheck;
                let p3 = prev.p3;
                prev.p3 = 0;
                (prev.p1, prev.p2, p3)
            };
            p.v.add_op3(OP_MakeRecord, p1, p2, p3);
        } else {
            // Insert an isolated OP_Typecheck
            let n = p.tab(tab).n_nv_col as i32;
            p.v.add_op2(OP_TypeCheck, i_reg, n);
            let p4 = p4_table_of(p, tab);
            p.v.append_p4(p4);
        }
        return;
    }
    let z = sqlite3_table_affinity_str(p, tab);
    let i = z.len() as i32;
    if i != 0 {
        if i_reg != 0 {
            p.v.add_op4(OP_Affinity, i_reg, i, 0, P4::Dynamic(z));
        } else {
            p.v.change_p4(-1, P4::Dynamic(z));
        }
    }
}

/// `readsTable(p, iDb, pTab)`: return non-zero if the table pTab in database iDb or any of
/// its indices have been opened at any point in the VDBE program. This is used to see if a
/// statement of the form "INSERT INTO <iDb, pTab> SELECT ..." can run for the results of the
/// SELECT.
fn reads_table(p: &Parse, i_db: i32, tab: TableId) -> bool {
    let vt = if p.tab(tab).is_virtual() { crate::vtab::sqlite3_get_vtable(p, tab).map(|v| v.id) } else { None };
    let i_end = p.v.current_addr();
    for i in 1..i_end {
        let op = p.v.get_op(i);
        if op.opcode == OP_OpenRead && op.p3 == i_db {
            let tnum = op.p2 as u32;
            if tnum == p.tab(tab).tnum {
                return true;
            }
            for &ix in &p.tab(tab).p_index {
                if tnum == p.idx(ix).tnum {
                    return true;
                }
            }
        }
        if op.opcode == OP_VOpen {
            if let (P4::Vtab(x), Some(id)) = (&op.p4, vt) {
                if x.id == id {
                    return true;
                }
            }
        }
    }
    false
}

/// `exprColumnFlagUnion(pWalker, pExpr)`: this walker callback will compute the union of
/// colFlags flags for all referenced columns in a CHECK constraint or generated column
/// expression.
fn expr_column_flag_union(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).op == TK_COLUMN && p.e(e).i_column >= 0 {
        let c = p.e(e).i_column as usize;
        w.e_code |= p.tab(w.p_tab).a_col[c].col_flags;
    }
    WRC_Continue
}

/// `sqlite3ComputeGeneratedColumns(pParse, iRegStore, pTab)`: all regular columns for
/// table pTab have been puts into registers starting with iRegStore. The registers that
/// correspond to STORED or VIRTUAL columns have not yet been initialized. This routine goes
/// back and computes the values for those columns based on the previously computed normal
/// columns.
pub fn sqlite3_compute_generated_columns(p: &mut Parse, i_reg_store: i32, tab: TableId) {
    // Before computing generated columns, first go through and make sure that appropriate
    // affinity has been applied to the regular columns
    sqlite3_table_affinity(p, tab, i_reg_store);
    if (p.tab(tab).tab_flags & TF_HasStored) != 0 {
        let last = p.v.current_addr() - 1;
        let opcode = p.v.get_op(last).opcode;
        if opcode == OP_Affinity {
            // Change the OP_Affinity argument to '@' (NONE) for all stored columns. '@'
            // is the affinity of no-affinity.
            let cols: Vec<u16> = p.tab(tab).a_col.iter().map(|c| c.col_flags).collect();
            if let P4::Dynamic(z) = &mut p.v.get_op_mut(last).p4 {
                let mut jj = 0usize;
                let mut ii = 0usize;
                while jj < z.len() && z[jj] != 0 {
                    if cols[ii] & COLFLAG_VIRTUAL as u16 != 0 {
                        ii += 1;
                        continue;
                    }
                    if cols[ii] & COLFLAG_STORED as u16 != 0 {
                        z[jj] = SQLITE_AFF_NONE;
                    }
                    jj += 1;
                    ii += 1;
                }
            }
        } else if opcode == OP_TypeCheck {
            // If an OP_TypeCheck was generated because the table is STRICT, then set P3 so
            // that the type-check is skipped for generated columns.
            p.v.get_op_mut(last).p3 = 1;
        }
    }

    // Because there can be multiple generated columns that refer to one another, this
    // needs to be a loop that continues until all columns have been computed.
    for i in 0..p.tab(tab).a_col.len() {
        if p.tab(tab).a_col[i].col_flags & COLFLAG_GENERATED as u16 != 0 {
            p.tabm(tab).a_col[i].col_flags |= COLFLAG_NOTAVAIL as u16;
        }
    }
    let mut w = Walker { x_expr_callback: Some(expr_column_flag_union), p_tab: tab, ..Walker::default() };
    // On the second pass, compute the value of each NOT-AVAILABLE column. Companion code in
    // the TK_COLUMN case of sqlite3ExprCodeTarget() will compute the value of earlier
    // VIRTUAL generated columns.
    p.i_self_tab = -i_reg_store;
    let mut redo: Option<usize>;
    loop {
        let mut e_progress = false;
        redo = None;
        for i in 0..p.tab(tab).a_col.len() {
            if p.tab(tab).a_col[i].col_flags & COLFLAG_NOTAVAIL as u16 != 0 {
                p.tabm(tab).a_col[i].col_flags |= COLFLAG_BUSY as u16;
                w.e_code = 0;
                let x = crate::build_tab::sqlite3_column_expr(p, tab, i);
                sqlite3_walk_expr(p, &mut w, x);
                p.tabm(tab).a_col[i].col_flags &= !(COLFLAG_BUSY as u16);
                if w.e_code & COLFLAG_NOTAVAIL as u16 != 0 {
                    redo = Some(i);
                    continue;
                }
                e_progress = true;
                let x = sqlite3_table_column_to_storage(p.tab(tab), i as i32) + i_reg_store;
                sqlite3_expr_code_generated_column(p, tab, i, x);
                p.tabm(tab).a_col[i].col_flags &= !(COLFLAG_NOTAVAIL as u16);
            }
        }
        if !(redo.is_some() && e_progress) {
            break;
        }
    }
    if let Some(r) = redo {
        let msg = format!("generated column loop on \"{}\"", p.tab(tab).a_col[r].z_cn_name);
        p.error_msg(msg);
    }
    p.i_self_tab = 0;
}

/// `autoIncBegin(pParse, iDb, pTab)`: locate or create an AutoincInfo structure associated
/// with table pTab. Return the register number for the memory cell that holds the
/// autoincrement counter (0 if the table has no AUTOINCREMENT).
fn auto_inc_begin(p: &mut Parse, i_db: i32, tab: TableId) -> i32 {
    let mut mem_id = 0;
    if (p.tab(tab).tab_flags & TF_Autoincrement) != 0 && (p.db.m_db_flags & DBFLAG_Vacuum as u32) == 0 {
        let seq = p.a_schema.get(i_db as usize).map(|s| s.p_seq_tab).unwrap_or(TableId::NULL);
        // Verify that the sqlite_sequence table exists and is an ordinary rowid table with
        // exactly two columns. Corrupt the database if it is not.
        if seq.is_null() || !p.tab(seq).has_rowid() || p.tab(seq).is_virtual() || p.tab(seq).n_col() != 2 {
            p.n_err += 1;
            p.rc = SQLITE_CORRUPT_SEQUENCE;
            p.not_supported("corrupt sqlite_sequence table");
            return 0;
        }
        // (sqlite3ParseToplevel(pParse): AUTOINCREMENT inside a trigger program uses the
        // counters of the top-level statement)
        if !p.uses_ainc {
            p.p_ainc.clear();
        }
        let pos = p.p_ainc.iter().position(|a| a.p_tab == tab);
        let info = match pos {
            Some(k) => p.p_ainc[k].clone(),
            None => {
                let top = crate::trigger::toplevel_n_mem(p);
                *top += 1; // Register to hold name of table
                *top += 1;
                let reg_ctr = *top; // Max rowid register
                *top += 2; // Rowid in sqlite_sequence + orig max val
                let info = AutoincInfo { p_tab: tab, i_db, reg_ctr };
                p.p_ainc.insert(0, info.clone());
                p.uses_ainc = true;
                info
            }
        };
        mem_id = info.reg_ctr;
    }
    mem_id
}

/// `sqlite3AutoincrementBegin(pParse)`: this routine generates code that will initialize all
/// of the register used by the autoincrement tracker.
pub fn sqlite3_autoincrement_begin(p: &mut Parse) {
    const AUTO_INC: [(u8, i8, i8, i8); 12] = [
        (OP_Null, 0, 0, 0),
        (OP_Rewind, 0, 10, 0),
        (OP_Column, 0, 0, 0),
        (OP_Ne, 0, 9, 0),
        (OP_Rowid, 0, 0, 0),
        (OP_Column, 0, 1, 0),
        (OP_AddImm, 0, 0, 0),
        (OP_Copy, 0, 0, 0),
        (OP_Goto, 0, 11, 0),
        (OP_Next, 0, 2, 0),
        (OP_Integer, 0, 0, 0),
        (OP_Close, 0, 0, 0),
    ];
    for k in 0..p.p_ainc.len() {
        let info = p.p_ainc[k].clone();
        let mem_id = info.reg_ctr;
        let seq = p.a_schema[info.i_db as usize].p_seq_tab;
        sqlite3_open_table(p, 0, info.i_db, seq, OP_OpenRead);
        let name = p.tab(info.p_tab).z_name.clone();
        p.v.load_string(mem_id - 1, &name);
        let list: Vec<VdbeOpList> = AUTO_INC.iter().map(|&(opcode, p1, p2, p3)| VdbeOpList { opcode, p1, p2, p3 }).collect();
        let a = p.v.add_op_list(&list);
        p.v.get_op_mut(a).p2 = mem_id;
        p.v.get_op_mut(a).p3 = mem_id + 2;
        p.v.get_op_mut(a + 2).p3 = mem_id;
        p.v.get_op_mut(a + 3).p1 = mem_id - 1;
        p.v.get_op_mut(a + 3).p3 = mem_id;
        p.v.get_op_mut(a + 3).p5 = SQLITE_JUMPIFNULL as u16;
        p.v.get_op_mut(a + 4).p2 = mem_id + 1;
        p.v.get_op_mut(a + 5).p3 = mem_id;
        p.v.get_op_mut(a + 6).p1 = mem_id;
        p.v.get_op_mut(a + 7).p2 = mem_id + 2;
        p.v.get_op_mut(a + 7).p1 = mem_id;
        p.v.get_op_mut(a + 10).p2 = mem_id;
        if p.n_tab == 0 {
            p.n_tab = 1;
        }
    }
}

/// `autoIncStep(pParse, memId, regRowid)`: update the maximum rowid for an autoincrement
/// calculation.
fn auto_inc_step(p: &mut Parse, mem_id: i32, reg_rowid: i32) {
    if mem_id > 0 {
        p.v.add_op2(OP_MemMax, mem_id, reg_rowid);
    }
}

/// `sqlite3AutoincrementEnd(pParse)`: this routine generates the code needed to write
/// autoincrement maximum rowid values back into the sqlite_sequence register.
pub fn sqlite3_autoincrement_end(p: &mut Parse) {
    if !p.uses_ainc {
        return;
    }
    const AUTO_INC_END: [(u8, i8, i8, i8); 5] =
        [(OP_NotNull, 0, 2, 0), (OP_NewRowid, 0, 0, 0), (OP_MakeRecord, 0, 2, 0), (OP_Insert, 0, 0, 0), (OP_Close, 0, 0, 0)];
    for k in 0..p.p_ainc.len() {
        let info = p.p_ainc[k].clone();
        let mem_id = info.reg_ctr;
        let i_rec = sqlite3_get_temp_reg(p);
        let here = p.v.current_addr();
        p.v.add_op3(OP_Le, mem_id + 2, here + 7, mem_id);
        let seq = p.a_schema[info.i_db as usize].p_seq_tab;
        sqlite3_open_table(p, 0, info.i_db, seq, OP_OpenWrite);
        let list: Vec<VdbeOpList> = AUTO_INC_END.iter().map(|&(opcode, p1, p2, p3)| VdbeOpList { opcode, p1, p2, p3 }).collect();
        let a = p.v.add_op_list(&list);
        p.v.get_op_mut(a).p1 = mem_id + 1;
        p.v.get_op_mut(a + 1).p2 = mem_id + 1;
        p.v.get_op_mut(a + 2).p1 = mem_id - 1;
        p.v.get_op_mut(a + 2).p3 = i_rec;
        p.v.get_op_mut(a + 3).p2 = i_rec;
        p.v.get_op_mut(a + 3).p3 = mem_id + 1;
        p.v.get_op_mut(a + 3).p5 = OPFLAG_APPEND as u16;
        sqlite3_release_temp_reg(p, i_rec);
    }
}

/// `sqlite3Insert(pParse, pTabList, pSelect, pColumn, onError, pUpsert)`: this routine is
/// called to handle SQL of the following forms:
///
/// ```text
///    insert into TABLE (IDLIST) values(EXPRLIST),(EXPRLIST),...
///    insert into TABLE (IDLIST) select
///    insert into TABLE (IDLIST) default values
/// ```
pub fn sqlite3_insert(p: &mut Parse, tab_list: SrcListId, select: SelectId, column: IdListId, on_error: i32, upsert: UpsertId) {
    let mut select = select;
    let mut n_hidden = 0; // Number of hidden columns if TABLE is virtual
    let mut i_data_cur = 0; // VDBE cursor that is the main data repository
    let mut i_idx_cur = 0; // First index cursor
    let mut ipk_column: i32 = -1; // Column that is the INTEGER PRIMARY KEY
    let mut src_tab = 0; // Data comes from this temporary cursor if >=0
    let mut addr_ins_top = 0; // Jump to label "D"
    let mut addr_cont = 0; // Top of insert loop. Label "C" in templates 3 and 4
    let mut dest = SelectDest::default(); // Destination for SELECT on rhs of INSERT
    let mut use_temp_table = false; // Store SELECT results in intermediate table
    let mut append_flag = false; // True if the insert is likely to be an append
    let mut list = ExprListId::NULL; // List of VALUES() to be inserted
    let mut reg_from_select = 0; // Base register for data coming from SELECT
    let mut reg_row_count = 0; // Memory cell used for the row counter
    let mut a_reg_idx: Vec<i32> = Vec::new(); // One register allocated to each index
    let mut a_tab_col_map: Vec<i32> = Vec::new(); // Mapping from pTab columns to pCol entries
    let n_column: i32; // Number of columns in the data
    let mut tmask = 0; // Mask of trigger times

    if p.n_err != 0 {
        return;
    }
    dest.i_sd_parm = 0; // Suppress a harmless compiler warning

    // If the Select object is really just a simple VALUES() list with a single row (the
    // common case) then keep that one row of values and discard the other (unused) parts of
    // the pSelect object
    if select.nn() && (p.s(select).sel_flags & SF_Values) != 0 && p.s(select).p_prior.is_null() {
        list = p.s(select).p_e_list;
        p.sm(select).p_e_list = ExprListId::NULL;
        select = SelectId::NULL;
    }

    // Locate the table into which we will be inserting new information.
    let tab = sqlite3_src_list_lookup(p, tab_list);
    if tab.is_null() {
        return;
    }
    let i_db = p.tab(tab).i_db;
    // (sqlite3AuthCheck(pParse, SQLITE_INSERT, ...): no authorizer)
    let without_rowid = !p.tab(tab).has_rowid();

    // Figure out if we have any triggers and if the table being inserted into is a view
    let trigger = crate::trigger::sqlite3_triggers_exist(p, tab, TK_INSERT, ExprListId::NULL, Some(&mut tmask));
    if p.unsupported.is_some() {
        return;
    }
    let is_view = p.tab(tab).is_view();

    // If pTab is really a view, make sure it has been initialized. ViewGetColumnNames() is a
    // no-op if pTab is not a view.
    if sqlite3_view_get_column_names(p, tab) != 0 {
        return;
    }

    // Cannot insert into a read-only table.
    if sqlite3_is_read_only(p, tab, &trigger) {
        return;
    }

    // Allocate a VDBE
    sqlite3_get_vdbe(p);
    // if( pParse->nested==0 ) sqlite3VdbeCountChanges(v);
    sqlite3_begin_write_operation(p, select.nn() || !trigger.is_empty(), i_db);

    // If the statement is of the form
    //
    //       INSERT INTO <table1> SELECT * FROM <table2>;
    //
    // Then special optimizations can be applied that make the transfer very fast and which
    // reduce fragmentation of indices.
    let mut xfer_done = false;
    if column.is_null() && select.nn() && trigger.is_empty() && xfer_optimization(p, tab, select, on_error, i_db) {
        xfer_done = true;
    }
    if p.unsupported.is_some() {
        return;
    }

    if !xfer_done {
        // If this is an AUTOINCREMENT table, look up the sequence number in the
        // sqlite_sequence table and store it in memory cell regAutoinc.
        let reg_autoinc = auto_inc_begin(p, i_db, tab);
        if p.unsupported.is_some() {
            return;
        }

        // Allocate a block registers to hold the rowid and the values for all columns of
        // the new row.
        let mut reg_rowid = p.n_mem + 1;
        let mut reg_ins = reg_rowid;
        p.n_mem += p.tab(tab).n_col() as i32 + 1;
        if p.tab(tab).is_virtual() {
            reg_rowid += 1;
            p.n_mem += 1;
        }
        let mut reg_data = reg_rowid + 1;

        // If the INSERT statement included an IDLIST term, then make sure all elements of the
        // IDLIST really are columns of the table and remember the column indices.
        //
        // If the table has an INTEGER PRIMARY KEY column and that column is named in the
        // IDLIST, then record in the ipkColumn variable the index into IDLIST of the primary
        // key column. ipkColumn is the index of the primary key as it appears in IDLIST, not
        // as is appears in the original table. (The index of the INTEGER PRIMARY KEY in the
        // original table is pTab->iPKey.) Note: if the table is a WITHOUT ROWID table then
        // ipkColumn is always -1.
        let mut b_id_list_in_order = (p.tab(tab).tab_flags & (TF_OOOHidden | TF_HasStored)) == 0;
        if column.nn() {
            a_tab_col_map = vec![0; p.tab(tab).a_col.len()];
            let names = p.a_idlist[column.ix()].a.clone();
            for (i, name) in names.iter().enumerate() {
                let j = crate::resolve::sqlite3_column_index(p, tab, name);
                if j >= 0 {
                    if a_tab_col_map[j as usize] == 0 {
                        a_tab_col_map[j as usize] = i as i32 + 1;
                    }
                    if i as i32 != j {
                        b_id_list_in_order = false;
                    }
                    if j == p.tab(tab).i_p_key as i32 {
                        ipk_column = i as i32;
                    }
                    let cf = p.tab(tab).a_col[j as usize].col_flags;
                    if cf & (COLFLAG_STORED | COLFLAG_VIRTUAL) as u16 != 0 {
                        let msg = format!("cannot INSERT into generated column \"{}\"", p.tab(tab).a_col[j as usize].z_cn_name);
                        p.error_msg(msg);
                        return;
                    }
                } else if sqlite3_is_rowid(name) && !without_rowid {
                    ipk_column = i as i32;
                    b_id_list_in_order = false;
                } else {
                    let msg = format!("table {} has no column named {}", crate::wherecode::src_item_name(p, tab_list, 0, false), name);
                    p.error_msg(msg);
                    p.check_schema = true;
                    return;
                }
            }
        }

        // Figure out how many columns of data are supplied. If the data is coming from a
        // SELECT statement, then generate a co-routine that produces a single row of the
        // SELECT on each invocation. The co-routine is the common header to the 3rd and 4th
        // templates.
        if select.nn() {
            // Data is coming from a SELECT or from a multi-row VALUES clause. Generate a
            // co-routine to run the SELECT.
            let src = p.s(select).p_src;
            if p.src(src).n_src() == 1 && p.src(src).a[0].via_coroutine && p.s(select).p_prior.is_null() {
                let (reg_return, reg_result, sub_sel) = {
                    let sq = p.src(src).a[0].p_subq.as_ref().expect("subquery");
                    (sq.reg_return, sq.reg_result, sq.p_select)
                };
                dest.i_sd_parm = reg_return;
                reg_from_select = reg_result;
                n_column = p.l(p.s(sub_sel).p_e_list).n_expr();
                let name = crate::wherecode::src_item_name(p, src, 0, false);
                crate::build::sqlite3_vdbe_explain(p, false, format!("SCAN {name}"));
                if b_id_list_in_order && n_column == p.tab(tab).n_col() as i32 {
                    reg_data = reg_from_select;
                    reg_rowid = reg_data - 1;
                    reg_ins = reg_rowid - if p.tab(tab).is_virtual() { 1 } else { 0 };
                }
            } else {
                let reg_yield = p.n_mem + 1;
                p.n_mem += 1;
                let addr_top = p.v.current_addr() + 1;
                p.v.add_op3(OP_InitCoroutine, reg_yield, 0, addr_top);
                dest = sqlite3_select_dest_init(SRT_Coroutine, reg_yield);
                dest.i_sdst = if b_id_list_in_order { reg_data } else { 0 };
                dest.n_sdst = p.tab(tab).n_col() as i32;
                let rc = sqlite3_select(p, select, &mut dest);
                reg_from_select = dest.i_sdst;
                if rc != 0 || p.n_err != 0 {
                    return;
                }
                p.v.end_coroutine(reg_yield);
                sqlite3_clear_temp_reg_cache(p);
                p.v.jump_here(addr_top - 1); // label B:
                n_column = p.l(p.s(select).p_e_list).n_expr();
            }

            // Set useTempTable to TRUE if the result of the SELECT statement should be written
            // into a temporary table (template 4). Set to FALSE if each output row of the
            // SELECT can be written directly into the destination table (template 3).
            //
            // A temp table must be used if the table being updated is also one of the tables
            // being read by the SELECT statement. Also use a temp table in the case of
            // row triggers.
            if !trigger.is_empty() || reads_table(p, i_db, tab) {
                use_temp_table = true;
            }

            if use_temp_table {
                // Invoke the coroutine to extract information from the SELECT and add it to a
                // transient table srcTab.
                src_tab = p.n_tab;
                p.n_tab += 1;
                let reg_rec = sqlite3_get_temp_reg(p);
                let reg_temp_rowid = sqlite3_get_temp_reg(p);
                p.v.add_op2(OP_OpenEphemeral, src_tab, n_column);
                let addr_l = p.v.add_op1(OP_Yield, dest.i_sd_parm);
                p.v.add_op3(OP_MakeRecord, reg_from_select, n_column, reg_rec);
                p.v.add_op2(OP_NewRowid, src_tab, reg_temp_rowid);
                p.v.add_op3(OP_Insert, src_tab, reg_rec, reg_temp_rowid);
                p.v.goto(addr_l);
                p.v.jump_here(addr_l);
                sqlite3_release_temp_reg(p, reg_rec);
                sqlite3_release_temp_reg(p, reg_temp_rowid);
            }
        } else {
            // This is the case if the data for the INSERT is coming from a single-row VALUES
            // clause
            let snc = p.new_nc(NameContext::default());
            src_tab = -1;
            if list.nn() {
                n_column = p.l(list).n_expr();
                if crate::resolve::sqlite3_resolve_expr_list_names(p, snc, list) != 0 {
                    return;
                }
            } else {
                n_column = 0;
            }
        }

        // If there is no IDLIST term but the table has an integer primary key, the set the
        // ipkColumn variable to the integer primary key column index in the original table
        // definition.
        if column.is_null() && n_column > 0 {
            ipk_column = p.tab(tab).i_p_key as i32;
            if ipk_column >= 0 && (p.tab(tab).tab_flags & TF_HasGenerated) != 0 {
                let mut i = ipk_column - 1;
                while i >= 0 {
                    if p.tab(tab).a_col[i as usize].col_flags & COLFLAG_GENERATED as u16 != 0 {
                        ipk_column -= 1;
                    }
                    i -= 1;
                }
            }

            // Make sure the number of columns in the source data matches the number of
            // columns to be inserted into the table.
            if (p.tab(tab).tab_flags & (TF_HasGenerated | TF_HasHidden)) != 0 {
                for i in 0..p.tab(tab).a_col.len() {
                    if p.tab(tab).a_col[i].col_flags & COLFLAG_NOINSERT as u16 != 0 {
                        n_hidden += 1;
                    }
                }
            }
            if n_column != p.tab(tab).n_col() as i32 - n_hidden {
                let msg = format!(
                    "table {} has {} columns but {} values were supplied",
                    crate::wherecode::src_item_name(p, tab_list, 0, false),
                    p.tab(tab).n_col() as i32 - n_hidden,
                    n_column
                );
                p.error_msg(msg);
                return;
            }
        }
        if column.nn() && n_column != p.a_idlist[column.ix()].a.len() as i32 {
            let msg = format!("{} values for {} columns", n_column, p.a_idlist[column.ix()].a.len());
            p.error_msg(msg);
            return;
        }

        // Initialize the count of rows to be inserted
        if (p.db.flags & SQLITE_CountRows) != 0 && p.nested == 0 && p.p_trigger_tab.is_null() && !p.b_returning {
            p.n_mem += 1;
            reg_row_count = p.n_mem;
            p.v.add_op2(OP_Integer, 0, reg_row_count);
        }

        // If this is not a view, open the table and and all indices
        if !is_view {
            let n_idx = sqlite3_open_table_and_indices(p, tab, OP_OpenWrite, 0, -1, None, &mut i_data_cur, &mut i_idx_cur);
            a_reg_idx = vec![0; n_idx as usize + 2];
            let idxs = p.tab(tab).p_index.clone();
            for i in 0..n_idx as usize {
                p.n_mem += 1;
                a_reg_idx[i] = p.n_mem;
                p.n_mem += p.idx(idxs[i]).n_column as i32;
            }
            p.n_mem += 1;
            a_reg_idx[n_idx as usize] = p.n_mem; // Register to store the table record
        }
        if upsert.nn() {
            if p.tab(tab).is_virtual() {
                let msg = format!("UPSERT not implemented for virtual table \"{}\"", p.tab(tab).z_name);
                p.error_msg(msg);
                return;
            }
            if p.tab(tab).is_view() {
                p.error_msg("cannot UPSERT a view");
                return;
            }
            let target = p.up(upsert).p_upsert_target;
            if crate::build_tab::sqlite3_has_explicit_nulls(p, target) {
                return;
            }
            p.srcm(tab_list).a[0].i_cursor = i_data_cur;
            let mut nx = upsert;
            loop {
                {
                    let u = p.upm(nx);
                    u.p_upsert_src = tab_list;
                    u.reg_data = reg_data;
                    u.i_data_cur = i_data_cur;
                    u.i_idx_cur = i_idx_cur;
                }
                if p.up(nx).p_upsert_target.nn() && sqlite3_upsert_analyze_target(p, tab_list, nx, upsert) != 0 {
                    return;
                }
                nx = p.up(nx).p_next_upsert;
                if nx.is_null() {
                    break;
                }
            }
        }

        // This is the top of the main insertion loop
        if use_temp_table {
            // This block codes the top of loop only. The complete loop is the following
            // pseudocode (template 4):
            //
            //         rewind temp table, if empty goto D
            //      C: loop over rows of intermediate table
            //           transfer values form intermediate table into <table>
            //         end loop
            //      D: ...
            addr_ins_top = p.v.add_op1(OP_Rewind, src_tab);
            addr_cont = p.v.current_addr();
        } else if select.nn() {
            // This block codes the top of loop only. The complete loop is the following
            // pseudocode (template 3):
            //
            //      C: yield X, at EOF goto D
            //         insert the select result into <table> from R..R+n
            //         goto C
            //      D: ...
            addr_ins_top = p.v.add_op1(OP_Yield, dest.i_sd_parm);
            addr_cont = addr_ins_top;
            if ipk_column >= 0 {
                // tag-20191021-001: If the INTEGER PRIMARY KEY is being generated by the
                // SELECT statement, then a copy of the INTEGER PRIMARY KEY value is made into
                // regRowid.
                p.v.add_op2(OP_Copy, reg_from_select + ipk_column, reg_rowid);
            }
        }

        // Compute data for ordinary columns of the new entry. Values are written in storage
        // order into registers starting with regData. Only ordinary columns are computed in
        // this loop. The rowid (if there is one) is computed later and generated columns are
        // computed after the rowid since they might depend on the value of the rowid.
        n_hidden = 0;
        let mut i_reg_store = reg_data;
        let n_col = p.tab(tab).n_col() as i32;
        let mut i = 0;
        while i < n_col {
            let k;
            if i == p.tab(tab).i_p_key as i32 {
                // tag-20191021-002: References to the INTEGER PRIMARY KEY are filled
                // in using the rowid from regRowid
                p.v.add_op1(OP_SoftNull, i_reg_store);
                i += 1;
                i_reg_store += 1;
                continue;
            }
            let col_flags = p.tab(tab).a_col[i as usize].col_flags;
            if (col_flags & COLFLAG_NOINSERT as u16) != 0 {
                n_hidden += 1;
                if (col_flags & COLFLAG_VIRTUAL as u16) != 0 {
                    // Virtual columns do not participate in OP_MakeRecord. So back up
                    // iRegStore by one slot to compensate for the iRegStore++ in the outer
                    // for() loop
                    i_reg_store -= 1;
                    i += 1;
                    i_reg_store += 1;
                    continue;
                } else if (col_flags & COLFLAG_STORED as u16) != 0 {
                    // Stored columns are computed later. But if there are BEFORE triggers,
                    // the slots used for stored columns will be OP_Copy-ed to an array for
                    // the trigger. So make sure the stored columns are initialized to NULL.
                    if tmask & TRIGGER_BEFORE != 0 {
                        p.v.add_op1(OP_SoftNull, i_reg_store);
                    }
                    i += 1;
                    i_reg_store += 1;
                    continue;
                } else if column.is_null() {
                    // Hidden columns that are not explicitly named in the INSERT get their
                    // default value
                    let x = crate::build_tab::sqlite3_column_expr(p, tab, i as usize);
                    sqlite3_expr_code_factorable(p, x, i_reg_store);
                    i += 1;
                    i_reg_store += 1;
                    continue;
                }
            }
            if column.nn() {
                let j = a_tab_col_map[i as usize];
                if j == 0 {
                    // A column not named in the insert column list gets its default value
                    let x = crate::build_tab::sqlite3_column_expr(p, tab, i as usize);
                    sqlite3_expr_code_factorable(p, x, i_reg_store);
                    i += 1;
                    i_reg_store += 1;
                    continue;
                }
                k = j - 1;
            } else if n_column == 0 {
                // This is INSERT INTO ... DEFAULT VALUES. Load the default value.
                let x = crate::build_tab::sqlite3_column_expr(p, tab, i as usize);
                sqlite3_expr_code_factorable(p, x, i_reg_store);
                i += 1;
                i_reg_store += 1;
                continue;
            } else {
                k = i - n_hidden;
            }

            if use_temp_table {
                p.v.add_op3(OP_Column, src_tab, k, i_reg_store);
            } else if select.nn() {
                if reg_from_select != reg_data {
                    p.v.add_op2(OP_SCopy, reg_from_select + k, i_reg_store);
                }
            } else {
                let x = p.l(list).a[k as usize].p_expr;
                let y = sqlite3_expr_code_target(p, x, i_reg_store);
                if y != i_reg_store {
                    let op = if p.e(x).has(EP_Subquery) { OP_Copy } else { OP_SCopy };
                    p.v.add_op2(op, y, i_reg_store);
                }
            }
            i += 1;
            i_reg_store += 1;
        }

        // Run the BEFORE and INSTEAD OF triggers, if there are any
        let end_of_loop = p.v.make_label();
        if tmask & TRIGGER_BEFORE != 0 {
            let reg_cols = sqlite3_get_temp_range(p, n_col + 1);

            // build the NEW.* reference row. Note that if there is an INTEGER PRIMARY KEY
            // into which a NULL is being inserted, that NULL will be translated into a
            // unique ID for the row. But on a BEFORE trigger, we do not know what the unique
            // ID will be (because the insert has not happened yet) so we substitute a rowid
            // of -1
            if ipk_column < 0 {
                p.v.add_op2(OP_Integer, -1, reg_cols);
            } else {
                if use_temp_table {
                    p.v.add_op3(OP_Column, src_tab, ipk_column, reg_cols);
                } else {
                    let x = p.l(list).a[ipk_column as usize].p_expr;
                    sqlite3_expr_code(p, x, reg_cols);
                }
                let addr1 = p.v.add_op1(OP_NotNull, reg_cols);
                p.v.add_op2(OP_Integer, -1, reg_cols);
                p.v.jump_here(addr1);
                p.v.add_op1(OP_MustBeInt, reg_cols);
                sqlite3_may_abort(p);
            }

            // Copy the new data already generated.
            let nnv = p.tab(tab).n_nv_col as i32;
            p.v.add_op3(OP_Copy, reg_rowid + 1, reg_cols + 1, nnv - 1);

            // Compute the new value for generated columns after all other law values have
            // been computed.
            if p.tab(tab).tab_flags & TF_HasGenerated != 0 {
                sqlite3_compute_generated_columns(p, reg_cols + 1, tab);
            }

            // If this is an INSERT on a view with an INSTEAD OF INSERT trigger, do not
            // attempt any conversions before assembling the record. If this is a real
            // table, attempt conversions as required by the table column affinities.
            if !is_view {
                sqlite3_table_affinity(p, tab, reg_cols + 1);
            }

            // Fire BEFORE or INSTEAD OF triggers
            crate::trigger::sqlite3_code_row_trigger(p, &trigger, TK_INSERT, ExprListId::NULL, TRIGGER_BEFORE, tab, reg_cols - n_col - 1, on_error, end_of_loop);

            sqlite3_release_temp_range(p, reg_cols, n_col + 1);
        }

        if !is_view {
            if p.tab(tab).is_virtual() {
                // The row that the VUpdate opcode will delete: none
                p.v.add_op2(OP_Null, 0, reg_ins);
            }
            if ipk_column >= 0 {
                // Compute the new rowid
                if use_temp_table {
                    p.v.add_op3(OP_Column, src_tab, ipk_column, reg_rowid);
                } else if select.nn() {
                    // Rowid already initialized at tag-20191021-001
                } else {
                    let ipk = p.l(list).a[ipk_column as usize].p_expr;
                    if p.e(ipk).op == TK_NULL && !p.tab(tab).is_virtual() {
                        p.v.add_op3(OP_NewRowid, i_data_cur, reg_rowid, reg_autoinc);
                        append_flag = true;
                    } else {
                        sqlite3_expr_code(p, ipk, reg_rowid);
                    }
                }
                // If the PRIMARY KEY expression is NULL, then use OP_NewRowid to generate a
                // unique primary key value.
                if !append_flag {
                    if !p.tab(tab).is_virtual() {
                        let addr1 = p.v.add_op1(OP_NotNull, reg_rowid);
                        p.v.add_op3(OP_NewRowid, i_data_cur, reg_rowid, reg_autoinc);
                        p.v.jump_here(addr1);
                    } else {
                        let addr1 = p.v.current_addr();
                        p.v.add_op2(OP_IsNull, reg_rowid, addr1 + 2);
                    }
                    p.v.add_op1(OP_MustBeInt, reg_rowid);
                    sqlite3_may_abort(p);
                }
            } else if p.tab(tab).is_virtual() || without_rowid {
                p.v.add_op2(OP_Null, 0, reg_rowid);
            } else {
                p.v.add_op3(OP_NewRowid, i_data_cur, reg_rowid, reg_autoinc);
                append_flag = true;
            }
            auto_inc_step(p, reg_autoinc, reg_rowid);

            // Compute the new value for generated columns after all other law values have
            // been computed.
            if p.tab(tab).tab_flags & TF_HasGenerated != 0 {
                sqlite3_compute_generated_columns(p, reg_rowid + 1, tab);
            }

            // Generate code to check constraints and generate index keys and do the insertion.
            if p.tab(tab).is_virtual() {
                let vt = crate::vtab::sqlite3_get_vtable(p, tab).map(|v| v.id).unwrap_or(0);
                crate::vtab::sqlite3_vtab_make_writable(p, tab);
                let name = p.tab(tab).z_name.clone();
                p.v.add_op4(OP_VUpdate, 1, n_col + 2, reg_ins, P4::Vtab(std::sync::Arc::new(sqliters_vdbe::vdbeaux::P4Vtab { id: vt, name })));
                p.v.change_p5(if on_error == OE_Default { OE_Abort } else { on_error } as u16);
                sqlite3_may_abort(p);
            } else {
                let mut is_replace = false; // Set to true if constraints may cause a replace
                sqlite3_generate_constraint_checks(
                    p, tab, &mut a_reg_idx, i_data_cur, i_idx_cur, reg_ins, 0, ipk_column >= 0, on_error, end_of_loop, &mut is_replace, None, upsert,
                );
                if p.db.flags & SQLITE_ForeignKeys != 0 {
                    crate::fkey::sqlite3_fk_check(p, tab, 0, reg_ins, None, false);
                }

                // Set the OPFLAG_USESEEKRESULT flag if either (a) there are no REPLACE
                // constraints or (b) there are no triggers and this table is not a parent
                // table in a foreign key constraint. It is safe to set the
                // OPFLAG_USESEEKRESULT flag in this case because the cursor used for the
                // OP_Insert has been left pointing at the conflicting row and so seeking is
                // not required.
                // sqlite3VdbeHasSubProgram(v): sub-programs are linked to the top-level
                // program (`pToplevel->pVdbe`) when their coding starts
                let has_sub_program = p.is_toplevel() && !p.sub_programs.is_empty();
                let b_use_seek = !is_replace || !has_sub_program;
                sqlite3_complete_insertion(p, tab, i_data_cur, i_idx_cur, reg_ins, &a_reg_idx, 0, append_flag, b_use_seek);
            }
        }

        // Update the count of rows that are inserted
        if reg_row_count != 0 {
            p.v.add_op2(OP_AddImm, reg_row_count, 1);
        }

        if !trigger.is_empty() {
            // Code AFTER triggers
            crate::trigger::sqlite3_code_row_trigger(p, &trigger, TK_INSERT, ExprListId::NULL, TRIGGER_AFTER, tab, reg_data - 2 - n_col, on_error, end_of_loop);
        }

        // The bottom of the main insertion loop, if the data source is a SELECT statement.
        p.v.resolve_label(end_of_loop);
        if use_temp_table {
            p.v.add_op2(OP_Next, src_tab, addr_cont);
            p.v.jump_here(addr_ins_top);
            p.v.add_op1(OP_Close, src_tab);
        } else if select.nn() {
            p.v.goto(addr_cont);
            p.v.jump_here(addr_ins_top);
        }
    }

    // insert_end:
    // Update the sqlite_sequence table by storing the content of the maximum rowid counter
    // values recorded while inserting into autoincrement tables.
    if p.nested == 0 && p.p_trigger_tab.is_null() {
        sqlite3_autoincrement_end(p);
    }

    // Return the number of rows inserted. If this routine is generating code because of a
    // call to sqlite3NestedParse(), do not invoke the callback function.
    if reg_row_count != 0 {
        sqlite3_code_change_count(p, reg_row_count, "rows inserted");
    }
}

/// `checkConstraintExprNode(pWalker, pExpr)`: this is the Walker callback from
/// sqlite3ExprReferencesUpdatedColumn(). Set bit 0x01 of pWalker->eCode if pWalker->eCode
/// to 0 if this expression node references any of the columns that are being changed by an
/// UPDATE statement.
fn check_constraint_expr_node(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).op == TK_COLUMN {
        let c = p.e(e).i_column;
        if c >= 0 {
            if w.v_int[c as usize] >= 0 {
                w.e_code |= 0x01; // CKCNSTRNT_COLUMN
            }
        } else {
            w.e_code |= 0x02; // CKCNSTRNT_ROWID
        }
    }
    WRC_Continue
}

/// `sqlite3ExprReferencesUpdatedColumn(pExpr, aiChng, chngRowid)`: pExpr is a CHECK
/// constraint on a row that is being UPDATE-ed. The UPDATE does not cover all columns of
/// the row. Return true if the expression references a column that is being changed
/// (aiChng[i]>=0) or the rowid (when chngRowid).
pub fn sqlite3_expr_references_updated_column(p: &mut Parse, e: ExprId, ai_chng: &[i32], chng_rowid: bool) -> bool {
    let mut w = Walker { x_expr_callback: Some(check_constraint_expr_node), v_int: ai_chng.to_vec(), ..Walker::default() };
    sqlite3_walk_expr(p, &mut w, e);
    if !chng_rowid {
        w.e_code &= !0x02;
    }
    w.e_code != 0
}

/// `sqlite3GenerateConstraintChecks(pParse, pTab, aRegIdx, iDataCur, iIdxCur, regNewData,
/// regOldData, pkChng, overrideError, ignoreDest, pbMayReplace, aiChng, pUpsert)`: generate
/// code to do constraint checks prior to an INSERT or an UPDATE on table pTab.
///
/// The regNewData parameter is the first register in a range that contains the data to be
/// inserted or the data after the update. There will be pTab->nCol+1 registers in this
/// range. The first register (the one that regNewData points to) will contain the new
/// rowid value. The remaining pTab->nCol registers will contain the content of the new
/// row.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_generate_constraint_checks(
    p: &mut Parse,
    tab: TableId,
    a_reg_idx: &mut [i32],
    i_data_cur: i32,
    i_idx_cur: i32,
    reg_new_data: i32,
    reg_old_data: i32,
    pk_chng: bool,
    mut override_error: i32,
    ignore_dest: i32,
    pb_may_replace: &mut bool,
    ai_chng: Option<&[i32]>,
    upsert_in: UpsertId,
) {
    let mut upsert = upsert_in;
    let mut seen_replace = false; // True if REPLACE is used to resolve INT PK conflict
    let mut upsert_clause = UpsertId::NULL; // The specific ON CONFLICT clause for pIdx
    let mut b_affinity_done = false; // True if the OP_Affinity operation has been run
    let mut upsert_ipk_return = 0; // Address of Goto at end of IPK uniqueness check
    let mut upsert_ipk_delay = 0; // Address of Goto to bypass initial IPK check
    let mut ipk_top = 0; // Top of the IPK uniqueness check
    let mut ipk_bottom = 0; // OP_Goto at the end of the IPK uniqueness check
    let mut addr_recheck = 0; // Jump here to recheck all uniqueness constraints
    let mut lbl_recheck_ok = 0; // Each recheck jumps to this label if it passes
    let mut n_replace_trig = 0; // Number of replace triggers coded
    let mut on_error: i32;

    let is_update = reg_old_data != 0;
    let n_col = p.tab(tab).n_col() as i32;

    // If pPk is not NULL, it is the PRIMARY KEY index of a WITHOUT ROWID table. nPkField is
    // the number of fields in the true primary key of the table.
    let (pk, n_pk_field) = if p.tab(tab).has_rowid() {
        (IndexId::NULL, 1)
    } else {
        let pk = sqlite3_primary_key_index(p, tab);
        (pk, p.idx(pk).n_key_col as i32)
    };

    // Record that this module has started
    if p.tab(tab).tab_flags & TF_HasNotNull != 0 {
        let mut b2nd_pass = false; // True if currently running 2nd pass
        let mut n_seen_replace = 0; // Number of ON CONFLICT REPLACE operations
        let mut n_generated = 0; // Number of generated columns seen
        loop {
            for i in 0..n_col {
                let (not_null, is_generated, i_dflt) = {
                    let c = &p.tab(tab).a_col[i as usize];
                    (c.not_null as i32, c.col_flags & COLFLAG_GENERATED as u16 != 0, c.i_dflt)
                };
                on_error = not_null;
                if on_error == OE_None {
                    continue; // No NOT NULL on this column
                }
                if i == p.tab(tab).i_p_key as i32 {
                    continue; // ROWID is never NULL
                }
                if is_generated && !b2nd_pass {
                    n_generated += 1;
                    continue; // Generated columns processed on 2nd pass
                }
                if let Some(a) = ai_chng {
                    if a[i as usize] < 0 && !is_generated {
                        // Do not check NOT NULL on columns that do not change
                        continue;
                    }
                }
                if override_error != OE_Default {
                    on_error = override_error;
                } else if on_error == OE_Default {
                    on_error = OE_Abort;
                }
                if on_error == OE_Replace {
                    if b2nd_pass || i_dflt == 0 {
                        // REPLACE becomes ABORT if there is no default value or on the 2nd
                        // pass
                        on_error = OE_Abort;
                    }
                } else if b2nd_pass && !is_generated {
                    continue;
                }
                let i_reg = sqlite3_table_column_to_storage(p.tab(tab), i) + reg_new_data + 1;
                match on_error {
                    OE_Replace => {
                        let addr1 = p.v.add_op1(OP_NotNull, i_reg);
                        n_seen_replace += 1;
                        let x = crate::build_tab::sqlite3_column_expr(p, tab, i as usize);
                        sqlite3_expr_code_copy(p, x, i_reg);
                        p.v.jump_here(addr1);
                    }
                    OE_Abort | OE_Rollback | OE_Fail => {
                        if on_error == OE_Abort {
                            sqlite3_may_abort(p);
                        }
                        let msg = format!("{}.{}", p.tab(tab).z_name, p.tab(tab).a_col[i as usize].z_cn_name);
                        p.v.add_op3(OP_HaltIfNull, SQLITE_CONSTRAINT_NOTNULL, on_error, i_reg);
                        p.v.append_p4(P4::dynamic(&msg));
                        p.v.change_p5(P5_ConstraintNotNull as u16);
                    }
                    _ => {
                        p.v.add_op2(OP_IsNull, i_reg, ignore_dest);
                    }
                }
            }
            if n_generated == 0 && n_seen_replace == 0 {
                // If there are no generated columns with NOT NULL constraints and no NOT NULL
                // ON CONFLICT REPLACE constraints, then a single pass is sufficient
                break;
            }
            if b2nd_pass {
                break; // Never need more than 2 passes
            }
            b2nd_pass = true;
            if n_seen_replace > 0 && (p.tab(tab).tab_flags & TF_HasGenerated) != 0 {
                // If any NOT NULL ON CONFLICT REPLACE constraints fired on the first pass,
                // recomputed values for all generated columns, as those values might depend
                // on columns affected by the REPLACE.
                sqlite3_compute_generated_columns(p, reg_new_data + 1, tab);
            }
        }
    }

    // Test all CHECK constraints
    let check = p.tab(tab).p_check;
    if check.nn() && (p.db.flags & SQLITE_IgnoreChecks) == 0 {
        p.i_self_tab = -(reg_new_data + 1);
        on_error = if override_error != OE_Default { override_error } else { OE_Abort };
        for i in 0..p.l(check).a.len() {
            let e = p.l(check).a[i].p_expr;
            if let Some(a) = ai_chng {
                if !sqlite3_expr_references_updated_column(p, e, a, pk_chng) {
                    // The check constraints do not reference any of the columns being
                    // updated so there is no point it verifying the check constraint.
                    continue;
                }
            }
            if !b_affinity_done {
                sqlite3_table_affinity(p, tab, reg_new_data + 1);
                b_affinity_done = true;
            }
            let all_ok = p.v.make_label();
            let copy = sqlite3_expr_dup(p, e, 0);
            sqlite3_expr_if_true(p, copy, all_ok, SQLITE_JUMPIFNULL);
            if on_error == OE_Ignore {
                p.v.goto(ignore_dest);
            } else {
                let z_name = p.l(check).a[i].z_e_name.clone();
                if on_error == OE_Replace {
                    on_error = OE_Abort; // IMP: R-26383-51744
                }
                let p4 = match z_name {
                    Some(z) => P4::dynamic(&z),
                    None => P4::NotUsed,
                };
                sqlite3_halt_constraint(p, SQLITE_CONSTRAINT_CHECK, on_error, p4, P5_ConstraintCheck);
            }
            p.v.resolve_label(all_ok);
        }
        p.i_self_tab = 0;
    }

    // UNIQUE and PRIMARY KEY constraints should be handled in the following order:
    //
    //   (1)  OE_Update
    //   (2)  OE_Abort, OE_Fail, OE_Rollback, OE_Ignore
    //   (3)  OE_Replace
    //
    // OE_Fail and OE_Ignore must happen before any changes are made. UPSERT is the same as
    // the corresponding conflict resolution. OE_Replace must be after all others since it
    // deletes rows.
    let all_idx = p.tab(tab).p_index.clone();
    // sIdxIter: the order in which the indexes are visited, (index, ix)
    let mut idx_iter: Vec<(IndexId, usize)> = all_idx.iter().enumerate().map(|(i, &x)| (x, i)).collect();
    if upsert.nn() {
        if p.up(upsert).p_upsert_target.is_null() {
            // An ON CONFLICT DO NOTHING clause, without a constraint-target. Make all
            // unique constraint resolution be OE_Ignore
            if !p.up(upsert).is_do_update {
                override_error = OE_Ignore;
                upsert = UpsertId::NULL;
            } else {
                // A single ON CONFLICT DO UPDATE. Make all resolutions OE_Update.
                override_error = OE_Update;
            }
        } else if !all_idx.is_empty() {
            // Otherwise, we'll need to run the IndexListTerm array version of the iterator
            // to ensure that all of the ON CONFLICT conditions are checked first and in
            // order.
            let n_idx = all_idx.len();
            let mut b_used = vec![false; n_idx];
            let mut order: Vec<(IndexId, usize)> = Vec::new();
            let mut term = upsert;
            while term.nn() {
                if p.up(term).p_upsert_target.is_null() {
                    break;
                }
                let ui = p.up(term).p_upsert_idx;
                if ui.is_null() {
                    term = p.up(term).p_next_upsert;
                    continue;
                }
                let jj = all_idx.iter().position(|&x| x == ui).unwrap_or(0);
                if b_used[jj] {
                    term = p.up(term).p_next_upsert;
                    continue;
                }
                b_used[jj] = true;
                order.push((all_idx[jj], jj));
                term = p.up(term).p_next_upsert;
            }
            for (jj, &ix) in all_idx.iter().enumerate() {
                if b_used[jj] {
                    continue;
                }
                order.push((ix, jj));
            }
            idx_iter = order;
        }
    }

    // Determine if it is possible that triggers (either explicitly coded triggers or FK
    // resolution actions) might run as a result of deletes that happen when OE_Replace
    // conflict resolution is used.
    let mut trigger: Vec<TriggerId> = Vec::new();
    let mut reg_trig_cnt: i32;
    if (p.db.flags & (SQLITE_RecTriggers | SQLITE_ForeignKeys)) == 0 {
        // There are not DELETE triggers nor FK constraints. No constraint rechecks are
        // needed.
        reg_trig_cnt = 0;
    } else {
        if p.db.flags & SQLITE_RecTriggers != 0 {
            trigger = crate::trigger::sqlite3_triggers_exist(p, tab, TK_DELETE, ExprListId::NULL, None);
            reg_trig_cnt = (!trigger.is_empty() || crate::fkey::sqlite3_fk_required(p, tab, None, false) != 0) as i32;
        } else {
            reg_trig_cnt = crate::fkey::sqlite3_fk_required(p, tab, None, false);
        }
        if reg_trig_cnt != 0 {
            // Replace triggers might exist. Only consider them if some REPLACE conflict
            // resolution might be used.
            if override_error != OE_Replace {
                if override_error != OE_Default {
                    reg_trig_cnt = 0;
                } else if !pk_chng || pk.nn() || p.tab(tab).key_conf as i32 != OE_Replace {
                    if !all_idx.iter().any(|&ix| p.idx(ix).on_error as i32 == OE_Replace) {
                        reg_trig_cnt = 0;
                    }
                }
            }
        }
        if reg_trig_cnt != 0 {
            // Replace triggers might exist. Allocate the counter and initialize it to zero.
            p.n_mem += 1;
            reg_trig_cnt = p.n_mem;
            p.v.add_op2(OP_Integer, 0, reg_trig_cnt);
            lbl_recheck_ok = p.v.make_label();
            addr_recheck = lbl_recheck_ok;
        }
    }

    // If rowid is changing, make sure the new rowid does not previously exist in the table.
    if pk_chng && pk.is_null() {
        let addr_rowid_ok = p.v.make_label();

        // Figure out what action to take in case of a rowid collision
        on_error = p.tab(tab).key_conf as i32;
        if override_error != OE_Default {
            on_error = override_error;
        } else if on_error == OE_Default {
            on_error = OE_Abort;
        }

        // Check to see if there is an UPSERT clause that applies to the rowid.
        if upsert.nn() {
            upsert_clause = sqlite3_upsert_of_index(p, upsert, IndexId::NULL);
            if upsert_clause.nn() {
                on_error = if !p.up(upsert_clause).is_do_update { OE_Ignore } else { OE_Update };
            }
            if upsert_clause != upsert {
                // The first ON CONFLICT clause has a conflict target other than the IPK. We
                // have to jump ahead to that first ON CONFLICT clause and come back here.
                upsert_ipk_delay = p.v.add_op0(OP_Goto);
            }
        }

        // If the response to a rowid conflict is REPLACE but the response to some other
        // UNIQUE constraint is FAIL or IGNORE, then we need to defer the REPLACE until after
        // all of the other constraints have been checked.
        if on_error == OE_Replace && on_error != override_error && !all_idx.is_empty() {
            if upsert_ipk_delay != 0 {
                ipk_top = upsert_ipk_delay + 1;
                upsert_ipk_delay = 0;
            } else {
                ipk_top = p.v.add_op0(OP_Goto) + 1;
            }
        }

        if is_update {
            // pkChng!=0 does not mean that the rowid has changed, only that it might have
            // changed. Skip the conflict logic below if the rowid is unchanged.
            p.v.add_op3(OP_Eq, reg_new_data, addr_rowid_ok, reg_old_data);
            p.v.change_p5(SQLITE_NOTNULL as u16);
        }

        // Check to see if the new rowid already exists in the table. Skip the following
        // conflict logic if it does not.
        p.v.add_op3(OP_NotExists, i_data_cur, addr_rowid_ok, reg_new_data);

        match on_error {
            OE_Replace => {
                // If there are DELETE triggers on this table and the recursive-triggers flag
                // is set, then we need to invoke the DELETE triggers ...
                if reg_trig_cnt != 0 {
                    sqlite3_multi_write(p);
                    sqlite3_generate_row_delete(p, tab, &trigger, i_data_cur, i_idx_cur, reg_new_data, 1, false, OE_Replace as u8, 1, -1);
                    p.v.add_op2(OP_AddImm, reg_trig_cnt, 1); // incr trigger cnt
                    n_replace_trig += 1;
                } else {
                    if !all_idx.is_empty() {
                        sqlite3_multi_write(p);
                        sqlite3_generate_row_index_delete(p, tab, i_data_cur, i_idx_cur, None, -1);
                    }
                }
                seen_replace = true;
            }
            OE_Update | OE_Ignore => {
                if on_error == OE_Update {
                    sqlite3_upsert_do_update(p, upsert, tab, IndexId::NULL, i_data_cur);
                }
                p.v.goto(ignore_dest);
            }
            _ => {
                // OE_Rollback, OE_Abort, OE_Fail (default -> OE_Abort)
                let oe = if on_error == OE_Rollback || on_error == OE_Abort || on_error == OE_Fail { on_error } else { OE_Abort };
                sqlite3_rowid_constraint(p, oe, tab);
            }
        }
        p.v.resolve_label(addr_rowid_ok);
        if ipk_top != 0 {
            ipk_bottom = p.v.add_op0(OP_Goto);
            p.v.jump_here(ipk_top - 1);
        } else if upsert.nn() && upsert_clause != upsert {
            upsert_ipk_return = p.v.add_op0(OP_Goto);
        }
    }

    // Test all UNIQUE constraints by creating entries for each UNIQUE index and making sure
    // that duplicate entries do not already exist. Compute the revised record entries for
    // indices as we go.
    //
    // This loop also handles the case of the PRIMARY KEY index for a WITHOUT ROWID table.
    let mut last_ix = all_idx.len();
    for (n_iter, &(idx, ix)) in idx_iter.iter().enumerate() {
        let _ = n_iter;
        last_ix = ix;
        if a_reg_idx[ix] == 0 {
            continue; // Skip indices that do not change
        }
        if upsert.nn() {
            upsert_clause = sqlite3_upsert_of_index(p, upsert, idx);
            if upsert_ipk_delay != 0 && upsert_clause == upsert {
                p.v.jump_here(upsert_ipk_delay);
            }
        }
        let addr_unique_ok = p.v.make_label(); // Jump here if the UNIQUE constraint is satisfied
        if !b_affinity_done {
            sqlite3_table_affinity(p, tab, reg_new_data + 1);
            b_affinity_done = true;
        }
        let i_this_cur = i_idx_cur + ix as i32;

        // Skip partial indices for which the WHERE clause is not true
        let part = p.idx(idx).p_part_idx_where;
        if part.nn() {
            p.v.add_op2(OP_Null, 0, a_reg_idx[ix]);
            p.i_self_tab = -(reg_new_data + 1);
            sqlite3_expr_if_false_dup(p, part, addr_unique_ok, SQLITE_JUMPIFNULL);
            p.i_self_tab = 0;
        }

        // Create a record for this index entry as it should appear after the insert or
        // update. Store that record in the aRegIdx[ix] register
        let reg_idx = a_reg_idx[ix] + 1;
        let n_column = p.idx(idx).n_column as i32;
        for i in 0..n_column {
            let i_field = p.idx(idx).ai_column[i as usize] as i32;
            if i_field == XN_EXPR {
                p.i_self_tab = -(reg_new_data + 1);
                let x = p.l(p.idx(idx).a_col_expr).a[i as usize].p_expr;
                sqlite3_expr_code_copy(p, x, reg_idx + i);
                p.i_self_tab = 0;
            } else if i_field == XN_ROWID || i_field == p.tab(tab).i_p_key as i32 {
                let x = reg_new_data;
                p.v.add_op2(OP_IntCopy, x, reg_idx + i);
            } else {
                let x = sqlite3_table_column_to_storage(p.tab(tab), i_field) + reg_new_data + 1;
                p.v.add_op2(OP_SCopy, x, reg_idx + i);
            }
        }
        p.v.add_op3(OP_MakeRecord, reg_idx, n_column, a_reg_idx[ix]);
        // (sqlite3SetMakeRecordP5: SQLITE_ENABLE_NULL_TRIM is not part of the build)

        // In an UPDATE operation, if this index is the PRIMARY KEY index of a WITHOUT ROWID
        // table and if the PRIMARY KEY is not changing, then there is no need to check for a
        // conflict.
        if is_update && pk == idx && !pk_chng {
            p.v.resolve_label(addr_unique_ok);
            continue;
        }

        // Find out what action to take in case there is a uniqueness conflict
        on_error = p.idx(idx).on_error as i32;
        if on_error == OE_None {
            p.v.resolve_label(addr_unique_ok);
            continue; // pIdx is not a UNIQUE index
        }
        if override_error != OE_Default {
            on_error = override_error;
        } else if on_error == OE_Default {
            on_error = OE_Abort;
        }

        // Figure out if the upsert clause applies to this index
        if upsert_clause.nn() {
            on_error = if !p.up(upsert_clause).is_do_update { OE_Ignore } else { OE_Update };
        }

        // Collision detection may be omitted if all of the following are true: (1) The
        // conflict resolution algorithm is REPLACE (2) The table is a WITHOUT ROWID table
        // (3) There are no secondary indexes on the table (4) No delete triggers need to
        // be fired if there is a conflict (5) No FK constraint counters need to be updated
        // if a conflict occurs.
        let is_first_and_only = ix == 0 && all_idx.len() == 1;
        if is_first_and_only
            && pk == idx
            && on_error == OE_Replace
            && ((p.db.flags & SQLITE_RecTriggers) == 0 || crate::trigger::sqlite3_triggers_exist(p, tab, TK_DELETE, ExprListId::NULL, None).is_empty())
            && ((p.db.flags & SQLITE_ForeignKeys) == 0
                || (p.tab(tab).p_fkey.is_empty() && crate::fkey::sqlite3_fk_references(p, tab).is_empty()))
        {
            p.v.resolve_label(addr_unique_ok);
            continue;
        }

        // Check to see if the new index entry will be unique
        let n_key_col = p.idx(idx).n_key_col as i32;
        let addr_conflict_ck = p.v.add_op4_int(OP_NoConflict, i_this_cur, addr_unique_ok, reg_idx, n_key_col);

        // Generate code to handle collisions
        let reg_r = if idx == pk { reg_idx } else { sqlite3_get_temp_range(p, n_pk_field) };
        if is_update || on_error == OE_Replace {
            if p.tab(tab).has_rowid() {
                p.v.add_op2(OP_IdxRowid, i_this_cur, reg_r);
                // Conflict only if the rowid of the existing index entry is different from
                // old-rowid
                if is_update {
                    p.v.add_op3(OP_Eq, reg_r, addr_unique_ok, reg_old_data);
                    p.v.change_p5(SQLITE_NOTNULL as u16);
                }
            } else {
                // Extract the PRIMARY KEY from the end of the index entry and store it in
                // registers regR..regR+nPk-1
                if idx != pk {
                    for i in 0..p.idx(pk).n_key_col as usize {
                        let c = p.idx(pk).ai_column[i] as i32;
                        let x = sqlite3_table_column_to_index(p.idx(idx), c);
                        p.v.add_op3(OP_Column, i_this_cur, x, reg_r + i as i32);
                    }
                }
                if is_update {
                    // If currently processing the PRIMARY KEY of a WITHOUT ROWID table,
                    // then conflict only if the new PRIMARY KEY values are actually
                    // different from the old.
                    //
                    // For a UNIQUE index, only conflict if the PRIMARY KEY values of the
                    // index entry (which is either the row being updated or a different row)
                    // differ from the PRIMARY KEY of the row being updated.
                    let n_pk_key = p.idx(pk).n_key_col as i32;
                    let mut addr_jump = p.v.current_addr() + n_pk_key;
                    let mut op = OP_Ne;
                    let reg_cmp = if p.idx(idx).is_primary_key() { reg_idx } else { reg_r };
                    for i in 0..n_pk_key {
                        let z_coll = p.idx(pk).az_coll[i as usize].clone();
                        let coll = crate::callback::sqlite3_locate_coll_seq(p, &z_coll);
                        let x = p.idx(pk).ai_column[i as usize] as i32;
                        if i == n_pk_key - 1 {
                            addr_jump = addr_unique_ok;
                            op = OP_Eq;
                        }
                        let x = sqlite3_table_column_to_storage(p.tab(tab), x);
                        let p4 = match coll {
                            Some(c) => P4::CollSeq(c),
                            None => P4::NotUsed,
                        };
                        p.v.add_op4(op, reg_old_data + 1 + x, addr_jump, reg_cmp + i, p4);
                        p.v.change_p5(SQLITE_NOTNULL as u16);
                    }
                }
            }
        }

        // Generate code that executes if the new index entry is not unique
        let n_conflict_ck = p.v.current_addr() - addr_conflict_ck;
        match on_error {
            OE_Rollback | OE_Abort | OE_Fail => {
                sqlite3_unique_constraint(p, on_error, idx);
            }
            OE_Update | OE_Ignore => {
                if on_error == OE_Update {
                    sqlite3_upsert_do_update(p, upsert, tab, idx, i_idx_cur + ix as i32);
                }
                p.v.goto(ignore_dest);
            }
            _ => {
                // OE_Replace
                if reg_trig_cnt != 0 {
                    sqlite3_multi_write(p);
                    n_replace_trig += 1;
                }
                if !trigger.is_empty() && is_update {
                    p.v.add_op1(OP_CursorLock, i_data_cur);
                }
                let mode = if idx == pk { ONEPASS_SINGLE } else { ONEPASS_OFF };
                sqlite3_generate_row_delete(p, tab, &trigger, i_data_cur, i_idx_cur, reg_r, n_pk_field, false, OE_Replace as u8, mode as u8, i_this_cur);
                if !trigger.is_empty() && is_update {
                    p.v.add_op1(OP_CursorUnlock, i_data_cur);
                }
                seen_replace = true;
            }
        }
        if reg_trig_cnt != 0 {
            p.v.add_op2(OP_AddImm, reg_trig_cnt, 1); // incr trigger cnt
            let addr_bypass = p.v.add_op0(OP_Goto); // Bypass recheck
            p.v.resolve_label(lbl_recheck_ok);
            lbl_recheck_ok = p.v.make_label();
            if p.idx(idx).p_part_idx_where.nn() {
                // Bypass the recheck if this partial index is not defined for the current
                // row
                p.v.add_op2(OP_IsNull, reg_idx - 1, lbl_recheck_ok);
            }
            // Copy the constraint check code from above, except change the constraint-ok
            // jump destination to be the address of the next retest block
            let mut addr = addr_conflict_ck;
            let mut n = n_conflict_ck;
            while n > 0 {
                let x = p.v.get_op(addr).clone();
                if x.opcode != OP_IdxRowid || is_update {
                    let p2 = if OPCODE_PROPERTY[x.opcode as usize] & OPFLG_JUMP != 0 { lbl_recheck_ok } else { x.p2 };
                    p.v.add_op4(x.opcode, x.p1, p2, x.p3, x.p4.clone());
                    p.v.change_p5(x.p5);
                }
                n -= 1;
                addr += 1;
            }
            sqlite3_unique_constraint(p, OE_Abort, idx);
            p.v.jump_here(addr_bypass); // Terminate the recheck bypass
        }
        p.v.resolve_label(addr_unique_ok);
        if reg_r != reg_idx {
            sqlite3_release_temp_range(p, reg_r, n_pk_field);
        }
        if upsert_clause.nn() && upsert_ipk_return != 0 && sqlite3_upsert_next_is_ipk(p, upsert_clause) {
            p.v.goto(upsert_ipk_delay + 1);
            p.v.jump_here(upsert_ipk_return);
            upsert_ipk_return = 0;
        }
    }
    // The index-iterator variable ix after the loop: the number of indexes (C's iterator
    // ends with *pIx = nIdx / one past the last index of the linked list)
    let _ = last_ix;
    let ix = all_idx.len();

    // If the IPK constraint is a REPLACE, run it last
    if ipk_top != 0 {
        p.v.goto(ipk_top);
        p.v.jump_here(ipk_bottom);
    }

    // Recheck all uniqueness constraints after replace triggers have run
    if n_replace_trig != 0 {
        p.v.add_op2(OP_IfNot, reg_trig_cnt, lbl_recheck_ok);
        if pk.is_null() {
            if is_update {
                p.v.add_op3(OP_Eq, reg_new_data, addr_recheck, reg_old_data);
                p.v.change_p5(SQLITE_NOTNULL as u16);
            }
            p.v.add_op3(OP_NotExists, i_data_cur, addr_recheck, reg_new_data);
            sqlite3_rowid_constraint(p, OE_Abort, tab);
        } else {
            p.v.goto(addr_recheck);
        }
    }
    if reg_trig_cnt != 0 {
        p.v.resolve_label(lbl_recheck_ok);
    }

    // Generate the table record
    if p.tab(tab).has_rowid() {
        let reg_rec = a_reg_idx[ix];
        let nnv = p.tab(tab).n_nv_col as i32;
        p.v.add_op3(OP_MakeRecord, reg_new_data + 1, nnv, reg_rec);
        // (sqlite3SetMakeRecordP5: not part of the build)
        if !b_affinity_done {
            sqlite3_table_affinity(p, tab, 0);
        }
    }

    *pb_may_replace = seen_replace;
}

/// `sqlite3CompleteInsertion(pParse, pTab, iDataCur, iIdxCur, regNewData, aRegIdx,
/// update_flags, appendBias, useSeekResult)`: this routine generates code to finish the
/// INSERT or UPDATE operation that was started by a prior call to
/// sqlite3GenerateConstraintChecks.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_complete_insertion(
    p: &mut Parse,
    tab: TableId,
    i_data_cur: i32,
    i_idx_cur: i32,
    reg_new_data: i32,
    a_reg_idx: &[i32],
    update_flags: i32,
    append_bias: bool,
    use_seek_result: bool,
) {
    let idxs = p.tab(tab).p_index.clone();
    let has_rowid = p.tab(tab).has_rowid();
    let mut i = 0;
    for &ix in &idxs {
        if a_reg_idx[i] == 0 {
            i += 1;
            continue;
        }
        if p.idx(ix).p_part_idx_where.nn() || (update_flags != 0 && p.idx(ix).b_has_expr) {
            let here = p.v.current_addr();
            p.v.add_op2(OP_IsNull, a_reg_idx[i], here + 2);
        }
        let mut pik_flags: i32 = if use_seek_result { OPFLAG_USESEEKRESULT } else { 0 };
        if p.idx(ix).is_primary_key() && !has_rowid {
            pik_flags |= OPFLAG_NCHANGE;
            pik_flags |= update_flags & OPFLAG_SAVEPOSITION;
            // (codeWithoutRowidPreupdate: SQLITE_ENABLE_PREUPDATE_HOOK is not part of the
            // build)
        }
        let n = if p.idx(ix).uniq_not_null { p.idx(ix).n_key_col } else { p.idx(ix).n_column } as i32;
        p.v.add_op4_int(OP_IdxInsert, i_idx_cur + i as i32, a_reg_idx[i], a_reg_idx[i] + 1, n);
        p.v.change_p5(pik_flags as u16);
        i += 1;
    }
    if !has_rowid {
        return;
    }
    let mut pik_flags: i32;
    if p.nested != 0 {
        pik_flags = 0;
    } else {
        pik_flags = OPFLAG_NCHANGE;
        pik_flags |= if update_flags != 0 { update_flags } else { OPFLAG_LASTROWID };
    }
    if append_bias {
        pik_flags |= OPFLAG_APPEND;
    }
    if use_seek_result {
        pik_flags |= OPFLAG_USESEEKRESULT;
    }
    p.v.add_op3(OP_Insert, i_data_cur, a_reg_idx[i], reg_new_data);
    if p.nested == 0 {
        let p4 = p4_table_of(p, tab);
        p.v.append_p4(p4);
    }
    p.v.change_p5(pik_flags as u16);
}

/// `xferCompatibleIndex(pDest, pSrc)`: check to see if index pSrc is compatible as a source
/// of data for index pDest in an insert transfer optimization.
fn xfer_compatible_index(p: &Parse, dest: IndexId, src: IndexId) -> bool {
    let (d, s) = (p.idx(dest), p.idx(src));
    if d.n_key_col != s.n_key_col || d.n_column != s.n_column {
        return false; // Different number of columns
    }
    if d.on_error != s.on_error {
        return false; // Different conflict resolution strategies
    }
    for i in 0..s.n_key_col as usize {
        if s.ai_column[i] != d.ai_column[i] {
            return false; // Different columns indexed
        }
        if s.ai_column[i] as i32 == XN_EXPR {
            let (ea, eb) = (p.l(s.a_col_expr).a[i].p_expr, p.l(d.a_col_expr).a[i].p_expr);
            if sqlite3_expr_compare(p, false, ea, eb, -1) != 0 {
                return false; // Different expressions in the index
            }
        }
        if s.a_sort_order[i] != d.a_sort_order[i] {
            return false; // Different sort orders
        }
        if !s.az_coll[i].eq_ignore_ascii_case(&d.az_coll[i]) {
            return false; // Different collating sequences
        }
    }
    if sqlite3_expr_compare(p, false, s.p_part_idx_where, d.p_part_idx_where, -1) != 0 {
        return false; // Different WHERE clauses
    }
    // If no test above fails then the indices must be compatible
    true
}

/// `xferCheckRowid(pWalk, pExpr)`: walker callback of xferCompatibleCheck().
fn xfer_check_rowid(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).op == TK_COLUMN && p.e(e).i_column < 0 {
        w.e_code = 1;
        WRC_Abort
    } else {
        WRC_Continue
    }
}

/// `xferCompatibleCheck(pDest, pSrc)`: return true if the CHECK constraints of pDest are
/// compatible with those of pSrc for the transfer optimization.
fn xfer_compatible_check(p: &mut Parse, dest: TableId, src: TableId) -> bool {
    if sqlite3_expr_list_compare(p, p.tab(src).p_check, p.tab(dest).p_check, -1) != 0 {
        return false;
    }
    if p.tab(dest).i_p_key < 0 {
        let mut w = Walker { x_expr_callback: Some(xfer_check_rowid), ..Walker::default() };
        let chk = p.tab(dest).p_check;
        sqlite3_walk_expr_list(p, &mut w, chk);
        if w.e_code != 0 {
            return false;
        }
    }
    true
}

/// `xferOptimization(pParse, pDest, pSelect, onError, iDbDest)`: attempt the transfer
/// optimization on INSERTs of the form
///
/// ```text
///     INSERT INTO tab1 SELECT * FROM tab2;
/// ```
///
/// Return true if the optimization applied (the INSERT is complete), false if the normal
/// INSERT code must follow.
fn xfer_optimization(p: &mut Parse, dest: TableId, select: SelectId, mut on_error: i32, i_db_dest: i32) -> bool {
    let mut empty_dest_test = 0; // Address of test for empty pDest
    let mut empty_src_test = 0; // Address of test for empty pSrc
    let mut dest_has_unique_idx = false; // True if pDest has a UNIQUE index

    if p.p_with.nn() || p.s(select).p_with.nn() {
        // Do not attempt to process this query if there are an WITH clauses attached to it.
        // Proceeding may generate a false "no such table: xxx" error if pSelect reads from
        // a CTE named "xxx".
        return false;
    }
    if p.tab(dest).is_virtual() {
        return false; // tab1 must not be a virtual table
    }
    if on_error == OE_Default {
        if p.tab(dest).i_p_key >= 0 {
            on_error = p.tab(dest).key_conf as i32;
        }
        if on_error == OE_Default {
            on_error = OE_Abort;
        }
    }
    let s = p.s(select).clone();
    if p.src(s.p_src).n_src() != 1 {
        return false; // FROM clause must have exactly one term
    }
    if p.src(s.p_src).a[0].is_subquery {
        return false; // FROM clause cannot contain a subquery
    }
    if s.p_where.nn() {
        return false; // SELECT may not have a WHERE clause
    }
    if s.p_order_by.nn() {
        return false; // SELECT may not have an ORDER BY clause
    }
    // Do not need to test for a HAVING clause. If HAVING is present but there is no ORDER
    // BY, we will get an error.
    if s.p_group_by.nn() {
        return false; // SELECT may not have a GROUP BY clause
    }
    if s.p_limit.nn() {
        return false; // SELECT may not have a LIMIT clause
    }
    if s.p_prior.nn() {
        return false; // SELECT may not be a compound query
    }
    if s.sel_flags & SF_Distinct != 0 {
        return false; // SELECT may not be DISTINCT
    }
    let e_list = s.p_e_list;
    if p.l(e_list).n_expr() != 1 {
        return false; // The result set must have exactly one column
    }
    if p.e(p.l(e_list).a[0].p_expr).op != TK_ASTERISK {
        return false; // The result set must be the special operator "*"
    }

    // At this point we have established that the statement is of the correct syntactic form
    // to participate in this optimization. Now we have to check the semantics.
    let src = crate::schema::sqlite3_locate_table_item(p, 0, s.p_src, 0);
    if src.is_null() {
        return false; // FROM clause does not contain a real table
    }
    if p.tab(src).tnum == p.tab(dest).tnum && p.tab(src).i_db == p.tab(dest).i_db {
        return false; // tab1 and tab2 may not be the same table
    }
    if p.tab(dest).has_rowid() != p.tab(src).has_rowid() {
        return false; // source and destination must both be WITHOUT ROWID or not
    }
    if !p.tab(src).is_ordinary() {
        return false; // tab2 may not be a view or virtual table
    }
    if p.tab(dest).n_col() != p.tab(src).n_col() {
        return false; // Number of columns must be the same in tab1 and tab2
    }
    if p.tab(dest).i_p_key != p.tab(src).i_p_key {
        return false; // Both tables must have the same INTEGER PRIMARY KEY
    }
    if (p.tab(dest).tab_flags & TF_Strict) != 0 {
        if (p.tab(src).tab_flags & TF_Strict) == 0 {
            return false; // Cannot feed from a non-strict into a strict table
        }
        for i in 0..p.tab(dest).a_col.len() {
            let ed = p.tab(dest).a_col[i].e_c_type as i32;
            let es = p.tab(src).a_col[i].e_c_type as i32;
            if ed == COLTYPE_ANY {
                continue;
            }
            if ed == es {
                continue;
            }
            if ed == COLTYPE_INT && es == COLTYPE_INTEGER {
                continue;
            }
            if ed == COLTYPE_INTEGER && es == COLTYPE_INT {
                continue;
            }
            return false; // Affinity must be the same on all columns
        }
    }
    for i in 0..p.tab(dest).a_col.len() {
        let dc = p.tab(dest).a_col[i].clone();
        let sc = p.tab(src).a_col[i].clone();
        // (SQLITE_ENABLE_HIDDEN_COLUMNS is not part of the build)
        // Even if tables t1 and t2 have identical schemas, if they contain generated
        // columns, then this statement is semantically incorrect:
        //
        //     INSERT INTO t2 SELECT * FROM t1;
        //
        // The reason is that generated column values are returned by the the SELECT
        // statement on the right but the INSERT statement on the left ignores generated
        // column values. Disallow the transfer optimization if the destination table
        // contains any generated columns.
        if (dc.col_flags & COLFLAG_GENERATED as u16) != (sc.col_flags & COLFLAG_GENERATED as u16) {
            return false; // Both columns have the same generated-column type
        }
        // But the transfer is only allowed if both the source and destination tables have
        // the exact same expressions for generated columns. This requirement could be
        // relaxed for VIRTUAL columns, but STORED columns need to be an exact match.
        if dc.col_flags & COLFLAG_GENERATED as u16 != 0 {
            let es = crate::build_tab::sqlite3_column_expr(p, src, i);
            let ed = crate::build_tab::sqlite3_column_expr(p, dest, i);
            if sqlite3_expr_compare(p, false, es, ed, -1) != 0 {
                return false; // Different generator expressions
            }
        }
        if dc.affinity != sc.affinity {
            return false; // Affinity must be the same on all columns
        }
        let dcoll = crate::schema::sqlite3_column_coll(&dc).unwrap_or("BINARY");
        let scoll = crate::schema::sqlite3_column_coll(&sc).unwrap_or("BINARY");
        if !dcoll.eq_ignore_ascii_case(scoll) {
            return false; // Collating sequence must be the same on all columns
        }
        if dc.not_null != 0 && sc.not_null == 0 {
            return false; // tab2 must be NOT NULL if tab1 is
        }
        // Default values for second and subsequent columns need to match.
        if (dc.col_flags & COLFLAG_GENERATED as u16) == 0 && i > 0 {
            let ed = crate::build_tab::sqlite3_column_expr(p, dest, i);
            let es = crate::build_tab::sqlite3_column_expr(p, src, i);
            if ed.is_null() != es.is_null() || (ed.nn() && p.e(ed).z_token != p.e(es).z_token) {
                return false; // Default values must be the same for all columns
            }
        }
    }
    let didx = p.tab(dest).p_index.clone();
    let sidx = p.tab(src).p_index.clone();
    for &di in &didx {
        if p.idx(di).is_unique() {
            dest_has_unique_idx = true;
        }
        let Some(&si) = sidx.iter().find(|&&si| xfer_compatible_index(p, di, si)) else {
            return false; // pDestIdx has no corresponding index in pSrc
        };
        if p.idx(si).tnum == p.idx(di).tnum && p.tab(src).i_db == p.tab(dest).i_db {
            // The sqlite3FaultSim() call allows this corruption test to be bypassed during
            // testing, in order to exercise code paths further down.
            return false; // pDest and pSrc share the same storage
        }
    }
    if p.tab(dest).p_check.nn() && (p.db.m_db_flags & DBFLAG_Vacuum as u32) == 0 && !xfer_compatible_check(p, dest, src) {
        return false; // Tables have different CHECK constraints. Ticket #2252
    }
    // Disallow the transfer optimization if the destination table constrains any foreign
    // key constraints. This is so the required FK checks are performed.
    if (p.db.flags & SQLITE_ForeignKeys) != 0 && !p.tab(dest).p_fkey.is_empty() {
        return false;
    }
    if (p.db.flags & SQLITE_CountRows) != 0 {
        return false; // xfer opt does not play well with PRAGMA count_changes
    }
    // (db->xAuth: statements of a connection with an authorizer are not given to the
    // generator)

    // If we get this far, it means that the xfer optimization is at least a possibility,
    // though it might not work if the destination has a UNIQUE index or the destination is
    // non-empty.
    let i_db_src = p.tab(src).i_db;
    sqlite3_get_vdbe(p);
    sqlite3_code_verify_schema(p, i_db_src);
    let i_src = p.n_tab; // Cursors from source and destination
    p.n_tab += 1;
    let i_dest = p.n_tab;
    p.n_tab += 1;
    let reg_autoinc = auto_inc_begin(p, i_db_dest, dest);
    let reg_data = sqlite3_get_temp_reg(p);
    p.v.add_op2(OP_Null, 0, reg_data);
    let reg_rowid = sqlite3_get_temp_reg(p);
    sqlite3_open_table(p, i_dest, i_db_dest, dest, OP_OpenWrite);
    let vacuum = (p.db.m_db_flags & DBFLAG_Vacuum as u32) != 0;
    if !vacuum
        && ((p.tab(dest).i_p_key < 0 && !didx.is_empty())
            || dest_has_unique_idx
            || (on_error != OE_Abort && on_error != OE_Rollback))
    {
        // In some circumstances, we are able to run the xfer optimization only if the
        // destination table is initially empty. Code that will only execute if the
        // destination table is initially empty: (1) there is a primary key and the table
        // has indices (2) there are UNIQUE indices (3) onError is something other than
        // ABORT and ROLLBACK
        let addr1 = p.v.add_op2(OP_Rewind, i_dest, 0);
        empty_dest_test = p.v.add_op0(OP_Goto);
        p.v.jump_here(addr1);
    }
    if p.tab(src).has_rowid() {
        sqlite3_open_table(p, i_src, i_db_src, src, OP_OpenRead);
        empty_src_test = p.v.add_op2(OP_Rewind, i_src, 0);
        let addr1;
        if p.tab(dest).i_p_key >= 0 {
            addr1 = p.v.add_op2(OP_Rowid, i_src, reg_rowid);
            if !vacuum {
                let addr2 = p.v.add_op3(OP_NotExists, i_dest, 0, reg_rowid);
                sqlite3_rowid_constraint(p, on_error, dest);
                p.v.jump_here(addr2);
            }
            auto_inc_step(p, reg_autoinc, reg_rowid);
        } else if didx.is_empty() && (p.db.m_db_flags & DBFLAG_VacuumInto as u32) == 0 {
            addr1 = p.v.add_op2(OP_NewRowid, i_dest, reg_rowid);
        } else {
            addr1 = p.v.add_op2(OP_Rowid, i_src, reg_rowid);
        }
        let ins_flags = if vacuum {
            p.v.add_op1(OP_SeekEnd, i_dest);
            OPFLAG_APPEND | OPFLAG_USESEEKRESULT | OPFLAG_PREFORMAT
        } else {
            OPFLAG_NCHANGE | OPFLAG_LASTROWID | OPFLAG_APPEND | OPFLAG_PREFORMAT
        };
        // (SQLITE_ENABLE_PREUPDATE_HOOK is not part of the build)
        p.v.add_op3(OP_RowCell, i_dest, i_src, reg_rowid);
        p.v.add_op3(OP_Insert, i_dest, reg_data, reg_rowid);
        if !vacuum {
            let p4 = p4_table_of(p, dest);
            p.v.change_p4(-1, p4);
        }
        p.v.change_p5(ins_flags as u16);
        p.v.add_op2(OP_Next, i_src, addr1);
        p.v.add_op2(OP_Close, i_src, 0);
        p.v.add_op2(OP_Close, i_dest, 0);
    } else {
        let (dt, sn) = (p.tab(dest).tnum, p.tab(dest).z_name.clone());
        sqlite3_table_lock(p, i_db_dest, dt, 1, &sn);
        let (st, ss) = (p.tab(src).tnum, p.tab(src).z_name.clone());
        sqlite3_table_lock(p, i_db_src, st, 0, &ss);
    }
    for &di in &didx {
        let mut idx_ins_flags: i32 = 0;
        let si = *sidx.iter().find(|&&si| xfer_compatible_index(p, di, si)).expect("compatible index");
        let stnum = p.idx(si).tnum as i32;
        p.v.add_op3(OP_OpenRead, i_src, stnum, i_db_src);
        sqlite3_vdbe_set_p4_key_info(p, si);
        let dtnum = p.idx(di).tnum as i32;
        p.v.add_op3(OP_OpenWrite, i_dest, dtnum, i_db_dest);
        sqlite3_vdbe_set_p4_key_info(p, di);
        p.v.change_p5(OPFLAG_BULKCSR as u16);
        let addr1 = p.v.add_op2(OP_Rewind, i_src, 0);
        if vacuum {
            // This INSERT command is part of a VACUUM operation, which guarantees that the
            // destination index is initially empty and that the collating sequences of all
            // columns are BINARY.
            let all_binary = p.idx(si).az_coll.iter().take(p.idx(si).n_column as usize).all(|c| c.eq_ignore_ascii_case("BINARY"));
            if all_binary {
                idx_ins_flags = OPFLAG_USESEEKRESULT | OPFLAG_PREFORMAT;
                p.v.add_op1(OP_SeekEnd, i_dest);
                p.v.add_op2(OP_RowCell, i_dest, i_src);
            }
        } else if !p.tab(src).has_rowid() && p.idx(di).idx_type == SQLITE_IDXTYPE_PRIMARYKEY as u8 {
            idx_ins_flags |= OPFLAG_NCHANGE;
        }
        if idx_ins_flags != (OPFLAG_USESEEKRESULT | OPFLAG_PREFORMAT) {
            p.v.add_op3(OP_RowData, i_src, reg_data, 1);
            // (codeWithoutRowidPreupdate: not part of the build)
        }
        p.v.add_op2(OP_IdxInsert, i_dest, reg_data);
        p.v.change_p5((idx_ins_flags | OPFLAG_APPEND) as u16);
        p.v.add_op2(OP_Next, i_src, addr1 + 1);
        p.v.jump_here(addr1);
        p.v.add_op2(OP_Close, i_src, 0);
        p.v.add_op2(OP_Close, i_dest, 0);
    }
    if empty_src_test != 0 {
        p.v.jump_here(empty_src_test);
    }
    sqlite3_release_temp_reg(p, reg_rowid);
    sqlite3_release_temp_reg(p, reg_data);
    if empty_dest_test != 0 {
        sqlite3_autoincrement_end(p);
        p.v.add_op2(OP_Halt, SQLITE_OK, 0);
        p.v.jump_here(empty_dest_test);
        p.v.add_op2(OP_Close, i_dest, 0);
        false
    } else {
        true
    }
}
