//! Port of `trigger.c` (code side): finding the triggers of a statement
//! (`sqlite3TriggerList`, `sqlite3TriggersExist`), the RETURNING pseudo-trigger
//! (`sqlite3AddReturning` of build.c, `codeReturningTrigger`), `sqlite3CodeRowTrigger` and
//! `sqlite3TriggerColmask`.

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{SubProgram, P4};

use crate::consts::*;
use crate::expr::*;
use crate::exprcode::sqlite3_expr_code_factorable;
use crate::resolve::sqlite3_resolve_expr_list_names;
use crate::sqliteint::*;
use crate::tk::*;
use crate::walker::*;

/// `sqlite3TriggerList(pParse, pTab)`: given table pTab, return a list of all the triggers
/// attached to the table. The list is connected by Trigger.pNext pointers.
///
/// All of the triggers on pTab that are in the same database as pTab are already attached
/// to the Table object. But there might be additional triggers on pTab that are in the TEMP
/// schema. Those triggers are appended (at the front) here. RETURNING triggers are appended
/// as well.
pub fn sqlite3_trigger_list(p: &mut Parse, tab: TableId) -> Vec<TriggerId> {
    let mut list: Vec<TriggerId> = p.tab(tab).p_trigger.clone();
    let tab_schema = p.tab(tab).i_db;
    let tmp: Vec<TriggerId> = p.a_schema.get(1).map(|s| s.trig_hash.ids()).unwrap_or_default();
    for trig in tmp {
        let (t_schema, table, b_ret, op) = {
            let t = p.trig(trig);
            (t.i_tab_schema, t.table.clone(), t.b_returning, t.op)
        };
        if t_schema == tab_schema
            && !table.is_empty()
            && table.eq_ignore_ascii_case(&p.tab(tab).z_name)
            && (t_schema != 1 || b_ret)
        {
            list.insert(0, trig);
        } else if op == TK_RETURNING {
            let name = p.tab(tab).z_name.clone();
            let t = p.trigm(trig);
            t.table = name;
            t.i_tab_schema = tab_schema;
            list.insert(0, trig);
        }
    }
    list
}

/// `checkColumnOverlap(pIdList, pEList)`: this function is used to determine whether or not
/// a trigger on table pTab should fire given the set of columns being updated (pEList).
fn check_column_overlap(p: &Parse, id_list: IdListId, e_list: ExprListId) -> bool {
    if id_list.is_null() || e_list.is_null() {
        return true;
    }
    for it in &p.l(e_list).a {
        if let Some(z) = &it.z_e_name {
            if crate::build::sqlite3_id_list_index(p, id_list, z) >= 0 {
                return true;
            }
        }
    }
    false
}

/// `tempTriggersExist(db)`: return true if any TEMP triggers exist.
fn temp_triggers_exist(p: &Parse) -> bool {
    p.a_schema.get(1).is_some_and(|s| !s.trig_hash.is_empty())
}

/// `sqlite3TriggersExist(pParse, pTab, op, pChanges, pMask)`: return a list of all triggers
/// on table pTab if there exists at least one trigger that is fired by the statement (an
/// empty list otherwise). `mask` receives the TRIGGER_BEFORE / TRIGGER_AFTER bits.
pub fn sqlite3_triggers_exist(p: &mut Parse, tab: TableId, op: u8, changes: ExprListId, mask: Option<&mut i32>) -> Vec<TriggerId> {
    if !p.disable_triggers {
        // A trigger of the table whose SQL text the generator could not load
        let key = p.tab(tab).z_name.to_ascii_lowercase();
        if let Some(why) = p.a_schema.iter().find_map(|s| s.bad_trig.get(&key).copied()) {
            p.not_supported(why);
            if let Some(m) = mask {
                *m = 0;
            }
            return Vec::new();
        }
    }
    if (p.tab(tab).p_trigger.is_empty() && !temp_triggers_exist(p)) || p.disable_triggers {
        if let Some(m) = mask {
            *m = 0;
        }
        return Vec::new();
    }
    triggers_really_exist(p, tab, op, changes, mask)
}

/// `triggersReallyExist(pParse, pTab, op, pChanges, pMask)`
fn triggers_really_exist(p: &mut Parse, tab: TableId, op: u8, changes: ExprListId, mask_out: Option<&mut i32>) -> Vec<TriggerId> {
    let mut mask = 0;
    let mut list = sqlite3_trigger_list(p, tab);
    if !list.is_empty() {
        if (p.db.flags & SQLITE_EnableTrigger) == 0 {
            if let Some(&first_own) = p.tab(tab).p_trigger.first() {
                if p.trig(first_own).i_schema != 1 {
                    // The SQLITE_DBCONFIG_ENABLE_TRIGGER setting is off. That means that only
                    // TEMP triggers are allowed. Truncate the pList so that it includes only
                    // TEMP triggers
                    if list[0] == first_own {
                        list.clear();
                    } else if let Some(pos) = list.iter().position(|&t| t == first_own) {
                        list.truncate(pos);
                    }
                }
            }
        }
        for &t in &list {
            let (t_op, cols, b_ret, tr_tm) = {
                let x = p.trig(t);
                (x.op, x.p_columns, x.b_returning, x.tr_tm)
            };
            if t_op == op && check_column_overlap(p, cols, changes) {
                mask |= tr_tm as i32;
            } else if t_op == TK_RETURNING {
                // The first time a RETURNING trigger is seen, the "op" value tells us what
                // kind of trigger it is (INSERT, UPDATE, or DELETE).
                p.trigm(t).op = op;
                if p.tab(tab).is_virtual() {
                    if op != TK_INSERT {
                        let what = if op == TK_DELETE { "DELETE" } else { "UPDATE" };
                        p.error_msg(format!("{what} RETURNING is not available on virtual tables"));
                    }
                    p.trigm(t).tr_tm = TRIGGER_BEFORE as u8;
                } else {
                    p.trigm(t).tr_tm = TRIGGER_AFTER as u8;
                }
                mask |= p.trig(t).tr_tm as i32;
            } else if b_ret && t_op == TK_INSERT && op == TK_UPDATE && p.is_toplevel() {
                // Also fire a RETURNING trigger for an UPSERT
                mask |= tr_tm as i32;
            }
        }
    }
    if let Some(m) = mask_out {
        *m = mask;
    }
    if mask != 0 { list } else { Vec::new() }
}

