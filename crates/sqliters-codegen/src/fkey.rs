//! Port of `fkey.c`: foreign key constraints of DML statements (`sqlite3FkLocateIndex`,
//! `fkLookupParent`, `fkScanChildren`, `sqlite3FkCheck`, `sqlite3FkOldmask`,
//! `sqlite3FkRequired`, `fkActionTrigger`, `sqlite3FkActions`). A foreign key is named by
//! its child table (`FKey.pFrom`) and its index in that table's `p_fkey` list.

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::P4;

use crate::build::*;
use crate::consts::*;
use crate::expr::*;
use crate::schema::*;
use crate::sqliteint::*;
use crate::tk::*;

/// A foreign key: (child table `pFrom`, index into its `p_fkey`).
pub type FkRef = (TableId, usize);

fn fk<'a>(p: &'a Parse<'_>, r: FkRef) -> &'a FKey {
    &p.tab(r.0).p_fkey[r.1]
}

/// `sqlite3FkReferences(pTab)`: the foreign keys of the schema of `tab` whose parent table
/// is `tab` (C's `fkeyHash` entry of the name: a list linked by `pNextTo`, the most recently
/// created foreign key first).
pub fn sqlite3_fk_references(p: &Parse, tab: TableId) -> Vec<FkRef> {
    let i_db = p.tab(tab).i_db;
    if i_db < 0 || i_db as usize >= p.a_schema.len() {
        return Vec::new();
    }
    let name = &p.tab(tab).z_name;
    let mut tabs: Vec<TableId> = p.a_schema[i_db as usize].tbl_hash.values().copied().collect();
    // Tables were created in schema order: the newest foreign key is in the newest table.
    tabs.sort_by(|a, b| b.cmp(a));
    let mut out = Vec::new();
    for t in tabs {
        if !p.tab(t).is_ordinary() {
            continue;
        }
        for (k, f) in p.tab(t).p_fkey.iter().enumerate() {
            if f.z_to.eq_ignore_ascii_case(name) {
                out.push((t, k));
            }
        }
    }
    out
}

/// `sqlite3FkLocateIndex(pParse, pParent, pFKey, ppIdx, paiCol)`: a foreign key constraint
/// requires that the parent key columns are either the PRIMARY KEY (rowid or WITHOUT ROWID
/// primary key) or have a UNIQUE index on them. Find that index. Returns `Err(())` (after an
/// error message unless triggers are disabled) when there is none; `Ok((idx, aiCol))`
/// otherwise: `idx` NULL means the INTEGER PRIMARY KEY; `aiCol[i]` is the child column
/// mapped to index column i (only computed for multi-column keys and the PRIMARY KEY).
pub fn sqlite3_fk_locate_index(p: &mut Parse, parent: TableId, f: FkRef, want_aicol: bool) -> Result<(IndexId, Option<Vec<i32>>), ()> {
    let fkey = fk(p, f).clone();
    let n_col = fkey.a_col.len();
    let z_key = fkey.a_col[0].1.clone();
    let mut ai_col: Option<Vec<i32>> = None;

    // The caller is responsible for zeroing output parameters.
    if n_col == 1 {
        // The FK maps to the IPK if any of the following are true:
        //
        //   1) There is an INTEGER PRIMARY KEY column and the FK is implicitly mapped to the
        //      primary key of table pParent, or
        //   2) The FK is explicitly mapped to a column declared as INTEGER PRIMARY KEY.
        let t = p.tab(parent);
        if t.i_p_key >= 0 {
            match &z_key {
                None => return Ok((IndexId::NULL, None)),
                Some(z) => {
                    if t.a_col[t.i_p_key as usize].z_cn_name.eq_ignore_ascii_case(z) {
                        return Ok((IndexId::NULL, None));
                    }
                }
            }
        }
    } else if want_aicol {
        ai_col = Some(vec![0; n_col]);
    }

    let mut found = IndexId::NULL;
    for ix in p.tab(parent).p_index.clone() {
        let idx = p.idx(ix);
        if idx.n_key_col as usize == n_col && idx.is_unique() && idx.p_part_idx_where.is_null() {
            // pIdx is a UNIQUE index (or a PRIMARY KEY) and has the right number of columns.
            // If zKey is NULL, then this foreign key is implicitly mapped to the PRIMARY KEY
            // of table pParent. Otherwise, it is mapped to the columns in zKey.
            if z_key.is_none() {
                if idx.is_primary_key() {
                    if let Some(a) = ai_col.as_mut() {
                        for i in 0..n_col {
                            a[i] = fkey.a_col[i].0;
                        }
                    }
                    found = ix;
                    break;
                }
            } else {
                // If zKey is non-NULL, then this foreign key was declared to map to an
                // explicit list of columns in table pParent. Check if this index matches
                // those columns. Also, check that the index uses the default collation
                // sequences for each column.
                let mut i = 0;
                while i < n_col {
                    let i_col = p.idx(ix).ai_column[i];
                    if i_col < 0 {
                        break; // No foreign keys against expression indexes
                    }
                    // If the index uses a collation sequence that is different from the
                    // default collation sequence for the column, this index is unusable.
                    // Bail out early in this case.
                    let pc = &p.tab(parent).a_col[i_col as usize];
                    let z_dflt_coll = sqlite3_column_coll(pc).unwrap_or("BINARY").to_owned();
                    if !p.idx(ix).az_coll[i].eq_ignore_ascii_case(&z_dflt_coll) {
                        break;
                    }
                    let z_idx_col = pc.z_cn_name.clone();
                    let mut j = 0;
                    while j < n_col {
                        if fkey.a_col[j].1.as_deref().is_some_and(|z| z.eq_ignore_ascii_case(&z_idx_col)) {
                            if let Some(a) = ai_col.as_mut() {
                                a[i] = fkey.a_col[j].0;
                            }
                            break;
                        }
                        j += 1;
                    }
                    if j == n_col {
                        break;
                    }
                    i += 1;
                }
                if i == n_col {
                    found = ix;
                    break; // pIdx is usable
                }
            }
        }
    }

    if found.is_null() {
        if !p.disable_triggers {
            let msg = format!("foreign key mismatch - \"{}\" referencing \"{}\"", p.tab(f.0).z_name, fkey.z_to);
            p.error_msg(msg);
        }
        return Err(());
    }
    Ok((found, ai_col))
}

