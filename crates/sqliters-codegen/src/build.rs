//! `build.c`: the statement shell of the code generator (`sqlite3FinishCoding`,
//! `sqlite3CodeVerifySchema`, `sqlite3BeginWriteOperation`, `sqlite3MayAbort`) and the
//! transaction-control statements (BEGIN, COMMIT, ROLLBACK, SAVEPOINT, RELEASE).

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{MakeReady, P4, Program};

use crate::consts::*;
use crate::exprcode::sqlite3_expr_code;
use crate::select::sqlite3_get_vdbe;
use crate::sqliteint::*;
use crate::tk::*;
use crate::util::sqlite3_dequote;

/// `sqlite3FinishCoding(pParse)`: this routine is called after a single SQL statement has
/// been parsed and a VDBE program to execute that statement has been prepared. This routine
/// puts the finishing touches on the VDBE program and resets the pParse structure for the
/// next parse.
///
/// Note that if an error occurred, it might be the case that no VDBE code was generated.
pub fn sqlite3_finish_coding(p: &mut Parse) {
    if p.nested != 0 {
        return;
    }
    if p.n_err != 0 {
        return;
    }
    // Begin by generating some termination code at the end of the vdbe program
    if !p.has_vdbe {
        if p.db.init_busy {
            p.rc = SQLITE_DONE;
            return;
        }
        sqlite3_get_vdbe(p);
    }
    // (pParse->bReturning: RETURNING is not ported in step V1)
    p.v.add_op0(OP_Halt);

    // The cookie mask contains one bit for each database file open. (Bit 0 is for main, bit 1
    // is for temp, and so forth.) Bits are set for each database that is used. Generate code
    // to start a transaction on each used database and to verify the schema cookie on each
    // used database.
    p.v.jump_here(0);
    for i_db in 0..p.db.a_db.len() {
        if (p.cookie_mask & (1u32 << i_db)) == 0 {
            continue;
        }
        let sharable = p.db.a_db[i_db].sharable;
        p.v.uses_btree(i_db as i32, sharable);
        let (cookie, generation) = (p.db.a_db[i_db].schema_cookie, p.db.a_db[i_db].i_generation);
        p.v.add_op4_int(
            OP_Transaction,                              // Opcode
            i_db as i32,                                 // P1
            ((p.write_mask & (1u32 << i_db)) != 0) as i32, // P2
            cookie,                                      // P3
            generation,                                  // P4
        );
        if !p.db.init_busy {
            p.v.change_p5(1);
        }
    }
    // (pParse->nVtabLock / OP_VBegin, codeTableLocks, sqlite3AutoincrementBegin: virtual
    // tables, shared-cache table locks and AUTOINCREMENT belong to later steps)

    // Code constant expressions that were factored out of inner loops.
    if p.p_const_expr.nn() {
        let el = p.p_const_expr;
        p.ok_const_factor = false;
        let mut i = 0;
        while i < p.l(el).a.len() {
            let (e, reg) = {
                let it = &p.l(el).a[i];
                (it.p_expr, it.i_const_expr_reg)
            };
            sqlite3_expr_code(p, e, reg);
            i += 1;
        }
    }

    // Finally, jump back to the beginning of the executable code.
    p.v.goto(1);

    // Get the VDBE program ready for execution
    if p.n_err == 0 {
        p.rc = SQLITE_DONE;
    } else {
        p.rc = SQLITE_ERROR;
    }
}

/// The `sqlite3VdbeMakeReady(v, pParse)` call at the end of `sqlite3FinishCoding()`: take the
/// finished program out of the Parse.
pub fn sqlite3_vdbe_make_ready(p: &mut Parse) -> Program {
    let v = std::mem::take(&mut p.v);
    v.make_ready(MakeReady {
        n_var: p.n_var,
        n_mem: p.n_mem,
        n_tab: p.n_tab,
        n_max_arg: p.n_max_arg,
        is_multi_write: p.is_multi_write,
        may_abort: p.may_abort,
        explain: p.explain,
        p_vlist: std::mem::take(&mut p.p_v_list),
    })
}