/// `sqlite3AddReturning(pParse, pList)` (build.c): add the RETURNING clause to the parse
/// currently underway.
///
/// This routine creates a special TEMP trigger that will fire for each row of the DML
/// statement. That TEMP trigger contains a single SELECT statement which will be computed
/// for each row of the DML statement and the results stored in an ephemeral table.
pub fn sqlite3_add_returning(p: &mut Parse, list: ExprListId) {
    if p.p_new_trigger {
        p.error_msg("cannot use RETURNING in a trigger");
    }
    p.b_returning = true;
    let trig = p.new_trigger(Trigger {
        z_name: Some("sqlite_returning".to_owned()),
        op: TK_RETURNING,
        tr_tm: TRIGGER_AFTER as u8,
        b_returning: true,
        i_schema: 1,
        i_tab_schema: 1,
        step_list: vec![TriggerStep { op: TK_RETURNING, p_expr_list: list, ..TriggerStep::default() }],
        ..Trigger::default()
    });
    p.p_returning = Some(Returning { p_return_el: list, ret_trig: trig, ..Returning::default() });
    // The trigger joins the TEMP schema's trigger hash. When C's connection has its schemas
    // loaded, the TEMP triggers are in the hash already (this Parse loads the schema objects
    // on first use: load them now so that the hash has C's order).
    if !p.db.schema_not_loaded && !p.db.init_busy {
        crate::build_tab::sqlite3_init(p);
    }
    while p.a_schema.len() < 2 {
        p.a_schema.push(SchemaState::default());
    }
    p.a_schema[1].trig_hash.insert("sqlite_returning", trig);
}

/// `isAsteriskTerm(pParse, pTerm)`: return true if the pExpr term from the RETURNING clause
/// argument list is of the form "*". Raise an error if the terms if of the form "table.*".
fn is_asterisk_term(p: &mut Parse, term: ExprId) -> bool {
    if p.e(term).op == TK_ASTERISK {
        return true;
    }
    if p.e(term).op != TK_DOT {
        return false;
    }
    let r = p.e(term).p_right;
    if p.e(r).op != TK_ASTERISK {
        return false;
    }
    p.error_msg("RETURNING may not use \"TABLE.*\" wildcards");
    true
}

/// `sqlite3ExpandReturning(pParse, pList, pTab)`: the input list pList is the list of result
/// set terms from a RETURNING clause. The column names for the result set have already been
/// computed. This routine expands any "*" terms into the table's columns.
fn sqlite3_expand_returning(p: &mut Parse, list: ExprListId, tab: TableId) -> ExprListId {
    let mut new = ExprListId::NULL;
    for i in 0..p.l(list).a.len() {
        let old = p.l(list).a[i].p_expr;
        if old.is_null() {
            continue;
        }
        if is_asterisk_term(p, old) {
            for jj in 0..p.tab(tab).a_col.len() {
                if p.tab(tab).a_col[jj].col_flags & COLFLAG_HIDDEN as u16 != 0 {
                    continue;
                }
                let name = p.tab(tab).a_col[jj].z_cn_name.clone();
                let ne = sqlite3_expr(p, TK_ID, Some(&name));
                new = sqlite3_expr_list_append(p, new, ne);
                let it = p.lm(new).a.last_mut().expect("item");
                it.z_e_name = Some(name);
                it.e_e_name = ENAME_NAME as u8;
            }
        } else {
            let ne = sqlite3_expr_dup(p, old, 0);
            new = sqlite3_expr_list_append(p, new, ne);
            let (z, e) = (p.l(list).a[i].z_e_name.clone(), p.l(list).a[i].e_e_name);
            if z.is_some() {
                let it = p.lm(new).a.last_mut().expect("item");
                it.z_e_name = z;
                it.e_e_name = e;
            }
        }
    }
    new
}

/// `sqlite3ReturningSubqueryVarSelect(NotUsed, pExpr)`: if the expression is a correlated
/// subquery, set EP_VarSelect on it.
fn returning_subquery_var_select(p: &mut Parse, _w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).use_x_select() && (p.s(p.e(e).x_select()).sel_flags & SF_Correlated) != 0 {
        p.em(e).flags |= EP_VarSelect;
    }
    WRC_Continue
}

/// `sqlite3ReturningSubqueryCorrelated(pWalker, pSelect)`: if the SELECT references the
/// table pWalker->u.pTab, then do the following: (1) mark the SELECT as correlated,
/// (2) set pWalker->eCode to non-zero so that the caller will know that (1) happened.
fn returning_subquery_correlated(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let src = p.s(sel).p_src;
    if src.nn() {
        for i in 0..p.src(src).a.len() {
            if p.src(src).a[i].p_s_tab == w.p_tab {
                p.sm(sel).sel_flags |= SF_Correlated;
                w.e_code = 1;
                break;
            }
        }
    }
    WRC_Continue
}

/// `sqlite3ProcessReturningSubqueries(pEList, pTab)`: scan the expression list that is the
/// argument to RETURNING looking for subqueries that depend on the table which is being
/// modified. Mark any such subqueries as "correlated" so that they will be recomputed for
/// each row of the result.
fn sqlite3_process_returning_subqueries(p: &mut Parse, e_list: ExprListId, tab: TableId) {
    let mut w = Walker {
        x_expr_callback: Some(sqlite3_expr_walk_noop),
        x_select_callback: Some(returning_subquery_correlated),
        p_tab: tab,
        ..Walker::default()
    };
    sqlite3_walk_expr_list(p, &mut w, e_list);
    if w.e_code != 0 {
        w.x_expr_callback = Some(returning_subquery_var_select);
        w.x_select_callback = Some(sqlite3_select_walk_noop);
        sqlite3_walk_expr_list(p, &mut w, e_list);
    }
}