/// `fkLookupParent(pParse, iDb, pTab, pIdx, pFKey, aiCol, regData, nIncr, isIgnore)`: this
/// function is called when a row is inserted into or deleted from the child table of
/// foreign key constraint pFKey. If an SQL UPDATE is executed on the child table of pFKey,
/// this function is called twice for each row updated.
#[allow(clippy::too_many_arguments)]
fn fk_lookup_parent(p: &mut Parse, i_db: i32, tab: TableId, idx: IndexId, f: FkRef, ai_col: &[i32], reg_data: i32, n_incr: i32, is_ignore: bool) {
    let fkey = fk(p, f).clone();
    let from = f.0;
    crate::select::sqlite3_get_vdbe(p);
    let i_cur = p.n_tab - 1; // Cursor number to use
    let i_ok = p.v.make_label(); // jump here if parent key found

    // If nIncr is less than zero, then check at runtime if there are any outstanding
    // constraints to resolve. If there are not, there is no need to check if deleting this
    // row resolves any outstanding violations.
    //
    // Check if any of the key columns in the child table row are NULL. If any are, then the
    // constraint is considered satisfied. No need to search for a matching row in the
    // parent table.
    if n_incr < 0 {
        p.v.add_op2(OP_FkIfZero, fkey.is_deferred as i32, i_ok);
    }
    for i in 0..fkey.a_col.len() {
        let i_reg = sqlite3_table_column_to_storage(p.tab(from), ai_col[i]) + reg_data + 1;
        p.v.add_op2(OP_IsNull, i_reg, i_ok);
    }

    if !is_ignore {
        if idx.is_null() {
            // If pIdx is NULL, then the parent key is the INTEGER PRIMARY KEY column of the
            // parent table (table pTab).
            let reg_temp = sqlite3_get_temp_reg(p);

            // Invoke MustBeInt to coerce the child key value to an integer (i.e. apply the
            // affinity of the parent key). If this fails, then there is no matching parent
            // key. Before using MustBeInt, make a copy of the value. Otherwise, the value
            // inserted into the child key field of the table might be coerced too.
            let src = sqlite3_table_column_to_storage(p.tab(from), ai_col[0]) + 1 + reg_data;
            p.v.add_op2(OP_SCopy, src, reg_temp);
            let i_must_be_int = p.v.add_op2(OP_MustBeInt, reg_temp, 0);

            // If the parent table is the same as the child table, and we are about to
            // increment the constraint-counter (i.e. this is an INSERT operation), then check
            // if the row being inserted matches itself. If so, do not increment the
            // constraint-counter.
            if tab == from && n_incr == 1 {
                p.v.add_op3(OP_Eq, reg_data, i_ok, reg_temp);
                p.v.change_p5(SQLITE_NOTNULL as u16);
            }

            sqlite3_open_table(p, i_cur, i_db, tab, OP_OpenRead);
            p.v.add_op3(OP_NotExists, i_cur, 0, reg_temp);
            p.v.goto(i_ok);
            let a = p.v.current_addr() - 2;
            p.v.jump_here(a);
            p.v.jump_here(i_must_be_int);
            sqlite3_release_temp_reg(p, reg_temp);
        } else {
            let n_col = fkey.a_col.len() as i32;
            let reg_temp = sqlite3_get_temp_range(p, n_col);

            let tnum = p.idx(idx).tnum as i32;
            p.v.add_op3(OP_OpenRead, i_cur, tnum, i_db);
            sqlite3_vdbe_set_p4_key_info(p, idx);
            for i in 0..n_col {
                let src = sqlite3_table_column_to_storage(p.tab(from), ai_col[i as usize]) + 1 + reg_data;
                p.v.add_op2(OP_Copy, src, reg_temp + i);
            }

            // If the parent table is the same as the child table, and we are about to
            // increment the constraint-counter (i.e. this is an INSERT operation), then check
            // if the row being inserted matches itself. If so, do not increment the
            // constraint-counter.
            //
            // If any of the parent-key values are NULL, then the row cannot match itself. So
            // set JUMPIFNULL to make sure we do the OP_Found if any of the parent-key values
            // are NULL (at this point it is known that none of the child key values are).
            if tab == from && n_incr == 1 {
                let i_jump = p.v.current_addr() + n_col + 1;
                for i in 0..n_col {
                    let i_child = sqlite3_table_column_to_storage(p.tab(from), ai_col[i as usize]) + 1 + reg_data;
                    let ic = p.idx(idx).ai_column[i as usize];
                    let itab = p.idx(idx).p_table;
                    let mut i_parent = 1 + reg_data + sqlite3_table_column_to_storage(p.tab(itab), ic as i32);
                    if ic == p.tab(tab).i_p_key {
                        // The parent key is a composite key that includes the IPK column
                        i_parent = reg_data;
                    }
                    p.v.add_op3(OP_Ne, i_child, i_jump, i_parent);
                    p.v.change_p5(SQLITE_JUMPIFNULL as u16);
                }
                p.v.goto(i_ok);
            }

            let aff = sqlite3_index_affinity_str(p, idx);
            p.v.add_op4(OP_Affinity, reg_temp, n_col, 0, P4::Dynamic(aff[..n_col as usize].to_vec()));
            p.v.add_op4_int(OP_Found, i_cur, i_ok, reg_temp, n_col);
            sqlite3_release_temp_range(p, reg_temp, n_col);
        }
    }

    if !fkey.is_deferred && (p.db.flags & SQLITE_DeferFKs) == 0 && p.is_toplevel() && !p.is_multi_write {
        // Special case: If this is an INSERT statement that will insert exactly one row
        // into the table, raise a constraint immediately instead of incrementing a
        // counter. This is necessary as the VM code is being generated for will not
        // execute any further (the statement aborts).
        sqlite3_halt_constraint(p, SQLITE_CONSTRAINT_FOREIGNKEY, OE_Abort, P4::NotUsed, P5_ConstraintFK);
    } else {
        if n_incr > 0 && !fkey.is_deferred {
            sqlite3_may_abort(p);
        }
        p.v.add_op2(OP_FkCounter, fkey.is_deferred as i32, n_incr);
    }

    p.v.resolve_label(i_ok);
    p.v.add_op1(OP_Close, i_cur);
}