/// `sqlite3BeginTransaction(pParse, type)`: generate VDBE code for a BEGIN statement.
pub fn sqlite3_begin_transaction(p: &mut Parse, ty: i32) {
    // (sqlite3AuthCheck(pParse, SQLITE_TRANSACTION, "BEGIN", 0, 0): the engine authorizes)
    sqlite3_get_vdbe(p);
    if ty != TK_DEFERRED as i32 {
        for i in 0..p.db.a_db.len() {
            let (has_bt, read_only, sharable) = {
                let d = &p.db.a_db[i];
                (d.has_bt, d.read_only, d.sharable)
            };
            let e_txn_type = if has_bt && read_only {
                0 // Read txn
            } else if ty == TK_EXCLUSIVE as i32 {
                2 // Exclusive txn
            } else {
                1 // Write txn
            };
            p.v.add_op2(OP_Transaction, i as i32, e_txn_type);
            p.v.uses_btree(i as i32, sharable);
        }
    }
    p.v.add_op0(OP_AutoCommit);
}

/// `sqlite3EndTransaction(pParse, eType)`: generate VDBE code for a COMMIT or ROLLBACK
/// statement. Code for ROLLBACK is generated if eType==TK_ROLLBACK. Otherwise code is
/// generated for a COMMIT.
pub fn sqlite3_end_transaction(p: &mut Parse, e_type: i32) {
    let is_rollback = e_type == TK_ROLLBACK as i32;
    // (sqlite3AuthCheck(pParse, SQLITE_TRANSACTION, isRollback ? "ROLLBACK" : "COMMIT", 0, 0))
    sqlite3_get_vdbe(p);
    p.v.add_op2(OP_AutoCommit, 1, is_rollback as i32);
}

/// `sqlite3Savepoint(pParse, op, pName)`: this function is called by the parser when it
/// parses a command to create, release or rollback an SQL savepoint.
pub fn sqlite3_savepoint(p: &mut Parse, op: i32, name: &Token) {
    // zName = sqlite3NameFromToken(pParse->db, pName)
    let z_name = sqlite3_dequote(p.tok_str(name));
    sqlite3_get_vdbe(p);
    // (sqlite3AuthCheck(pParse, SQLITE_SAVEPOINT, az[op], zName, 0): the engine authorizes)
    p.v.add_op4(OP_Savepoint, op, 0, 0, P4::dynamic(&z_name));
}

/// `sqlite3CodeVerifySchema(pParse, iDb)`: record the fact that the schema cookie will need
/// to be verified for database `i_db`. The code to actually verify the schema cookie is
/// coded only once at the end of the program by `sqlite3FinishCoding()`.
///
/// (`sqlite3OpenTempDatabase()` for the temp database: with `pParse->explain` set C does not
/// open it; for a normal prepare the engine opens the temp database when the program runs.)
pub fn sqlite3_code_verify_schema(p: &mut Parse, i_db: i32) {
    // sqlite3CodeVerifySchemaAtToplevel(sqlite3ParseToplevel(pParse), iDb)
    p.cookie_mask |= 1u32 << i_db;
}

/// `sqlite3BeginWriteOperation(pParse, setStatement, iDb)`: generate VDBE code that prepares
/// for doing an operation that might change the database.
///
/// This routine starts a new transaction if we are not already within a transaction. If we
/// are already within a transaction, then a checkpoint is set if the `set_statement`
/// parameter is true. A checkpoint should be set for operations that might fail (due to a
/// constraint) part of the way through and which will need to undo some writes without
/// having to rollback the whole transaction. For operations where all constraints can be
/// checked before any changes are made to the database, it is never necessary to undo a
/// write and the checkpoint should not be set.
pub fn sqlite3_begin_write_operation(p: &mut Parse, set_statement: bool, i_db: i32) {
    sqlite3_code_verify_schema(p, i_db);
    p.write_mask |= 1u32 << i_db;
    p.is_multi_write |= set_statement;
}

/// `sqlite3MultiWrite(pParse)`: indicate that the statement currently under construction
/// might write more than one entry (example: deleting one row then inserting another,
/// inserting multiple rows in a table, or inserting a row and index entries.) If an abort
/// occurs after some of these writes have completed, then it will be necessary to undo the
/// completed writes.
pub fn sqlite3_multi_write(p: &mut Parse) {
    p.is_multi_write = true;
}

/// `sqlite3MayAbort(pParse)`: the code generator calls this routine if is discovers that it
/// is possible to abort a statement prior to completion. In order to perform this abort
/// without corrupting the database, we need to make sure that the statement is protected by
/// a statement transaction.
///
/// Technically, we only need to set the mayAbort flag if the isMultiWrite flag was
/// previously set. There is a time dependency such that the abort must occur after the
/// multiwrite. This makes some statement transactions unnecessary but always correct.
pub fn sqlite3_may_abort(p: &mut Parse) {
    p.may_abort = true;
}