/// `codeReturningTrigger(pParse, pTrigger, pTab, regIn)`: generate code for the RETURNING
/// trigger. Unlike other triggers that invoke a subprogram, the RETURNING trigger is coded
/// inline.
fn code_returning_trigger(p: &mut Parse, trigger: TriggerId, tab: TableId, reg_in: i32) {
    if !p.b_returning {
        // This RETURNING trigger must be for a different statement as this statement
        // does not have a RETURNING clause.
        return;
    }
    let Some(ret) = p.p_returning.clone() else { return };
    if trigger != ret.ret_trig {
        // This RETURNING trigger is for a different statement
        return;
    }
    let e_list = sqlite3_expr_list_dup(p, ret.p_return_el, 0);
    let mut from_item = SrcItem { p_s_tab: tab, z_name: Some(p.tab(tab).z_name.clone()), i_cursor: -1, ..SrcItem::default() };
    from_item.jointype = 0;
    let from = p.new_src(SrcList { a: vec![from_item] });
    let sel = p.new_select(Select { p_e_list: e_list, p_src: from, ..Select::default() });
    crate::select::sqlite3_select_prep(p, sel, NcId::NULL);
    if p.n_err == 0 {
        crate::select::sqlite3_generate_column_names(p, sel);
    }
    let new = sqlite3_expand_returning(p, ret.p_return_el, tab);
    if p.n_err == 0 {
        let mut r = ret.clone();
        if r.n_ret_col == 0 {
            r.n_ret_col = p.l(new).n_expr();
            r.i_ret_cur = p.n_tab;
            p.n_tab += 1;
        }
        let nc = p.new_nc(NameContext { i_base_reg: reg_in, nc_flags: NC_UBaseReg, ..NameContext::default() });
        p.e_trigger_op = p.trig(trigger).op;
        p.p_trigger_tab = tab;
        p.p_returning = Some(r.clone());
        if sqlite3_resolve_expr_list_names(p, nc, new) == SQLITE_OK {
            let n_col = p.l(new).n_expr();
            let reg = p.n_mem + 1;
            sqlite3_process_returning_subqueries(p, new, tab);
            p.n_mem += n_col + 2;
            r.i_ret_reg = reg;
            p.p_returning = Some(r.clone());
            let mut i = 0;
            while i < n_col {
                let col = p.l(new).a[i as usize].p_expr;
                sqlite3_expr_code_factorable(p, col, reg + i);
                if sqlite3_expr_affinity(p, col) == SQLITE_AFF_REAL {
                    p.v.add_op1(OP_RealAffinity, reg + i);
                }
                i += 1;
            }
            p.v.add_op3(OP_MakeRecord, reg, i, reg + i);
            p.v.add_op2(OP_NewRowid, r.i_ret_cur, reg + i + 1);
            p.v.add_op3(OP_Insert, r.i_ret_cur, reg + i, reg + i + 1);
        }
    }
    p.e_trigger_op = 0;
    p.p_trigger_tab = TableId::NULL;
}

/// `sqlite3CodeRowTrigger(pParse, pTrigger, op, pChanges, tr_tm, pTab, reg, orconf,
/// ignoreJump)`: this is called to code the required FOR EACH ROW triggers for an operation
/// on table pTab. The operation to code triggers for (INSERT, UPDATE or DELETE) is given by
/// the op parameter. The tr_tm parameter determines whether the BEFORE or AFTER triggers are
/// coded. If the operation is an UPDATE, then parameter pChanges is passed the list of
/// columns being modified.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_code_row_trigger(
    p: &mut Parse,
    triggers: &[TriggerId],
    op: u8,
    changes: ExprListId,
    tr_tm: i32,
    tab: TableId,
    reg: i32,
    orconf: i32,
    ignore_jump: i32,
) {
    for &t in triggers {
        let (t_op, b_ret, t_tm, cols) = {
            let x = p.trig(t);
            (x.op, x.b_returning, x.tr_tm, x.p_columns)
        };
        // Determine whether we should code this trigger. One of two choices: Option A: the
        // trigger is of the right type (op), timing (tr_tm) and columns; Option B: it is
        // the RETURNING trigger of an UPSERT that does an UPDATE.
        if (t_op == op || (b_ret && t_op == TK_INSERT && op == TK_UPDATE))
            && t_tm as i32 == tr_tm
            && check_column_overlap(p, cols, changes)
        {
            if !b_ret {
                sqlite3_code_row_trigger_direct(p, t, tab, reg, orconf, ignore_jump);
            } else if p.is_toplevel() {
                code_returning_trigger(p, t, tab, reg);
            }
        }
    }
}

/// `sqlite3TriggerColmask(pParse, pTrigger, pChanges, isNew, tr_tm, pTab, orconf)`: return
/// a bit vector to indicate which columns of the old.* or new.* row the triggers use.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_trigger_colmask(
    p: &mut Parse,
    triggers: &[TriggerId],
    changes: ExprListId,
    is_new: i32,
    tr_tm: i32,
    tab: TableId,
    orconf: i32,
) -> u32 {
    let op = if changes.nn() { TK_UPDATE } else { TK_DELETE };
    let mut mask: u32 = 0;
    if p.tab(tab).is_view() {
        return 0xffff_ffff;
    }
    for &t in triggers {
        let (t_op, b_ret, t_tm, cols) = {
            let x = p.trig(t);
            (x.op, x.b_returning, x.tr_tm, x.p_columns)
        };
        if t_op == op && (tr_tm & t_tm as i32) != 0 && check_column_overlap(p, cols, changes) {
            if b_ret {
                mask = 0xffff_ffff;
            } else if let Some(k) = get_row_trigger(p, t, tab, orconf) {
                mask |= p.p_trigger_prg[k].a_colmask[is_new as usize];
            }
        }
    }
    mask
}

// ---------------------------------------------------------------------------------------------
// hash.c: the iteration order of a schema's `trigHash`
// ---------------------------------------------------------------------------------------------

/// `strHash(z)` of hash.c.
fn str_hash(z: &str) -> u32 {
    let mut h: u32 = 0;
    for &b in z.as_bytes() {
        h = h.wrapping_add((0xdf & b) as u32);
        h = h.wrapping_mul(0x9e37_79b1);
    }
    h
}

/// A C `Hash` of triggers (`Schema.trigHash`) as far as its element order matters:
/// `sqlite3TriggerList()` walks the TEMP schema's hash from `first` to the end.
#[derive(Debug, Clone, Default)]
pub struct TrigHash {
    /// The elements in list order (`first`, `next`, ...): (key, hash, trigger)
    order: Vec<(String, u32, TriggerId)>,
    /// `ht[]`: per bucket (count, key of the chain head)
    ht: Vec<(u32, Option<String>)>,
}