/// `exprTableRegister(pParse, pTab, regBase, iCol)`: return an Expr object that refers to a
/// memory register corresponding to column iCol of table pTab (regBase holds the rowid).
fn expr_table_register(p: &mut Parse, tab: TableId, reg_base: i32, i_col: i16) -> ExprId {
    let mut e = sqlite3_expr(p, TK_REGISTER, None);
    if e.nn() {
        if i_col >= 0 && i_col != p.tab(tab).i_p_key {
            let st = sqlite3_table_column_to_storage(p.tab(tab), i_col as i32);
            let (aff, coll) = {
                let c = &p.tab(tab).a_col[i_col as usize];
                (c.affinity, sqlite3_column_coll(c).unwrap_or("BINARY").to_owned())
            };
            let x = p.em(e);
            x.i_table = reg_base + st + 1;
            x.aff_expr = aff;
            e = sqlite3_expr_add_collate_string(p, e, &coll);
        } else {
            let x = p.em(e);
            x.i_table = reg_base;
            x.aff_expr = SQLITE_AFF_INTEGER;
        }
    }
    e
}

/// `exprTableColumn(db, pTab, iCursor, iCol)`: return an Expr object that refers to column
/// iCol of table pTab with cursor iCursor.
fn expr_table_column(p: &mut Parse, tab: TableId, i_cursor: i32, i_col: i16) -> ExprId {
    let e = sqlite3_expr(p, TK_COLUMN, None);
    if e.nn() {
        let x = p.em(e);
        x.y_tab = tab;
        x.i_table = i_cursor;
        x.i_column = i_col;
    }
    e
}

