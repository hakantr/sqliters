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