impl TrigHash {
    /// The triggers in iteration order.
    pub fn ids(&self) -> Vec<TriggerId> {
        self.order.iter().map(|e| e.2).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
    /// `sqlite3HashFind(pH, zName)`
    pub fn find(&self, key: &str) -> Option<TriggerId> {
        self.order.iter().find(|e| e.0.eq_ignore_ascii_case(key)).map(|e| e.2)
    }
    /// `insertElement(pH, pEntry, pNew)`
    fn insert_element(&mut self, bucket: Option<usize>, elem: (String, u32, TriggerId)) {
        let head = match bucket {
            Some(b) => {
                let e = &mut self.ht[b];
                let head = if e.0 > 0 { e.1.clone() } else { None };
                e.0 += 1;
                e.1 = Some(elem.0.clone());
                head
            }
            None => None,
        };
        match head.and_then(|k| self.order.iter().position(|e| e.0 == k)) {
            Some(pos) => self.order.insert(pos, elem),
            None => self.order.insert(0, elem),
        }
    }
    /// `sqlite3HashInsert(pH, pKey, data)` of a new key.
    pub fn insert(&mut self, key: &str, id: TriggerId) {
        let h = str_hash(key);
        let count = self.order.len() + 1;
        if count >= 5 && count > 2 * self.ht.len() {
            // rehash(pH, pH->count*3), at most SQLITE_MALLOC_SOFT_LIMIT bytes of buckets
            let mut new_size = count * 3;
            if new_size * 16 > 1024 {
                new_size = 1024 / 16;
            }
            if new_size != self.ht.len() {
                let old = std::mem::take(&mut self.order);
                self.ht = vec![(0, None); new_size];
                for e in old {
                    let b = (e.1 as usize) % new_size;
                    self.insert_element(Some(b), e);
                }
            }
        }
        let bucket = if self.ht.is_empty() { None } else { Some((h as usize) % self.ht.len()) };
        self.insert_element(bucket, (key.to_owned(), h, id));
    }
}

// ---------------------------------------------------------------------------------------------
// CREATE TRIGGER at schema load
// ---------------------------------------------------------------------------------------------

/// `sqlite3BeginTrigger(pParse, pName1, pName2, tr_tm, op, pColumns, pTableName, pWhen,
/// isTemp, noErr)`: this is called by the parser when it sees a CREATE TRIGGER statement up
/// to the point of the BEGIN before the trigger actions. Only the schema load
/// (`db->init.busy`) is ported: the trigger is built for `sqlite3FinishTrigger()`.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_begin_trigger(
    p: &mut Parse,
    name1: &Token,
    name2: &Token,
    mut tr_tm: i32,
    op: i32,
    columns: IdListId,
    table_name: SrcListId,
    when: ExprId,
    is_temp: bool,
    _no_err: bool,
) {
    let mut name = Token::EMPTY;
    let mut i_db;
    if is_temp {
        // If TEMP was specified, then the trigger name may not be qualified.
        if name2.n > 0 {
            p.error_msg("temporary trigger may not have qualified name");
            return;
        }
        i_db = 1;
        name = *name1;
    } else {
        // Figure out the db that the trigger will be created in
        i_db = crate::build_tab::sqlite3_two_part_name(p, name1, name2, &mut name);
        if i_db < 0 {
            return;
        }
    }
    if table_name.is_null() {
        return;
    }

    // A long-standing parser bug is that this syntax was allowed:
    //
    //    CREATE TRIGGER attached.demo AFTER INSERT ON attached.tab ....
    //                                                 ^^^^^^^^
    //
    // To maintain backwards compatibility, ignore the database name on pTableName if we are
    // reparsing a schema object.
    if p.db.init_busy && i_db != 1 {
        p.srcm(table_name).a[0].z_database = None;
    }

    // If the trigger name was unqualified, and the table is a temp table, then set iDb to 1
    // to create the trigger in the temporary database.
    let tab = crate::delete::sqlite3_src_list_lookup(p, table_name);
    if !p.db.init_busy && name2.n == 0 && tab.nn() && p.tab(tab).i_db == 1 {
        i_db = 1;
    }

    // Ensure the table name matches database name and that the table exists
    let tok_name = sqlite3_name_of_token(p, &name);
    p.init_az[1].clone_from(&tok_name);
    if fix_src_list(p, table_name, i_db, true) {
        return;
    }
    let tab = crate::delete::sqlite3_src_list_lookup(p, table_name);
    if tab.is_null() {
        // The table does not exist.
        return;
    }
    if p.tab(tab).is_virtual() {
        p.error_msg("cannot create triggers on virtual tables");
        return;
    }
    if (p.tab(tab).tab_flags & TF_Shadow) != 0 && (p.db.flags & SQLITE_Defensive) != 0 {
        p.error_msg("cannot create triggers on shadow tables");
        return;
    }

    // Check that the trigger name is not reserved and that no trigger of the specified name
    // exists
    let z_name = tok_name;
    let tname = p.tab(tab).z_name.clone();
    if crate::build_tab::sqlite3_check_object_name(p, &z_name, "trigger", &tname) {
        return;
    }
    while p.a_schema.len() <= i_db as usize {
        p.a_schema.push(SchemaState::default());
    }
    if p.a_schema[i_db as usize].trig_hash.find(&z_name).is_some() {
        p.error_msg_tok(format!("trigger {} already exists", p.tok_str(&name)), &name);
        return;
    }

    // Do not create a trigger on a system table
    if tname.len() >= 7 && tname.as_bytes()[..7].eq_ignore_ascii_case(b"sqlite_") {
        p.error_msg("cannot create trigger on system table");
        return;
    }

    // INSTEAD of triggers are only for views and views only support INSTEAD of triggers.
    if p.tab(tab).is_view() && tr_tm != TK_INSTEAD as i32 {
        let what = if tr_tm == TK_BEFORE as i32 { "BEFORE" } else { "AFTER" };
        let n = crate::wherecode::src_item_name(p, table_name, 0, false);
        p.error_msg(format!("cannot create {what} trigger on view: {n}"));
        return;
    }
    if !p.tab(tab).is_view() && tr_tm == TK_INSTEAD as i32 {
        let n = crate::wherecode::src_item_name(p, table_name, 0, false);
        p.error_msg(format!("cannot create INSTEAD OF trigger on table: {n}"));
        return;
    }
    // (sqlite3AuthCheck: no authorizer at schema load)

    // INSTEAD OF triggers can only appear on views and BEFORE triggers cannot appear on
    // views. So we might as well translate every INSTEAD OF trigger into a BEFORE trigger. It
    // simplifies code elsewhere.
    if tr_tm == TK_INSTEAD as i32 {
        tr_tm = TK_BEFORE as i32;
    }

    // Build the Trigger object
    let when = sqlite3_expr_dup(p, when, EXPRDUP_REDUCE);
    let table = p.src(table_name).a[0].z_name.clone().unwrap_or_default();
    p.new_trigger = Some(Trigger {
        z_name: Some(z_name),
        table,
        op: op as u8,
        tr_tm: if tr_tm == TK_BEFORE as i32 { TRIGGER_BEFORE as u8 } else { TRIGGER_AFTER as u8 },
        b_returning: false,
        p_when: when,
        p_columns: columns,
        i_schema: i_db,
        i_tab_schema: p.tab(tab).i_db,
        step_list: Vec::new(),
    });
    p.p_new_trigger = true;
}