/// `fkScanChildren(pParse, pSrc, pTab, pIdx, pFKey, aiCol, regData, nIncr)`: this function is
/// called to generate code executed when a row is deleted from the parent table of foreign
/// key constraint pFKey and, if pFKey is deferred, when a row is inserted into the same
/// parent table. It scans the child table for rows that refer to the parent row and
/// adjusts the constraint counter by nIncr for each.
#[allow(clippy::too_many_arguments)]
fn fk_scan_children(p: &mut Parse, src: SrcListId, tab: TableId, idx: IndexId, f: FkRef, ai_col: Option<&[i32]>, reg_data: i32, n_incr: i32) {
    let fkey = fk(p, f).clone();
    let from = f.0;
    let mut where_ = ExprId::NULL;
    let mut i_fk_if_zero = 0;
    crate::select::sqlite3_get_vdbe(p);

    if n_incr < 0 {
        i_fk_if_zero = p.v.add_op2(OP_FkIfZero, fkey.is_deferred as i32, 0);
    }

    // Create an Expr object representing an SQL expression like:
    //
    //   <parent-key1> = <child-key1> AND <parent-key2> = <child-key2> ...
    //
    // The collation sequence used for the comparison should be that of the parent key
    // columns. The affinity of the parent key column should be applied to each child key
    // value before the comparison takes place.
    for i in 0..fkey.a_col.len() {
        let i_col = if idx.nn() { p.idx(idx).ai_column[i] } else { -1 };
        let left = expr_table_register(p, tab, reg_data, i_col);
        let i_col = match ai_col {
            Some(a) => a[i],
            None => fkey.a_col[0].0,
        };
        let z_col = p.tab(from).a_col[i_col as usize].z_cn_name.clone();
        let right = sqlite3_expr(p, TK_ID, Some(&z_col));
        let eq = sqlite3_p_expr(p, TK_EQ, left, right);
        where_ = sqlite3_expr_and(p, where_, eq);
    }

    // If the child table is the same as the parent table, then add terms to the WHERE
    // clause that prevent this entry from being scanned. The added WHERE clause terms are
    // like this:
    //
    //     $current_rowid!=rowid
    //     NOT( $current_a==a AND $current_b==b AND ... )
    //
    // The first form is used for rowid tables. The second form is used for WITHOUT ROWID
    // tables. In the second form, the *parent* key is (a,b,...) and the *current* values
    // are the values of the row being deleted or updated.
    if tab == from && n_incr > 0 {
        let ne;
        if p.tab(tab).has_rowid() {
            let left = expr_table_register(p, tab, reg_data, -1);
            let cur = p.src(src).a[0].i_cursor;
            let right = expr_table_column(p, tab, cur, -1);
            ne = sqlite3_p_expr(p, TK_NE, left, right);
        } else {
            let mut all = ExprId::NULL;
            for i in 0..p.idx(idx).n_key_col as usize {
                let i_col = p.idx(idx).ai_column[i];
                let left = expr_table_register(p, tab, reg_data, i_col);
                let name = p.tab(tab).a_col[i_col as usize].z_cn_name.clone();
                let right = sqlite3_expr(p, TK_ID, Some(&name));
                let eq = sqlite3_p_expr(p, TK_IS, left, right);
                all = sqlite3_expr_and(p, all, eq);
            }
            ne = sqlite3_p_expr(p, TK_NOT, all, ExprId::NULL);
        }
        where_ = sqlite3_expr_and(p, where_, ne);
    }

    // Resolve the references in the WHERE clause.
    let nc = p.new_nc(NameContext { p_src_list: src, ..NameContext::default() });
    crate::resolve::sqlite3_resolve_expr_names(p, nc, where_);

    // Create VDBE to loop through the entries in pSrc that match the WHERE clause. For each
    // row found, increment either the deferred or immediate foreign key constraint counter.
    if p.n_err == 0 {
        let w = crate::where_::sqlite3_where_begin(p, src, where_, ExprListId::NULL, ExprListId::NULL, SelectId::NULL, 0, 0);
        p.v.add_op2(OP_FkCounter, fkey.is_deferred as i32, n_incr);
        if let Some(w) = w {
            crate::where_::sqlite3_where_end(p, w);
        }
    }

    // Clean up the WHERE clause constructed above.
    if i_fk_if_zero != 0 {
        p.v.jump_here_or_pop_inst(i_fk_if_zero);
    }
}

/// `fkChildIsModified(pTab, p, aChange, bChngRowid)`: return true if any column of the child
/// key of foreign key `f` is modified by the UPDATE (`a_change[i]>=0`).
fn fk_child_is_modified(p: &Parse, tab: TableId, f: &FKey, a_change: &[i32], b_chng_rowid: bool) -> bool {
    for &(i_child_key, _) in &f.a_col {
        if a_change[i_child_key as usize] >= 0 {
            return true;
        }
        if i_child_key == p.tab(tab).i_p_key as i32 && b_chng_rowid {
            return true;
        }
    }
    false
}

/// `fkParentIsModified(pTab, p, aChange, bChngRowid)`: return true if any column of the
/// parent key of foreign key `f` (a key of `tab`) is modified by the UPDATE.
fn fk_parent_is_modified(p: &Parse, tab: TableId, f: &FKey, a_change: &[i32], b_chng_rowid: bool) -> bool {
    let t = p.tab(tab);
    for (_, z_key) in &f.a_col {
        for i_key in 0..t.a_col.len() {
            if a_change[i_key] >= 0 || (i_key as i32 == t.i_p_key as i32 && b_chng_rowid) {
                let col = &t.a_col[i_key];
                if let Some(z) = z_key {
                    if col.z_cn_name.eq_ignore_ascii_case(z) {
                        return true;
                    }
                } else if col.col_flags & COLFLAG_PRIMKEY as u16 != 0 {
                    return true;
                }
            }
        }
    }
    false
}

/// `isSetNullAction(pParse, pFKey)`: return true if the parser passed as the first argument
/// is being used to code a trigger that is really a "SET NULL" action belonging to trigger
/// pFKey.
fn is_set_null_action(p: &Parse, f: FkRef) -> bool {
    if let Some(top) = p.p_trigger_prg.last() {
        let t = top.p_trigger;
        let fkey = fk(p, f);
        let at = p.fk_action_triggers.iter().find(|x| x.0 == f.0 && x.1 == f.1);
        if let Some(&(_, _, a0, a1)) = at {
            if (a0 == Some(t) && fkey.a_action[0] as i32 == OE_SetNull) || (a1 == Some(t) && fkey.a_action[1] as i32 == OE_SetNull) {
                return true;
            }
        }
    }
    false
}