#[allow(dead_code)]
const _: i32 = SAVEPOINT_BEGIN;

/// `sqlite3NameFromToken(db, pName)`: given a token, return a string that consists of the
/// text of that token, dequoted. `None` for a NULL token.
pub fn sqlite3_name_from_token(p: &Parse, name: Option<&Token>) -> Option<String> {
    match name {
        Some(t) if t.z != Token::NO_Z => Some(sqlite3_dequote(p.tok_str(t))),
        _ => None,
    }
}

/// `sqlite3IdListAppend(pParse, pList, pToken)`: append a new element to the given IdList.
/// Create a new IdList if need be.
pub fn sqlite3_id_list_append(p: &mut Parse, list: IdListId, token: &Token) -> IdListId {
    let name = sqlite3_name_from_token(p, Some(token)).unwrap_or_default();
    sqlite3_id_list_append_str(p, list, name)
}

/// [`sqlite3_id_list_append`] with the name given as a string (`sqlite3IdListAppend(pParse,
/// pList, 0)` followed by the assignment of `zName`).
pub fn sqlite3_id_list_append_str(p: &mut Parse, list: IdListId, name: String) -> IdListId {
    let list = if list.is_null() {
        p.a_idlist.push(IdList::default());
        IdListId(p.a_idlist.len() as u32 - 1)
    } else {
        list
    };
    p.a_idlist[list.ix()].a.push(name);
    list
}

/// `sqlite3IdListIndex(pList, zName)`: return the index in pList of the identifier named
/// zId. Return -1 if not found.
pub fn sqlite3_id_list_index(p: &Parse, list: IdListId, z_name: &str) -> i32 {
    for (i, n) in p.a_idlist[list.ix()].a.iter().enumerate() {
        if n.eq_ignore_ascii_case(z_name) {
            return i as i32;
        }
    }
    -1
}

/// `sqlite3SrcListAppend(pParse, pList, pTable, pDatabase)`: append a new table name to the
/// given SrcList. Create a new SrcList if need be.
///
/// A SrcList is returned, or NULL if there is an OOM error or if the SrcList grows to large.
/// The returned SrcList might be the same as the SrcList that was input or it might be a new
/// one.
///
/// If pDatabase is not null, it means that the table has an optional database name prefix.
/// Like this: "database.table". The pDatabase points to the table name and the pTable points
/// to the database name. The SrcList.a[].zName field is filled with the table name which
/// might come from pTable (if pDatabase is NULL) or from pDatabase. SrcList.a[].zDatabase is
/// filled with the database name from pTable, or with NULL if no database is specified.
///
/// In other words, if call like this:
///
///         sqlite3SrcListAppend(D,A,B,0);
///
/// Then B is a table name and the database name is unspecified. If called like this:
///
///         sqlite3SrcListAppend(D,A,B,C);
///
/// Then C is the table name and B is the database name. If C is defined then so is B. In
/// other words, we never have a case where:
///
///         sqlite3SrcListAppend(D,A,0,C);
///
/// Both pTable and pDatabase are assumed to be quoted. They are dequoted before being added
/// to the SrcList.
pub fn sqlite3_src_list_append(p: &mut Parse, list: SrcListId, table: Option<&Token>, database: Option<&Token>) -> SrcListId {
    let list = if list.is_null() { p.new_src(SrcList::default()) } else { list };
    // sqlite3SrcListEnlarge(): "too many FROM clause terms, max: %d"
    if p.src(list).a.len() as i32 >= SQLITE_MAX_SRCLIST {
        p.error_msg(format!("too many FROM clause terms, max: {SQLITE_MAX_SRCLIST}"));
        return SrcListId::NULL;
    }
    let database = match database {
        Some(d) if d.z != Token::NO_Z => Some(d),
        _ => None,
    };
    let mut item = SrcItem { i_cursor: -1, ..SrcItem::default() };
    if let Some(d) = database {
        item.z_name = sqlite3_name_from_token(p, Some(d));
        item.z_database = sqlite3_name_from_token(p, table);
    } else {
        item.z_name = sqlite3_name_from_token(p, table);
        item.z_database = None;
    }
    p.srcm(list).a.push(item);
    list
}

/// `SQLITE_MAX_SRCLIST`: maximum size of a SrcList object
pub const SQLITE_MAX_SRCLIST: i32 = 200;