/// `sqlite3NameFromToken()` of a trigger name token.
fn sqlite3_name_of_token(p: &Parse, t: &Token) -> String {
    crate::build::sqlite3_name_from_token(p, Some(t)).unwrap_or_default()
}

/// `sqlite3FixSrcList(&sFix, pList)` with `sqlite3FixInit(&sFix, pParse, iDb, "trigger", ..)`.
fn fix_src_list(p: &mut Parse, src: SrcListId, i_db: i32, _is_trigger: bool) -> bool {
    if src.is_null() {
        return false;
    }
    let s = p.new_select(Select { p_src: src, ..Select::default() });
    let mut w = fixer(i_db);
    sqlite3_walk_select(p, &mut w, s) != 0
}

/// The walker of a `DbFixer` for a trigger of database `i_db`.
fn fixer(i_db: i32) -> Walker {
    Walker {
        x_expr_callback: Some(crate::build_tab::fix_expr_cb),
        x_select_callback: Some(crate::build_tab::fix_select_cb),
        n: i_db,
        b1: i_db == 1,
        i2: 1,
        walk_win_defn: true,
        ..Walker::default()
    }
}

/// `sqlite3FixTriggerStep(&sFix, pStep)`
fn fix_trigger_steps(p: &mut Parse, steps: &[TriggerStep], i_db: i32) -> bool {
    let mut w = fixer(i_db);
    for st in steps {
        if sqlite3_walk_select(p, &mut w, st.p_select) != 0
            || sqlite3_walk_expr(p, &mut w, st.p_where) != 0
            || sqlite3_walk_expr_list(p, &mut w, st.p_expr_list) != 0
            || fix_src_list(p, st.p_src, i_db, true)
        {
            return true;
        }
        let mut up = st.p_upsert;
        while up.nn() {
            let u = p.up(up).clone();
            if sqlite3_walk_expr_list(p, &mut w, u.p_upsert_target) != 0
                || sqlite3_walk_expr(p, &mut w, u.p_upsert_target_where) != 0
                || sqlite3_walk_expr_list(p, &mut w, u.p_upsert_set) != 0
                || sqlite3_walk_expr(p, &mut w, u.p_upsert_where) != 0
            {
                return true;
            }
            up = u.p_next_upsert;
        }
    }
    false
}

/// `sqlite3FinishTrigger(pParse, pStepList, pAll)`: this routine is called after all of the
/// trigger actions have been parsed in order to complete the process of building the
/// trigger (schema load: the trigger joins the schema's trigger hash and its table).
pub fn sqlite3_finish_trigger(p: &mut Parse, step_list: Vec<TriggerStep>) {
    let Some(mut trig) = p.new_trigger.take() else { return };
    p.p_new_trigger = false;
    if p.n_err != 0 {
        return;
    }
    let z_name = trig.z_name.clone().unwrap_or_default();
    let i_db = trig.i_schema;
    trig.step_list = step_list;
    if trig.step_list.len() as i32 > p.db.a_limit[SQLITE_LIMIT_TRIGGER_STEPS as usize] {
        p.error_msg(format!("trigger \"{z_name}\" contains too many steps"));
        return;
    }
    p.init_az[1].clone_from(&z_name);
    let steps = trig.step_list.clone();
    if fix_trigger_steps(p, &steps, i_db) {
        return;
    }
    let mut w = fixer(i_db);
    if sqlite3_walk_expr(p, &mut w, trig.p_when) != 0 {
        return;
    }
    if !p.db.init_busy {
        p.not_supported("statement kind (grammar rule not ported)");
        return;
    }
    let same_schema = trig.i_schema == trig.i_tab_schema;
    let table = trig.table.clone();
    let tab_schema = trig.i_tab_schema;
    let id = p.new_trigger(trig);
    p.a_schema[i_db as usize].trig_hash.insert(&z_name, id);
    if same_schema {
        let key = table.to_ascii_lowercase();
        if let Some(&tab) = p.a_schema.get(tab_schema as usize).and_then(|s| s.tbl_hash.get(&key)) {
            p.tabm(tab).p_trigger.insert(0, id);
        }
    }
}

/// `triggerSpanDup(db, zStart, zEnd)`: the text of a trigger step with every space
/// character turned into a plain space.
fn trigger_span_dup(p: &Parse, z_start: usize, z_end: usize) -> String {
    let z = crate::build_tab::span_dup_pub(p, z_start, z_end);
    z.chars().map(|c| if matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r' | '\x0b') { ' ' } else { c }).collect()
}

/// `sqlite3TriggerSelectStep(db, pSelect, zStart, zEnd)`: turn a SELECT statement (that the
/// pSelect argument points to) into a trigger step.
pub fn sqlite3_trigger_select_step(p: &mut Parse, select: SelectId, z_start: usize, z_end: usize) -> TriggerStep {
    TriggerStep { op: TK_SELECT, p_select: select, orconf: OE_Default as u8, z_span: Some(trigger_span_dup(p, z_start, z_end)), ..TriggerStep::default() }
}

/// `triggerStepAllocate(pParse, op, pTabList, zStart, zEnd)`: allocate space to hold a new
/// trigger step.
fn trigger_step_allocate(p: &mut Parse, op: u8, tab_list: SrcListId, z_start: usize, z_end: usize) -> Option<TriggerStep> {
    if p.n_err != 0 {
        return None;
    }
    let qualified = tab_list.nn() && p.src(tab_list).a[0].z_database.is_some();
    if let Some(t) = &p.new_trigger {
        if t.i_schema != 1 && qualified {
            p.error_msg("qualified table names are not allowed on INSERT, UPDATE, and DELETE statements within triggers");
            return None;
        }
    }
    let src = sqlite3_src_list_dup(p, tab_list, EXPRDUP_REDUCE);
    Some(TriggerStep { p_src: src, op, z_span: Some(trigger_span_dup(p, z_start, z_end)), ..TriggerStep::default() })
}

/// `sqlite3TriggerInsertStep(pParse, pTabList, pColumn, pSelect, orconf, pUpsert, zStart,
/// zEnd)`: build a trigger step out of an INSERT statement.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_trigger_insert_step(
    p: &mut Parse,
    tab_list: SrcListId,
    column: IdListId,
    select: SelectId,
    orconf: u8,
    upsert: UpsertId,
    z_start: usize,
    z_end: usize,
) -> Option<TriggerStep> {
    let mut st = trigger_step_allocate(p, TK_INSERT, tab_list, z_start, z_end)?;
    st.p_select = sqlite3_select_dup(p, select, EXPRDUP_REDUCE);
    st.p_id_list = column;
    st.p_upsert = upsert;
    st.orconf = orconf;
    if upsert.nn() {
        let t = p.up(upsert).p_upsert_target;
        crate::build_tab::sqlite3_has_explicit_nulls(p, t);
    }
    Some(st)
}