/// `sqlite3FkCheck(pParse, pTab, regOld, regNew, aChange, bChngRowid)`: this function is
/// called when inserting, deleting or updating a row of table pTab to generate VDBE code to
/// perform foreign key constraint processing for the operation.
pub fn sqlite3_fk_check(p: &mut Parse, tab: TableId, reg_old: i32, reg_new: i32, a_change: Option<&[i32]>, b_chng_rowid: bool) {
    let is_ignore_errors = p.disable_triggers;

    // Exactly one of regOld and regNew should be non-zero.

    // If foreign-keys are disabled, this function is a no-op.
    if (p.db.flags & SQLITE_ForeignKeys) == 0 {
        return;
    }
    if !p.tab(tab).is_ordinary() {
        return;
    }

    let i_db = p.tab(tab).i_db;
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();

    // Loop through all the foreign key constraints for which pTab is the child table.
    for k in 0..p.tab(tab).p_fkey.len() {
        let f: FkRef = (tab, k);
        let fkey = fk(p, f).clone();
        let mut b_ignore = false;

        if let Some(ac) = a_change {
            if !p.tab(tab).z_name.eq_ignore_ascii_case(&fkey.z_to) && !fk_child_is_modified(p, tab, &fkey, ac, b_chng_rowid) {
                continue;
            }
        }

        // Find the parent table of this foreign key. Also find a unique index on the
        // parent key columns in the parent table. If either of these schema items cannot
        // be located, set an error in pParse and return early.
        let to = if p.disable_triggers {
            crate::schema::sqlite3_find_table(p, &fkey.z_to, Some(&z_db))
        } else {
            crate::schema::sqlite3_locate_table(p, 0, &fkey.z_to, Some(&z_db))
        };
        let located = if to.is_null() { Err(()) } else { sqlite3_fk_locate_index(p, to, f, true) };
        let Ok((idx, ai_free)) = located else {
            if !is_ignore_errors {
                return;
            }
            if to.is_null() {
                // If isIgnoreErrors is true, then a table is being dropped. In this case
                // SQLite runs a "DELETE FROM xxx" on the table being dropped before actually
                // dropping it in order to check FK constraints. If the parent table of an FK
                // constraint on the current table is missing, behave as if it is empty. i.e.
                // decrement the relevant FK counter for each row of the current table with
                // non-NULL keys.
                let i_jump = p.v.current_addr() + fkey.a_col.len() as i32 + 1;
                for i in 0..fkey.a_col.len() {
                    let i_from_col = fkey.a_col[i].0;
                    let i_reg = sqlite3_table_column_to_storage(p.tab(tab), i_from_col) + reg_old + 1;
                    p.v.add_op2(OP_IsNull, i_reg, i_jump);
                }
                p.v.add_op2(OP_FkCounter, fkey.is_deferred as i32, -1);
            }
            continue;
        };
        let mut ai_col: Vec<i32> = match ai_free {
            Some(a) => a,
            None => vec![fkey.a_col[0].0],
        };
        for c in ai_col.iter_mut().take(fkey.a_col.len()) {
            if *c == p.tab(tab).i_p_key as i32 {
                *c = -1;
            }
            // (sqlite3AuthReadCol: no authorizer)
        }
        let _ = &mut b_ignore;

        // Take a shared-cache advisory read-lock on the parent table. Allocate a cursor
        // to use for scanning the parent table.
        let (tt, tn) = (p.tab(to).tnum, p.tab(to).z_name.clone());
        sqlite3_table_lock(p, i_db, tt, 0, &tn);
        p.n_tab += 1;

        if reg_old != 0 {
            // A row is being removed from the child table. Search for the parent. If the
            // parent does not exist, removing the child row resolves an outstanding foreign
            // key constraint violation. So decrement the constraint counter.
            fk_lookup_parent(p, i_db, to, idx, f, &ai_col, reg_old, -1, b_ignore);
        }
        if reg_new != 0 && !is_set_null_action(p, f) {
            // A row is being added to the child table. If a parent row cannot be found,
            // adding the child row has violated the FK constraint.
            //
            // If this operation is being performed as part of a trigger program that is
            // actually a "SET NULL" action belonging to this very foreign key, then omit
            // this scan altogether. As all child key values are guaranteed to be NULL, it
            // is not possible for adding this row to cause an FK violation.
            fk_lookup_parent(p, i_db, to, idx, f, &ai_col, reg_new, 1, b_ignore);
        }
    }

    // Loop through all the foreign key constraints that refer to this table. (the "child"
    // constraints)
    for f in sqlite3_fk_references(p, tab) {
        let fkey = fk(p, f).clone();
        if let Some(ac) = a_change {
            if !fk_parent_is_modified(p, tab, &fkey, ac, b_chng_rowid) {
                continue;
            }
        }

        if !fkey.is_deferred && (p.db.flags & SQLITE_DeferFKs) == 0 && p.is_toplevel() && !p.is_multi_write {
            // Inserting a single row into a parent table cannot cause (or fix) an immediate
            // foreign key violation. So do nothing in this case.
            continue;
        }

        let Ok((idx, ai_col)) = sqlite3_fk_locate_index(p, tab, f, true) else {
            if !is_ignore_errors {
                return;
            }
            continue;
        };

        // Create a SrcList structure containing the child table. We need the child table
        // as a SrcList for sqlite3WhereBegin()
        let src = sqlite3_src_list_append(p, SrcListId::NULL, None, None);
        if src.nn() {
            let from = f.0;
            let name = p.tab(from).z_name.clone();
            let cur = p.n_tab;
            p.n_tab += 1;
            {
                let it = &mut p.srcm(src).a[0];
                it.p_s_tab = from;
                it.z_name = Some(name);
                it.i_cursor = cur;
            }
            p.tabm(from).n_tab_ref += 1;

            if reg_new != 0 {
                fk_scan_children(p, src, tab, idx, f, ai_col.as_deref(), reg_new, -1);
            }
            if reg_old != 0 {
                let mut e_action = fkey.a_action[a_change.is_some() as usize] as i32;
                if (p.db.flags & SQLITE_FkNoAction) != 0 {
                    e_action = OE_None;
                }
                fk_scan_children(p, src, tab, idx, f, ai_col.as_deref(), reg_old, 1);
                // If this is a deferred FK constraint, or a CASCADE or SET NULL action
                // applies, then any foreign key violations caused by removing the parent key
                // will be rectified by the action trigger. So do not set the "may-abort"
                // flag in this case.
                //
                // Note 1: If the FK is declared "ON UPDATE CASCADE", then the may-abort flag
                // will only be set if the update modifies a child key column.
                //
                // Note 2: At first glance it may seem like SQLite could simply omit all
                // OP_FkCounter related scans when either CASCADE or SET NULL applies. The
                // trouble starts if the CASCADE or SET NULL action trigger causes other
                // triggers or action rules attached to the child table to fire. In these
                // cases the fk constraint counters might be set incorrectly if any OP_FkCounter
                // related scans are omitted.
                if !fkey.is_deferred && e_action != OE_Cascade && e_action != OE_SetNull {
                    sqlite3_may_abort(p);
                }
            }
        }
    }
}