/// `sqlite3SrcItemAttachSubquery(pParse, pItem, pSelect, dupSelect)`: attach a Subquery
/// object to pItem->uv.pSubq. Set the pSelect value but leave all the other values
/// initialized to zero.
///
/// A copy of the Select object is made if dupSelect is true, and the SrcItem takes
/// responsibility for deleting the copy. If dupSelect is false, ownership of the Select
/// passes to the SrcItem. Either way, the SrcItem will take responsibility for deleting the
/// Select.
///
/// When dupSelect is zero, that means the Select might get deleted right away if there is an
/// error. The pSelect might also be deleted even if there is no error, if the subquery is
/// later flattened or otherwise optimized away.
pub fn sqlite3_src_item_attach_subquery(p: &mut Parse, src: SrcListId, i: usize, select: SelectId, dup_select: bool) -> bool {
    {
        let it = &mut p.srcm(src).a[i];
        if it.fixed_schema {
            it.i_fixed_db = 0;
            it.fixed_schema = false;
        } else if it.z_database.is_some() {
            it.z_database = None;
        }
    }
    let select = if dup_select { crate::expr::sqlite3_select_dup(p, select, 0) } else { select };
    if select.is_null() {
        return false;
    }
    let it = &mut p.srcm(src).a[i];
    it.is_subquery = true;
    it.p_subq = Some(Box::new(Subquery { p_select: select, ..Subquery::default() }));
    true
}

/// `sqlite3SrcListAppendFromTerm(pParse, p, pTable, pDatabase, pAlias, pSubquery,
/// pOnUsing)`: this routine is called by the parser to add a new term to the end of a growing
/// FROM clause. The "p" parameter is the part of the FROM clause that has already been
/// constructed. "p" is NULL if this is the first term of the FROM clause. pTable and
/// pDatabase are the name of the table and database named in the FROM clause term. pDatabase
/// is NULL if the database name qualifier is missing - the usual case. If the term has an
/// alias, then pAlias points to the alias token. If the term is a subquery, then pSubquery is
/// the SELECT statement that the subquery encodes. The pTable and pDatabase parameters are
/// NULL for subqueries. The pOnUsing parameter is the content of the ON and USING clauses.
///
/// Return a new SrcList which encodes is the FROM with the new term added.
pub fn sqlite3_src_list_append_from_term(
    p: &mut Parse,
    list: SrcListId,
    table: Option<&Token>,
    database: Option<&Token>,
    alias: &Token,
    subquery: SelectId,
    on_using: Option<&OnOrUsing>,
) -> SrcListId {
    if list.is_null()
        && let Some(ou) = on_using
        && (ou.p_on.nn() || ou.p_using.nn())
    {
        p.error_msg(format!("a JOIN clause is required before {}", if ou.p_on.nn() { "ON" } else { "USING" }));
        return SrcListId::NULL;
    }
    let list = sqlite3_src_list_append(p, list, table, database);
    if list.is_null() {
        return SrcListId::NULL;
    }
    let i = p.src(list).a.len() - 1;
    if alias.n != 0 {
        let a = sqlite3_name_from_token(p, Some(alias));
        p.srcm(list).a[i].z_alias = a;
    }
    if subquery.nn()
        && sqlite3_src_item_attach_subquery(p, list, i, subquery, false)
        && (p.s(subquery).sel_flags & SF_NestedFrom) != 0
    {
        p.srcm(list).a[i].is_nested_from = true;
    }
    let it = &mut p.srcm(list).a[i];
    match on_using {
        None => it.p_on = ExprId::NULL,
        Some(ou) if ou.p_using.nn() => {
            it.is_using = true;
            it.p_using = ou.p_using;
        }
        Some(ou) => it.p_on = ou.p_on,
    }
    list
}

/// `sqlite3SrcListIndexedBy(pParse, p, pIndexedBy)`: add an INDEXED BY or NOT INDEXED clause
/// to the most recently added element of the source-list passed as the second argument.
pub fn sqlite3_src_list_indexed_by(p: &mut Parse, list: SrcListId, indexed_by: &Token) {
    if list.nn() && indexed_by.n > 0 {
        if indexed_by.n == 1 && indexed_by.z == Token::NO_Z {
            // A "NOT INDEXED" clause was supplied. See parse.y construct "indexed_opt" for
            // details.
            p.srcm(list).a.last_mut().expect("item").not_indexed = true;
        } else {
            let name = sqlite3_name_from_token(p, Some(indexed_by));
            let it = p.srcm(list).a.last_mut().expect("item");
            it.z_indexed_by = name;
            it.is_indexed_by = true;
        }
    }
}