/// `sqlite3TriggerUpdateStep(pParse, pTabList, pFrom, pEList, pWhere, orconf, zStart,
/// zEnd)`: construct a trigger step that implements an UPDATE statement.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_trigger_update_step(
    p: &mut Parse,
    tab_list: SrcListId,
    from: SrcListId,
    e_list: ExprListId,
    where_: ExprId,
    orconf: u8,
    z_start: usize,
    z_end: usize,
) -> Option<TriggerStep> {
    let mut st = trigger_step_allocate(p, TK_UPDATE, tab_list, z_start, z_end)?;
    st.p_expr_list = sqlite3_expr_list_dup(p, e_list, EXPRDUP_REDUCE);
    st.p_where = sqlite3_expr_dup(p, where_, EXPRDUP_REDUCE);
    let mut from_dup = sqlite3_src_list_dup(p, from, EXPRDUP_REDUCE);
    st.orconf = orconf;
    if from_dup.nn() {
        let sub = crate::select::sqlite3_select_new(
            p,
            ExprListId::NULL,
            from_dup,
            ExprId::NULL,
            ExprListId::NULL,
            ExprId::NULL,
            ExprListId::NULL,
            SF_NestedFrom,
            ExprId::NULL,
        );
        let as_ = Token { z: Token::NO_Z, n: 0 };
        from_dup = crate::build::sqlite3_src_list_append_from_term(p, SrcListId::NULL, None, None, &as_, sub, None);
    }
    if from_dup.nn() && st.p_src.nn() {
        st.p_src = crate::build::sqlite3_src_list_append_list(p, st.p_src, from_dup);
    }
    Some(st)
}

/// `sqlite3TriggerDeleteStep(pParse, pTabList, pWhere, zStart, zEnd)`: construct a trigger
/// step that implements a DELETE statement.
pub fn sqlite3_trigger_delete_step(p: &mut Parse, tab_list: SrcListId, where_: ExprId, z_start: usize, z_end: usize) -> Option<TriggerStep> {
    let mut st = trigger_step_allocate(p, TK_DELETE, tab_list, z_start, z_end)?;
    st.p_where = sqlite3_expr_dup(p, where_, EXPRDUP_REDUCE);
    st.orconf = OE_Default as u8;
    Some(st)
}

// ---------------------------------------------------------------------------------------------
// Trigger sub-programs
// ---------------------------------------------------------------------------------------------

/// A trigger sub-program of the statement (`TriggerPrg.pProgram`): built while the trigger
/// is coded, finished by [`finish_sub_programs`].
#[derive(Debug, Clone, Default)]
pub struct SubProgSlot {
    /// The program, once coded (its `P4_SUBPROGRAM` operands still name slots, see
    /// [`sub_ref`])
    pub prog: Option<SubProgram>,
    /// The token (`pTrigger`)
    pub token: u64,
    /// Position in completion order (programs complete before the ones that call them,
    /// except recursive calls)
    pub done_seq: usize,
}

/// A `P4_SUBPROGRAM` operand naming sub-program slot `slot` while the statement is coded
/// (`n_mem == -2` marks it; [`finish_sub_programs`] replaces it).
fn sub_ref(slot: usize, token: u64) -> P4 {
    P4::SubProgram(Arc::new(SubProgram { a_op: Vec::new(), n_mem: -2, n_csr: slot as i32, n_once: 0, token }))
}

/// The fields of `Parse` that a sub-parse (`sSubParse` of `codeRowTrigger()`) has of its own:
/// they are swapped with fresh values while a trigger program is coded.
#[derive(Default)]
pub struct SubParseState {
    v: sqliters_vdbe::vdbeaux::Vdbe,
    has_vdbe: bool,
    n_mem: i32,
    n_tab: i32,
    n_temp_reg: u8,
    a_temp_reg: [i32; 8],
    n_range_reg: i32,
    i_range_reg: i32,
    p_const_expr: ExprListId,
    ok_const_factor: bool,
    col_names_set: bool,
    p_trigger_tab: TableId,
    e_trigger_op: u8,
    e_orconf: u8,
    oldmask: u32,
    newmask: u32,
    i_self_tab: i32,
    p_idx_epr: Vec<IndexedExpr>,
    p_idx_part_expr: Vec<IndexedExpr>,
    n_height: i32,
    n_nest_sel: i32,
    addr_explain: i32,
    explain: u8,
    explain_dummy: i32,
    within_rj_subrtn: u8,
    m_subrtn_sig: u8,
    a_subrtn_sig_addr: Vec<i32>,
    b_has_exists: bool,
    b_has_with: bool,
    has_compound: bool,
    p_with: WithId,
    b_returning: bool,
    is_nested_trigger: bool,
    nested: u8,
    n_select: i32,
}