/// `COLUMN_MASK(x)`
fn column_mask(x: i32) -> u32 {
    if x > 31 { 0xffff_ffff } else { 1u32 << x }
}

/// `sqlite3FkOldmask(pParse, pTab)`: this function is called before generating code to update
/// or delete a row contained in table pTab. If the operation is a DELETE, then parameter
/// aChange is passed a NULL value. For an UPDATE, aChange points to an array of size N,
/// where N is the number of columns in table pTab. It returns a mask of the old.* columns
/// that the foreign key processing reads.
pub fn sqlite3_fk_oldmask(p: &mut Parse, tab: TableId) -> u32 {
    let mut mask: u32 = 0;
    if (p.db.flags & SQLITE_ForeignKeys) != 0 && p.tab(tab).is_ordinary() {
        for f in &p.tab(tab).p_fkey {
            for &(i_from, _) in &f.a_col {
                mask |= column_mask(i_from);
            }
        }
        for f in sqlite3_fk_references(p, tab) {
            if let Ok((idx, _)) = sqlite3_fk_locate_index(p, tab, f, false) {
                if idx.nn() {
                    for i in 0..p.idx(idx).n_key_col as usize {
                        mask |= column_mask(p.idx(idx).ai_column[i] as i32);
                    }
                }
            }
        }
    }
    mask
}

/// `sqlite3FkRequired(pParse, pTab, aChange, chngRowid)`: this function is called before
/// generating code to update or delete a row, or insert a new row, into table pTab. If
/// foreign key processing is required, return non-zero (2 if the operation may cause FK
/// actions or REPLACE-like row deletions that need a full old row).
pub fn sqlite3_fk_required(p: &Parse, tab: TableId, a_change: Option<&[i32]>, chng_rowid: bool) -> i32 {
    let mut e_ret = 1;
    let mut b_have_fk = false;
    if (p.db.flags & SQLITE_ForeignKeys) != 0 && p.tab(tab).is_ordinary() {
        match a_change {
            None => {
                // A DELETE operation. Foreign key processing is required if the table in
                // question is either the child or parent table for any foreign key
                // constraint.
                b_have_fk = !sqlite3_fk_references(p, tab).is_empty() || !p.tab(tab).p_fkey.is_empty();
            }
            Some(a_change) => {
                // This is an UPDATE. Foreign key processing is only required if the operation
                // modifies one or more child or parent key columns.

                // Check if any child key columns are being modified.
                for f in &p.tab(tab).p_fkey {
                    if fk_child_is_modified(p, tab, f, a_change, chng_rowid) {
                        if p.tab(tab).z_name.eq_ignore_ascii_case(&f.z_to) {
                            e_ret = 2;
                        }
                        b_have_fk = true;
                    }
                }

                // Check if any parent key columns are being modified.
                for f in sqlite3_fk_references(p, tab) {
                    let fkey = fk(p, f);
                    if fk_parent_is_modified(p, tab, fkey, a_change, chng_rowid) {
                        if (p.db.flags & SQLITE_FkNoAction) == 0 && fkey.a_action[1] as i32 != OE_None {
                            return 2;
                        }
                        b_have_fk = true;
                    }
                }
            }
        }
    }
    if b_have_fk { e_ret } else { 0 }
}