/// `sqlite3SrcListAppendList(pParse, p1, p2)`: append the contents of SrcList p2 to SrcList
/// p1 and return the resulting SrcList. Or, if an error occurs, return NULL. In all cases,
/// p1 and p2 are freed by this function.
pub fn sqlite3_src_list_append_list(p: &mut Parse, p1: SrcListId, p2: SrcListId) -> SrcListId {
    if p2.nn() {
        let items = std::mem::take(&mut p.srcm(p2).a);
        let jt = items.first().map_or(0, |i| i.jointype) & JT_LTORJ as u8;
        let l = &mut p.srcm(p1).a;
        l.extend(items);
        l[0].jointype |= jt;
    }
    p1
}

/// `sqlite3SrcListFuncArgs(pParse, p, pList)`: add the list of function arguments to the
/// SrcList entry for a table-valued function.
pub fn sqlite3_src_list_func_args(p: &mut Parse, list: SrcListId, args: ExprListId) {
    if list.nn() {
        let it = p.srcm(list).a.last_mut().expect("item");
        it.p_func_arg = args;
        it.is_tab_func = true;
    }
}

/// `sqlite3SrcListShiftJoinType(pParse, p)`: when building up a FROM clause in the parser,
/// the join operator is initially attached to the left operand. But the code generator
/// expects the join operator to be on the right operand. This routine shifts all join
/// operators from left to right for an entire FROM clause.
///
/// Example: Suppose the join is like this:
///
///           A natural cross join B
///
/// The operator is "natural cross join". The A and B operands are stored in p->a[0] and
/// p->a[1], respectively. The parser initially stores the operator with A. This routine
/// shifts that operator over to B.
///
/// Additional changes:
///
///   *   All tables to the left of the right-most RIGHT JOIN are tagged with JT_LTORJ (mnemonic:
///       Left Table Of Right Join) so that the code generator can easily tell that the table
///       is part of the left operand of at least one RIGHT JOIN.
pub fn sqlite3_src_list_shift_join_type(p: &mut Parse, list: SrcListId) {
    if list.nn() && p.src(list).a.len() > 1 {
        let a = &mut p.srcm(list).a;
        let mut i = a.len() - 1;
        let mut all_flags: u8 = 0;
        loop {
            a[i].jointype = a[i - 1].jointype;
            all_flags |= a[i].jointype;
            i -= 1;
            if i == 0 {
                break;
            }
        }
        a[0].jointype = 0;

        // All terms to the left of a RIGHT JOIN should be tagged with the JT_LTORJ flags
        if all_flags & JT_RIGHT as u8 != 0 {
            let mut i = a.len() as isize - 1;
            while i > 0 && (a[i as usize].jointype & JT_RIGHT as u8) == 0 {
                i -= 1;
            }
            i -= 1;
            loop {
                a[i as usize].jointype |= JT_LTORJ as u8;
                i -= 1;
                if i < 0 {
                    break;
                }
            }
        }
    }
}

/// `sqlite3VdbeExplain(pParse, bPush, zFmt, ...)` (vdbeaux.c): add a new OP_Explain opcode.
///
/// If the bPush flag is true, then make this opcode the parent for subsequent Explains until
/// sqlite3VdbeExplainPop() is called.
///
/// In a production build the opcode is only added for EXPLAIN QUERY PLAN (`explain==2`).
pub fn sqlite3_vdbe_explain(p: &mut Parse, b_push: bool, z_msg: String) -> i32 {
    let mut addr = 0;
    if p.explain == 2 {
        let i_this = p.v.current_addr();
        addr = p.v.add_op4(OP_Explain, i_this, p.addr_explain, 0, P4::dynamic(&z_msg));
        if b_push {
            p.addr_explain = i_this;
        }
    }
    addr
}

/// `sqlite3VdbeExplainPop(pParse)`: pop the EXPLAIN QUERY PLAN stack one level.
pub fn sqlite3_vdbe_explain_pop(p: &mut Parse) {
    p.addr_explain = sqlite3_vdbe_explain_parent(p);
}

/// `sqlite3VdbeExplainParent(pParse)`: return the address of the current EXPLAIN QUERY PLAN
/// baseline. 0 means "none".
pub fn sqlite3_vdbe_explain_parent(p: &Parse) -> i32 {
    if p.addr_explain == 0 {
        return 0;
    }
    p.v.get_op(p.addr_explain).p2
}