fn swap_sub_parse(p: &mut Parse, s: &mut SubParseState) {
    // expmask bits recorded for the program being coded belong to it
    p.v.expmask |= p.pending_expmask.take();
    std::mem::swap(&mut p.v, &mut s.v);
    std::mem::swap(&mut p.has_vdbe, &mut s.has_vdbe);
    std::mem::swap(&mut p.n_mem, &mut s.n_mem);
    std::mem::swap(&mut p.n_tab, &mut s.n_tab);
    std::mem::swap(&mut p.n_temp_reg, &mut s.n_temp_reg);
    std::mem::swap(&mut p.a_temp_reg, &mut s.a_temp_reg);
    std::mem::swap(&mut p.n_range_reg, &mut s.n_range_reg);
    std::mem::swap(&mut p.i_range_reg, &mut s.i_range_reg);
    std::mem::swap(&mut p.p_const_expr, &mut s.p_const_expr);
    std::mem::swap(&mut p.ok_const_factor, &mut s.ok_const_factor);
    std::mem::swap(&mut p.col_names_set, &mut s.col_names_set);
    std::mem::swap(&mut p.p_trigger_tab, &mut s.p_trigger_tab);
    std::mem::swap(&mut p.e_trigger_op, &mut s.e_trigger_op);
    std::mem::swap(&mut p.e_orconf, &mut s.e_orconf);
    std::mem::swap(&mut p.oldmask, &mut s.oldmask);
    std::mem::swap(&mut p.newmask, &mut s.newmask);
    std::mem::swap(&mut p.i_self_tab, &mut s.i_self_tab);
    std::mem::swap(&mut p.p_idx_epr, &mut s.p_idx_epr);
    std::mem::swap(&mut p.p_idx_part_expr, &mut s.p_idx_part_expr);
    std::mem::swap(&mut p.n_height, &mut s.n_height);
    std::mem::swap(&mut p.n_nest_sel, &mut s.n_nest_sel);
    std::mem::swap(&mut p.addr_explain, &mut s.addr_explain);
    std::mem::swap(&mut p.explain, &mut s.explain);
    std::mem::swap(&mut p.explain_dummy, &mut s.explain_dummy);
    std::mem::swap(&mut p.within_rj_subrtn, &mut s.within_rj_subrtn);
    std::mem::swap(&mut p.m_subrtn_sig, &mut s.m_subrtn_sig);
    std::mem::swap(&mut p.a_subrtn_sig_addr, &mut s.a_subrtn_sig_addr);
    std::mem::swap(&mut p.b_has_exists, &mut s.b_has_exists);
    std::mem::swap(&mut p.b_has_with, &mut s.b_has_with);
    std::mem::swap(&mut p.has_compound, &mut s.has_compound);
    std::mem::swap(&mut p.p_with, &mut s.p_with);
    std::mem::swap(&mut p.b_returning, &mut s.b_returning);
    std::mem::swap(&mut p.is_nested_trigger, &mut s.is_nested_trigger);
    std::mem::swap(&mut p.nested, &mut s.nested);
    std::mem::swap(&mut p.n_select, &mut s.n_select);
}

/// `codeTriggerProgram(pParse, pStepList, orconf)`: generate VDBE code for the statements
/// inside the body of a trigger program.
fn code_trigger_program(p: &mut Parse, steps: &[TriggerStep], orconf: i32) {
    for st in steps {
        // Figure out the ON CONFLICT policy that will be used for this step of the trigger
        // program. If the statement that caused this trigger to fire had an explicit ON
        // CONFLICT, then use it. Otherwise, use the ON CONFLICT policy that was specified as
        // part of the trigger step.
        p.e_orconf = if orconf == OE_Default { st.orconf } else { orconf as u8 };
        if let Some(span) = &st.z_span {
            p.v.add_op4(OP_Trace, 0x7fff_ffff, 1, 0, P4::dynamic(&format!("-- {span}")));
        }
        match st.op {
            TK_UPDATE => {
                let src = sqlite3_src_list_dup(p, st.p_src, 0);
                let el = sqlite3_expr_list_dup(p, st.p_expr_list, 0);
                let w = sqlite3_expr_dup(p, st.p_where, 0);
                let oc = p.e_orconf as i32;
                crate::update::sqlite3_update(p, src, el, w, oc, ExprListId::NULL, ExprId::NULL, UpsertId::NULL);
                p.v.add_op0(OP_ResetCount);
            }
            TK_INSERT => {
                let src = sqlite3_src_list_dup(p, st.p_src, 0);
                let sel = sqlite3_select_dup(p, st.p_select, 0);
                let ids = if st.p_id_list.nn() {
                    let a = p.a_idlist[st.p_id_list.ix()].clone();
                    p.a_idlist.push(a);
                    IdListId(p.a_idlist.len() as u32 - 1)
                } else {
                    IdListId::NULL
                };
                let up = crate::upsert::sqlite3_upsert_dup(p, st.p_upsert);
                let oc = p.e_orconf as i32;
                crate::insert::sqlite3_insert(p, src, sel, ids, oc, up);
                p.v.add_op0(OP_ResetCount);
            }
            TK_DELETE => {
                let src = sqlite3_src_list_dup(p, st.p_src, 0);
                let w = sqlite3_expr_dup(p, st.p_where, 0);
                crate::delete::sqlite3_delete_from(p, src, w, ExprListId::NULL, ExprId::NULL);
                p.v.add_op0(OP_ResetCount);
            }
            _ => {
                let sel = sqlite3_select_dup(p, st.p_select, 0);
                let mut dest = crate::select::sqlite3_select_dest_init(SRT_Discard, 0);
                crate::select::sqlite3_select(p, sel, &mut dest);
            }
        }
    }
}

/// `codeRowTrigger(pParse, pTrigger, pTab, orconf)`: create and populate a new TriggerPrg
/// object with a sub-program implementing trigger pTrigger with ON CONFLICT policy orconf.
/// Returns the index of the TriggerPrg in `p_trigger_prg`.
fn code_row_trigger(p: &mut Parse, trigger: TriggerId, tab: TableId, orconf: i32) -> Option<usize> {
    if p.trigger_depth >= p.db.a_limit[SQLITE_LIMIT_TRIGGER_DEPTH as usize] {
        p.error_msg("triggers nested too deep");
        return None;
    }

    // Allocate the TriggerPrg and SubProgram objects. To ensure that they are freed if an
    // error occurs, add them to the list of TriggerPrg objects in the root Parse.
    let slot = p.sub_programs.len();
    p.sub_programs.push(SubProgSlot { prog: None, token: trigger.0 as u64, done_seq: usize::MAX });
    p.p_trigger_prg.push(TriggerPrg { p_trigger: trigger, orconf, i_program: slot, a_colmask: [0xffff_ffff, 0xffff_ffff] });
    let k = p.p_trigger_prg.len() - 1;

    // Allocate and populate a new Parse context to use for coding the trigger sub-program.
    let mut st = SubParseState { is_nested_trigger: true, ..SubParseState::default() };
    swap_sub_parse(p, &mut st);
    // (the state of the calling Parse is kept on a stack: autoIncBegin() reaches the
    // top-level Parse's nMem through it)
    p.sub_parse_stack.push(st);
    p.trigger_depth += 1;
    p.p_trigger_tab = tab;
    p.e_trigger_op = p.trig(trigger).op;
    p.oldmask = 0;
    p.newmask = 0;

    crate::select::sqlite3_get_vdbe(p);
    // (VdbeComment "Start: ...")
    if let Some(name) = p.trig(trigger).z_name.clone() {
        p.v.change_p4(-1, P4::dynamic(&format!("-- TRIGGER {name}")));
    }

    // If one was specified, code the WHEN clause. If it evaluates to false (or NULL) the
    // sub-vdbe is immediately halted by jumping to the OP_Halt inserted at the end of the
    // sub-vdbe.
    let mut i_end_trigger = 0;
    let when0 = p.trig(trigger).p_when;
    if when0.nn() {
        let when = sqlite3_expr_dup(p, when0, 0);
        let nc = p.new_nc(NameContext::default());
        if crate::resolve::sqlite3_resolve_expr_names(p, nc, when) == SQLITE_OK {
            i_end_trigger = p.v.make_label();
            crate::exprcode::sqlite3_expr_if_false(p, when, i_end_trigger, SQLITE_JUMPIFNULL);
        }
    }

    // Code the trigger program into the sub-vdbe.
    let steps = p.trig(trigger).step_list.clone();
    code_trigger_program(p, &steps, orconf);

    // Insert an OP_Halt at the end of the sub-program.
    if i_end_trigger != 0 {
        p.v.resolve_label(i_end_trigger);
    }
    p.v.add_op0(OP_Halt);

    let mut n_max_arg = 0;
    let ops = if p.n_err == 0 { Some(p.v.take_op_array(&mut n_max_arg)) } else { None };
    let (n_mem, n_csr, oldmask, newmask) = (p.n_mem, p.n_tab, p.oldmask, p.newmask);
    p.trigger_depth -= 1;
    let mut st = p.sub_parse_stack.pop().expect("sub-parse state");
    swap_sub_parse(p, &mut st);
    if n_max_arg > p.n_max_arg {
        p.n_max_arg = n_max_arg;
    }
    let done = p.sub_programs.iter().filter(|s| s.done_seq != usize::MAX).count();
    let token = trigger.0 as u64;
    let a_op = ops.unwrap_or_default();
    let n_once = a_op.iter().filter(|o| o.opcode == OP_Once).count() as i32;
    p.sub_programs[slot] = SubProgSlot { prog: Some(SubProgram { a_op, n_mem, n_csr, n_once, token }), token, done_seq: done };
    p.p_trigger_prg[k].a_colmask = [oldmask, newmask];
    Some(k)
}