/// `fkActionTrigger(pParse, pTab, pFKey, pChanges)`: this function is called when an UPDATE
/// or DELETE operation is being compiled on table pTab, which is the parent table of foreign
/// key pFKey. If the foreign key has an ON DELETE / ON UPDATE action other than NO ACTION,
/// return the trigger that implements it (built on first use and kept for the statement).
fn fk_action_trigger(p: &mut Parse, tab: TableId, f: FkRef, changes: ExprListId) -> Option<TriggerId> {
    let fkey = fk(p, f).clone();
    let i_action = changes.nn() as usize;
    let mut action = fkey.a_action[i_action] as i32;
    if (p.db.flags & SQLITE_FkNoAction) != 0 {
        action = OE_None;
    }
    if action == OE_Restrict && (p.db.flags & SQLITE_DeferFKs) != 0 {
        return None;
    }
    let cached = p.fk_action_triggers.iter().find(|x| x.0 == f.0 && x.1 == f.1).and_then(|x| if i_action == 0 { x.2 } else { x.3 });
    if action == OE_None || cached.is_some() {
        return cached;
    }
    let from = f.0;
    let Ok((idx, ai_col)) = sqlite3_fk_locate_index(p, tab, f, true) else { return None };
    let mut where_ = ExprId::NULL;
    let mut when = ExprId::NULL;
    let mut list = ExprListId::NULL;
    let mut select = SelectId::NULL;

    for i in 0..fkey.a_col.len() {
        let i_from_col = match &ai_col {
            Some(a) => a[i],
            None => fkey.a_col[0].0,
        };
        let to_col = {
            let pc = if idx.nn() { p.idx(idx).ai_column[i] } else { p.tab(tab).i_p_key };
            p.tab(tab).a_col[pc as usize].z_cn_name.clone()
        };
        let from_col = p.tab(from).a_col[i_from_col as usize].z_cn_name.clone();

        // Create the expression "OLD.zToCol = zFromCol". It is important that the "OLD.zToCol"
        // term is on the LHS of the = operator, so that the affinity and collation sequence
        // associated with the parent table are used for the comparison.
        let a = sqlite3_expr_alloc_str(p, TK_ID, Some("old"), 0);
        let b = sqlite3_expr_alloc_str(p, TK_ID, Some(&to_col), 0);
        let dot = sqlite3_p_expr(p, TK_DOT, a, b);
        let c = sqlite3_expr_alloc_str(p, TK_ID, Some(&from_col), 0);
        let eq = sqlite3_p_expr(p, TK_EQ, dot, c);
        where_ = sqlite3_expr_and(p, where_, eq);

        // For ON UPDATE, construct the next term of the WHEN clause. The final WHEN clause
        // will be like this:
        //
        //    WHEN NOT(old.col1 IS new.col1 AND ... AND old.colN IS new.colN)
        if changes.nn() {
            let a = sqlite3_expr_alloc_str(p, TK_ID, Some("old"), 0);
            let b = sqlite3_expr_alloc_str(p, TK_ID, Some(&to_col), 0);
            let l = sqlite3_p_expr(p, TK_DOT, a, b);
            let a = sqlite3_expr_alloc_str(p, TK_ID, Some("new"), 0);
            let b = sqlite3_expr_alloc_str(p, TK_ID, Some(&to_col), 0);
            let r = sqlite3_p_expr(p, TK_DOT, a, b);
            let eq = sqlite3_p_expr(p, TK_IS, l, r);
            when = sqlite3_expr_and(p, when, eq);
        }

        if action != OE_Restrict && (action != OE_Cascade || changes.nn()) {
            let new;
            if action == OE_Cascade {
                let a = sqlite3_expr_alloc_str(p, TK_ID, Some("new"), 0);
                let b = sqlite3_expr_alloc_str(p, TK_ID, Some(&to_col), 0);
                new = sqlite3_p_expr(p, TK_DOT, a, b);
            } else if action == OE_SetDflt {
                let cf = p.tab(from).a_col[i_from_col as usize].col_flags;
                let dflt = if cf & COLFLAG_GENERATED as u16 != 0 {
                    ExprId::NULL
                } else {
                    crate::build_tab::sqlite3_column_expr(p, from, i_from_col as usize)
                };
                new = if dflt.nn() { sqlite3_expr_dup(p, dflt, 0) } else { sqlite3_expr_alloc_str(p, TK_NULL, None, 0) };
            } else {
                new = sqlite3_expr_alloc_str(p, TK_NULL, None, 0);
            }
            list = sqlite3_expr_list_append(p, list, new);
            // sqlite3ExprListSetName(pParse, pList, &tFromCol, 0)
            let it = p.lm(list).a.last_mut().expect("item");
            it.z_e_name = Some(from_col.clone());
            it.e_e_name = ENAME_NAME as u8;
        }
    }

    let z_from = p.tab(from).z_name.clone();
    let tab_db = p.tab(tab).i_db;
    if action == OE_Restrict {
        let raise_msg = sqlite3_expr(p, TK_STRING, Some("FOREIGN KEY constraint failed"));
        let raise = sqlite3_p_expr(p, TK_RAISE, raise_msg, ExprId::NULL);
        if raise.nn() {
            p.em(raise).aff_expr = OE_Abort as u8;
        }
        let src = sqlite3_src_list_append(p, SrcListId::NULL, None, None);
        if src.nn() {
            let it = &mut p.srcm(src).a[0];
            it.z_name = Some(z_from.clone());
            it.fixed_schema = true;
            it.i_fixed_db = tab_db;
        }
        let el = sqlite3_expr_list_append(p, ExprListId::NULL, raise);
        select = crate::select::sqlite3_select_new(p, el, src, where_, ExprListId::NULL, ExprId::NULL, ExprListId::NULL, 0, ExprId::NULL);
        where_ = ExprId::NULL;
    }

    let step_src = sqlite3_src_list_append(p, SrcListId::NULL, None, None);
    if step_src.nn() {
        let it = &mut p.srcm(step_src).a[0];
        it.z_name = Some(z_from);
        it.i_fixed_db = tab_db;
        it.fixed_schema = true;
    }
    let mut step = TriggerStep {
        p_src: step_src,
        p_where: sqlite3_expr_dup(p, where_, EXPRDUP_REDUCE),
        p_expr_list: sqlite3_expr_list_dup(p, list, EXPRDUP_REDUCE),
        p_select: sqlite3_select_dup(p, select, EXPRDUP_REDUCE),
        ..TriggerStep::default()
    };
    let mut trig_when = ExprId::NULL;
    if when.nn() {
        let w = sqlite3_p_expr(p, TK_NOT, when, ExprId::NULL);
        trig_when = sqlite3_expr_dup(p, w, EXPRDUP_REDUCE);
    }
    step.op = match action {
        OE_Restrict => TK_SELECT,
        OE_Cascade if changes.is_null() => TK_DELETE,
        _ => TK_UPDATE,
    };
    let id = p.new_trigger(Trigger {
        z_name: None,
        table: String::new(),
        op: if changes.nn() { TK_UPDATE } else { TK_DELETE },
        tr_tm: 0,
        b_returning: false,
        p_when: trig_when,
        p_columns: IdListId::NULL,
        i_schema: tab_db,
        i_tab_schema: tab_db,
        step_list: vec![step],
    });
    match p.fk_action_triggers.iter_mut().find(|x| x.0 == f.0 && x.1 == f.1) {
        Some(x) => {
            if i_action == 0 {
                x.2 = Some(id);
            } else {
                x.3 = Some(id);
            }
        }
        None => {
            let (a0, a1) = if i_action == 0 { (Some(id), None) } else { (None, Some(id)) };
            p.fk_action_triggers.push((f.0, f.1, a0, a1));
        }
    }
    Some(id)
}

/// `sqlite3FkActions(pParse, pTab, pChanges, regOld, aChange, bChngRowid)`: this function is
/// called when deleting or updating a row to implement any required CASCADE, SET NULL or
/// SET DEFAULT actions.
pub fn sqlite3_fk_actions(p: &mut Parse, tab: TableId, changes: ExprListId, reg_old: i32, a_change: Option<&[i32]>, b_chng_rowid: bool) {
    // If foreign-key support is enabled, iterate through all FKs that refer to table pTab.
    // If there is an action associated with the FK for this operation (either update or
    // delete), invoke the associated trigger sub-program.
    if (p.db.flags & SQLITE_ForeignKeys) != 0 {
        for f in sqlite3_fk_references(p, tab) {
            let modified = match a_change {
                None => true,
                Some(ac) => {
                    let fkey = fk(p, f).clone();
                    fk_parent_is_modified(p, tab, &fkey, ac, b_chng_rowid)
                }
            };
            if modified {
                if let Some(act) = fk_action_trigger(p, tab, f, changes) {
                    crate::trigger::sqlite3_code_row_trigger_direct(p, act, tab, reg_old, OE_Abort, 0);
                }
            }
        }
    }
}

/// `sqlite3FkDropTable(pParse, pName, pTab)`: this function is called to generate code that
/// runs when table pTab is being dropped from the database. The SrcList passed as the second
/// argument to this function contains a single entry guaranteed to resolve to table pTab.
///
/// Normally, no code is required. However, if either
///
///   (a) The table is the parent table of a FK constraint, or
///   (b) The table is the child table of a deferred FK constraint and it is determined at
///       runtime that there are outstanding deferred FK constraint violations in the
///       database,
///
/// then the equivalent of "DELETE FROM <tbl>" is executed before dropping the table from the
/// database. If (a) is true and the DELETE triggers a FK violation, an exception is thrown,
/// as in "DELETE FROM <tbl>".
pub fn sqlite3_fk_drop_table(p: &mut Parse, name: SrcListId, tab: TableId) {
    if (p.db.flags & SQLITE_ForeignKeys) != 0 && p.tab(tab).is_ordinary() {
        let mut i_skip = 0;
        crate::select::sqlite3_get_vdbe(p);
        if sqlite3_fk_references(p, tab).is_empty() {
            // Search for a deferred foreign key constraint for which this table is the child
            // table. If one cannot be found, return without generating any code. Otherwise,
            // jump over the DELETE statement when the deferred foreign key constraint counter
            // is zero.
            let deferred = p
                .tab(tab)
                .p_fkey
                .iter()
                .any(|f| f.is_deferred || (p.db.flags & SQLITE_DeferFKs) != 0);
            if !deferred {
                return;
            }
            i_skip = p.v.make_label();
            p.v.add_op2(OP_FkIfZero, 1, i_skip);
        }

        p.disable_triggers = true;
        let src = sqlite3_src_list_dup(p, name, 0);
        crate::delete::sqlite3_delete_from(p, src, ExprId::NULL, ExprListId::NULL, ExprId::NULL);
        p.disable_triggers = false;

        // If the DELETE has generated immediate foreign key constraint violations, halt the
        // VDBE program and return an error at this point, before any modifications of the
        // schema. Since triggers are disabled, it is not possible for this to have occurred
        // within any sub-program. If FKs are deferred, the violations will be caught at
        // commit time.
        if (p.db.flags & SQLITE_DeferFKs) == 0 {
            let a = p.v.current_addr() + 2;
            p.v.add_op2(OP_FkIfZero, 0, a);
            sqlite3_halt_constraint(
                p,
                SQLITE_CONSTRAINT_FOREIGNKEY,
                OE_Abort,
                P4::NotUsed,
                P5_ConstraintFK as i32,
            );
        }

        if i_skip != 0 {
            p.v.resolve_label(i_skip);
        }
    }
}