/// `getRowTrigger(pParse, pTrigger, pTab, orconf)`: return a pointer to a TriggerPrg object
/// containing a sub-program implementing trigger pTrigger with ON CONFLICT policy orconf.
/// If no such TriggerPrg object exists, a new object is allocated and populated before
/// being returned.
fn get_row_trigger(p: &mut Parse, trigger: TriggerId, tab: TableId, orconf: i32) -> Option<usize> {
    // It may be that this trigger has already been coded (or is in the process of being
    // coded). If this is the case, return a pointer to the existing TriggerPrg structure.
    if let Some(k) = p.p_trigger_prg.iter().rposition(|x| x.p_trigger == trigger && x.orconf == orconf) {
        return Some(k);
    }
    // If an existing TriggerPrg could not be located, create a new one.
    let r = code_row_trigger(p, trigger, tab, orconf);
    p.err_byte_offset = -1;
    r
}

/// `sqlite3CodeRowTriggerDirect(pParse, p, pTab, reg, orconf, ignoreJump)`: generate code
/// for the trigger program associated with trigger p on table pTab. The reg, orconf and
/// ignoreJump parameters passed to this function are the same as those described in the
/// header function for sqlite3CodeRowTrigger().
pub fn sqlite3_code_row_trigger_direct(p: &mut Parse, t: TriggerId, tab: TableId, reg: i32, orconf: i32, ignore_jump: i32) {
    crate::select::sqlite3_get_vdbe(p);
    let Some(k) = get_row_trigger(p, t, tab, orconf) else { return };
    let b_recursive = p.trig(t).z_name.is_some() && (p.db.flags & SQLITE_RecTriggers) == 0;
    let slot = p.p_trigger_prg[k].i_program;
    let token = p.sub_programs[slot].token;
    p.n_mem += 1;
    let m = p.n_mem;
    p.v.add_op4(OP_Program, reg, ignore_jump, m, sub_ref(slot, token));
    // Set the P5 operand of the OP_Program instruction to non-zero if recursive invocation
    // of this trigger program should be disallowed. Recursive invocation is disallowed if
    // (a) the sub-program is really a trigger, not a foreign key action, and (b) the flag
    // to enable recursive triggers is clear.
    p.v.change_p5(b_recursive as u16);
}

/// The final `P4_SUBPROGRAM` operands and `pProgram` list of a program whose code is
/// complete: every slot reference becomes the sub-program object (one `Arc` per program),
/// or a stub ([`SubProgram::is_stub`]) when the program was still being coded where the
/// reference was made (a recursive trigger: C points at the same object).
pub fn finish_sub_programs(p: &mut Parse) {
    if p.sub_programs.is_empty() {
        return;
    }
    let n = p.sub_programs.len();
    // C's pProgram list: most recently linked first
    let final_index = |slot: usize| (n - 1 - slot) as i32;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&s| p.sub_programs[s].done_seq);
    let mut arcs: Vec<Option<Arc<SubProgram>>> = vec![None; n];
    let resolve = |ops: &mut Vec<sqliters_vdbe::vdbeaux::VdbeOp>, arcs: &Vec<Option<Arc<SubProgram>>>, slots: &Vec<SubProgSlot>| {
        for op in ops.iter_mut() {
            if let P4::SubProgram(sp) = &op.p4 {
                if sp.n_mem == -2 {
                    let s = sp.n_csr as usize;
                    op.p4 = match &arcs[s] {
                        Some(a) => P4::SubProgram(a.clone()),
                        None => P4::SubProgram(Arc::new(SubProgram {
                            a_op: Vec::new(),
                            n_mem: -1,
                            n_csr: final_index(s),
                            n_once: 0,
                            token: slots[s].token,
                        })),
                    };
                }
            }
        }
    };
    for &s in &order {
        let mut prog = p.sub_programs[s].prog.clone().unwrap_or_default();
        resolve(&mut prog.a_op, &arcs, &p.sub_programs);
        arcs[s] = Some(Arc::new(prog));
    }
    let mut main_ops = std::mem::take(&mut p.v.a_op);
    resolve(&mut main_ops, &arcs, &p.sub_programs);
    p.v.a_op = main_ops;
    p.v.p_program = (0..n).rev().map(|s| arcs[s].clone().expect("program")).collect();
}

/// `pToplevel->nMem`: the register counter of the top-level Parse.
pub fn toplevel_n_mem<'b>(p: &'b mut Parse<'_>) -> &'b mut i32 {
    if p.sub_parse_stack.is_empty() {
        &mut p.n_mem
    } else {
        &mut p.sub_parse_stack[0].n_mem
    }
}
