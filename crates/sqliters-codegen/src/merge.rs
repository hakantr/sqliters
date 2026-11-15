//! `select.c`: compound SELECT statements evaluated by merging two co-routines
//! (`multiSelectByMerge`, `generateOutputSubroutine`, `multiSelectByMergeKeyInfo`,
//! `multiSelectCollSeq`). Used for every compound with an ORDER BY clause and, with a
//! synthesized `ORDER BY 1`, for UNION, INTERSECT and EXCEPT.

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{CollSeq, KeyInfo, P4};

use crate::callback::dflt_coll;
use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::resolve::sqlite3_resolve_order_group_by;
use crate::select::*;
use crate::sqliteint::*;
use crate::tk::*;

/// `multiSelectCollSeq(pParse, p, iCol)`: return the appropriate collating sequence for the
/// `i_col`-th column of the result set for the compound-select statement `sel`. Return NULL
/// if the column has no default collating sequence.
///
/// The collating sequence for the compound select is taken from the left-most term of the
/// select that has a collating sequence.
pub(crate) fn multi_select_coll_seq(p: &mut Parse, sel: SelectId, i_col: i32) -> Option<Arc<CollSeq>> {
    let prior = p.s(sel).p_prior;
    let mut ret = if prior.nn() { multi_select_coll_seq(p, prior, i_col) } else { None };
    // iCol must be less than p->pEList->nExpr. Otherwise an error would have been thrown
    // during the processing of the sqlite3_prepare().
    if ret.is_none() && i_col < p.l(p.s(sel).p_e_list).n_expr() {
        let e = p.l(p.s(sel).p_e_list).a[i_col as usize].p_expr;
        ret = sqlite3_expr_coll_seq(p, e);
    }
    ret
}

/// `multiSelectByMergeKeyInfo(pParse, p, nExtra)`: the ORDER BY clause of the select statement
/// `sel` (which is a compound) is used to build a KeyInfo for the merge: each term gets the
/// collating sequence of the corresponding result column (an explicit COLLATE operator is
/// added to the term).
pub(crate) fn multi_select_by_merge_key_info(p: &mut Parse, sel: SelectId, n_extra: i32) -> KeyInfo {
    let order_by = p.s(sel).p_order_by;
    let n_order_by = if order_by.nn() { p.l(order_by).n_expr() } else { 0 };
    let mut ret = KeyInfo::alloc(p.enc(), (n_order_by + n_extra) as usize, 1);
    for i in 0..n_order_by as usize {
        let (term, col, sort_flags) = {
            let it = &p.l(order_by).a[i];
            (it.p_expr, it.i_order_by_col as i32, it.sort_flags)
        };
        let coll;
        if p.e(term).flags & EP_Collate != 0 {
            coll = sqlite3_expr_coll_seq(p, term);
        } else {
            let c = match multi_select_coll_seq(p, sel, col - 1) {
                Some(c) => c,
                None => dflt_coll(p),
            };
            let name = c.name.clone();
            let ne = sqlite3_expr_add_collate_string(p, term, &name);
            p.lm(order_by).a[i].p_expr = ne;
            coll = Some(c);
        }
        ret.a_coll[i] = coll;
        ret.a_sort_flags[i] = sort_flags;
    }
    ret
}

/// `generateOutputSubroutine(pParse, p, pIn, pDest, regReturn, regPrev, pKeyInfo, iBreak)`:
/// code an output subroutine for a coroutine implementation of a SELECT statement.
///
/// The data to be output is contained in pIn->iSdst. There are pIn->nSdst columns to be
/// output. `dest` is where the output should be sent.
///
/// `reg_return` is the register holding the return address of the subroutine.
///
/// If `reg_prev>0` then it is the first register in a vector that records the previous
/// output. mem[regPrev] is a flag that is false if there has been no previous output. If
/// regPrev>0 then code is generated to suppress duplicates. `key_info` is used for comparing
/// keys.
///
/// If the LIMIT found in p->iLimit is reached, jump immediately to `i_break`.
#[allow(clippy::too_many_arguments)]
fn generate_output_subroutine(
    p: &mut Parse,
    sel: SelectId,
    p_in: &SelectDest,
    dest: &mut SelectDest,
    reg_return: i32,
    reg_prev: i32,
    key_info: Option<Arc<KeyInfo>>,
    i_break: i32,
) -> i32 {
    let addr = p.v.current_addr();
    let i_continue = p.v.make_label();

    // Suppress duplicates for UNION, EXCEPT, and INTERSECT
    if reg_prev != 0 {
        let addr1 = p.v.add_op1(OP_IfNot, reg_prev);
        let addr2 = p.v.add_op4(
            OP_Compare,
            p_in.i_sdst,
            reg_prev + 1,
            p_in.n_sdst,
            P4::KeyInfo(key_info.expect("key info for duplicate removal")),
        );
        p.v.add_op3(OP_Jump, addr2 + 2, i_continue, addr2 + 2);
        p.v.jump_here(addr1);
        p.v.add_op3(OP_Copy, p_in.i_sdst, reg_prev + 1, p_in.n_sdst - 1);
        p.v.add_op2(OP_Integer, 1, reg_prev);
    }

    // Suppress the first OFFSET entries if there is an OFFSET clause
    let off = p.s(sel).i_offset;
    if off > 0 {
        p.v.add_op3(OP_IfPos, off, i_continue, 1);
    }

    match dest.e_dest as i32 {
        // Store the result as data using a unique key.
        SRT_Fifo | SRT_DistFifo | SRT_Table | SRT_EphemTab => {
            let r1 = sqlite3_get_temp_reg(p);
            let r2 = sqlite3_get_temp_reg(p);
            let i_parm = dest.i_sd_parm;
            p.v.add_op3(OP_MakeRecord, p_in.i_sdst, p_in.n_sdst, r1);
            if dest.e_dest as i32 == SRT_DistFifo {
                // If the destination is DistFifo, then cursor (iParm+1) is open on an
                // ephemeral index. Add the current row to that index.
                p.v.add_op4_int(OP_IdxInsert, i_parm + 1, r1, p_in.i_sdst, p_in.n_sdst);
            }
            p.v.add_op2(OP_NewRowid, i_parm, r2);
            p.v.add_op3(OP_Insert, i_parm, r1, r2);
            p.v.change_p5(OPFLAG_APPEND as u16);
            sqlite3_release_temp_reg(p, r2);
            sqlite3_release_temp_reg(p, r1);
        }
        // If any row exist in the result set, record that fact and abort.
        SRT_Exists => {
            p.v.add_op2(OP_Integer, 1, dest.i_sd_parm);
            // The LIMIT clause will terminate the loop for us
        }
        // If we are creating a set for an "expr IN (SELECT ...)".
        SRT_Set => {
            let r1 = sqlite3_get_temp_reg(p);
            let mut aff = dest.z_aff_sdst.clone().unwrap_or_default();
            aff.truncate(p_in.n_sdst as usize);
            p.v.add_op4(OP_MakeRecord, p_in.i_sdst, p_in.n_sdst, r1, P4::Dynamic(aff));
            p.v.add_op4_int(OP_IdxInsert, dest.i_sd_parm, r1, p_in.i_sdst, p_in.n_sdst);
            if dest.i_sd_parm2 > 0 {
                p.v.add_op4_int(OP_FilterAdd, dest.i_sd_parm2, 0, p_in.i_sdst, p_in.n_sdst);
                crate::build::sqlite3_vdbe_explain(p, false, "CREATE BLOOM FILTER".to_owned());
            }
            sqlite3_release_temp_reg(p, r1);
        }
        // If this is a scalar select that is part of an expression, then store the results in
        // the appropriate memory cell and break out of the scan loop.
        SRT_Mem => {
            sqlite3_expr_code_move(p, p_in.i_sdst, dest.i_sd_parm, p_in.n_sdst);
            // The LIMIT clause will jump out of the loop for us
        }
        // The results are stored in a sequence of registers starting at pDest->iSdst. Then
        // the co-routine yields.
        SRT_Coroutine => {
            if dest.i_sdst == 0 {
                dest.i_sdst = sqlite3_get_temp_range(p, p_in.n_sdst);
                dest.n_sdst = p_in.n_sdst;
            }
            sqlite3_expr_code_move(p, p_in.i_sdst, dest.i_sdst, p_in.n_sdst);
            p.v.add_op1(OP_Yield, dest.i_sd_parm);
        }
        SRT_DistQueue | SRT_Queue => {
            let i_parm = dest.i_sd_parm;
            let so = dest.p_order_by;
            let n_key = p.l(so).n_expr();
            let r1 = sqlite3_get_temp_reg(p);
            let r2 = sqlite3_get_temp_range(p, n_key + 2);
            let r3 = r2 + n_key + 1;

            p.v.add_op3(OP_MakeRecord, p_in.i_sdst, p_in.n_sdst, r3);
            if dest.e_dest as i32 == SRT_DistQueue {
                p.v.add_op2(OP_IdxInsert, i_parm + 1, r3);
            }
            for ii in 0..n_key {
                let col = p.l(so).a[ii as usize].i_order_by_col as i32;
                p.v.add_op2(OP_SCopy, p_in.i_sdst + col - 1, r2 + ii);
            }
            p.v.add_op2(OP_Sequence, i_parm, r2 + n_key);
            p.v.add_op3(OP_MakeRecord, r2, n_key + 2, r1);
            p.v.add_op4_int(OP_IdxInsert, i_parm, r1, r2, n_key + 2);
            sqlite3_release_temp_reg(p, r1);
            sqlite3_release_temp_range(p, r2, n_key + 2);
        }
        // Ignore the output
        SRT_Discard => {}
        // If none of the above, then the result destination must be SRT_Output.
        _ => {
            p.v.add_op2(OP_ResultRow, p_in.i_sdst, p_in.n_sdst);
        }
    }

    // Jump to the end of the loop if the LIMIT is reached.
    let lim = p.s(sel).i_limit;
    if lim != 0 {
        p.v.add_op2(OP_DecrJumpZero, lim, i_break);
    }

    // Generate the subroutine return
    p.v.resolve_label(i_continue);
    p.v.add_op1(OP_Return, reg_return);
    addr
}

/// `multiSelectByMerge(pParse, p, pDest)`: alternative compound select code generator for
/// cases when there is an ORDER BY clause.
///
/// We assume a query of the following form:
///
///      <selectA>  <operator>  <selectB>  ORDER BY <orderbylist>
///
/// <operator> is one of UNION ALL, UNION, EXCEPT, or INTERSECT. The idea is to co-routine
/// <selectA> and <selectB>. When both are run in parallel, with each yielding results in the
/// order of the ORDER BY clause, the results are merged: seven subroutines (outA, outB, EofA,
/// EofB, AltB, AeqB, AgtB) implement the merge as described in the header comment of this
/// function in `select.c`.
pub fn multi_select_by_merge(p: &mut Parse, sel: SelectId, dest: &mut SelectDest) -> i32 {
    let label_end = p.v.make_label();
    let label_cmpr = p.v.make_label();

    // Patch up the ORDER BY clause
    let op = p.s(sel).op;
    let mut order_by = p.s(sel).p_order_by;
    let mut n_order_by = p.l(order_by).n_expr();

    // For operators other than UNION ALL we have to make sure that the ORDER BY clause
    // covers every term of the result set. Add terms to the ORDER BY clause as necessary.
    if op != TK_ALL {
        let n_e = p.l(p.s(sel).p_e_list).n_expr();
        for i in 1..=n_e {
            let found = p.l(order_by).a[..n_order_by as usize].iter().any(|it| it.i_order_by_col as i32 == i);
            if !found {
                let new = sqlite3_expr_int32(p, i);
                order_by = sqlite3_expr_list_append(p, order_by, new);
                p.sm(sel).p_order_by = order_by;
                p.lm(order_by).a[n_order_by as usize].i_order_by_col = i as u16;
                n_order_by += 1;
            }
        }
    }

    // Compute the comparison permutation and keyinfo that is used with the permutation used
    // to determine if the next row of results comes from selectA or selectB. Also add
    // explicit collations to the ORDER BY clause terms so that when the subqueries to the
    // right and the left are evaluated, they use the correct collation.
    let mut a_permute: Vec<u32> = Vec::with_capacity(n_order_by as usize);
    let mut b_keep = false;
    for i in 1..=n_order_by {
        let c = p.l(order_by).a[(i - 1) as usize].i_order_by_col as u32;
        a_permute.push(c.wrapping_sub(1));
        if c.wrapping_sub(1) != (i - 1) as u32 {
            b_keep = true;
        }
    }
    let a_permute = if b_keep { Some(a_permute) } else { None };
    let key_merge = multi_select_by_merge_key_info(p, sel, 1);

    // Allocate a range of temporary registers and the KeyInfo needed for the logic that
    // removes duplicate result rows when the operator is UNION, EXCEPT, or INTERSECT (but not
    // UNION ALL).
    let reg_prev;
    let mut key_dup: Option<Arc<KeyInfo>> = None;
    if op == TK_ALL {
        reg_prev = 0;
    } else {
        let n_expr = p.l(p.s(sel).p_e_list).n_expr();
        reg_prev = p.n_mem + 1;
        p.n_mem += n_expr + 1;
        p.v.add_op2(OP_Integer, 0, reg_prev);
        let mut k = KeyInfo::alloc(p.enc(), n_expr as usize, 1);
        for i in 0..n_expr {
            k.a_coll[i as usize] = multi_select_coll_seq(p, sel, i);
            k.a_sort_flags[i as usize] = 0;
        }
        key_dup = Some(Arc::new(k));
    }

    // Separate the left and the right query from one another
    let mut n_select = 1;
    let mut split;
    if (op == TK_ALL || op == TK_UNION) && p.db.optimization_enabled(SQLITE_BalancedMerge) {
        split = sel;
        while p.s(split).p_prior.nn() && p.s(split).op == op {
            n_select += 1;
            split = p.s(split).p_prior;
        }
    }
    if n_select <= 3 {
        split = sel;
    } else {
        split = sel;
        let mut i = 2;
        while i < n_select {
            split = p.s(split).p_prior;
            i += 2;
        }
    }
    let prior = p.s(split).p_prior;
    p.sm(split).p_prior = SelectId::NULL;
    p.sm(prior).p_next = SelectId::NULL;
    let dup = sqlite3_expr_list_dup(p, order_by, 0);
    p.sm(prior).p_order_by = dup;
    let ob = p.s(sel).p_order_by;
    sqlite3_resolve_order_group_by(p, sel, ob, "ORDER");
    let ob = p.s(prior).p_order_by;
    sqlite3_resolve_order_group_by(p, prior, ob, "ORDER");

    // Compute the limit registers
    compute_limit_registers(p, sel, label_end);
    let (reg_limit_a, reg_limit_b);
    if p.s(sel).i_limit != 0 && (op == TK_ALL || union_with_limit_one(p, sel)) {
        p.n_mem += 1;
        reg_limit_a = p.n_mem;
        p.n_mem += 1;
        reg_limit_b = p.n_mem;
        let src = if p.s(sel).i_offset != 0 { p.s(sel).i_offset + 1 } else { p.s(sel).i_limit };
        p.v.add_op2(OP_Copy, src, reg_limit_a);
        p.v.add_op2(OP_Copy, reg_limit_a, reg_limit_b);
    } else {
        reg_limit_a = 0;
        reg_limit_b = 0;
    }
    p.sm(sel).p_limit = ExprId::NULL;

    p.n_mem += 1;
    let reg_addr_a = p.n_mem;
    p.n_mem += 1;
    let reg_addr_b = p.n_mem;
    p.n_mem += 1;
    let reg_out_a = p.n_mem;
    p.n_mem += 1;
    let reg_out_b = p.n_mem;
    let mut dest_a = sqlite3_select_dest_init(SRT_Coroutine, reg_addr_a);
    let mut dest_b = sqlite3_select_dest_init(SRT_Coroutine, reg_addr_b);

    {
        let op_name = sqlite3_select_op_name(p.s(sel).op);
        crate::build::sqlite3_vdbe_explain(p, true, format!("MERGE ({op_name})"));
    }

    // Generate a coroutine to evaluate the SELECT statement to the left of the compound
    // operator - the "A" select.
    let addr_select_a = p.v.current_addr() + 1;
    let mut addr1 = p.v.add_op3(OP_InitCoroutine, reg_addr_a, 0, addr_select_a);
    p.sm(prior).i_limit = reg_limit_a;
    crate::build::sqlite3_vdbe_explain(p, true, "LEFT".to_owned());
    sqlite3_select(p, prior, &mut dest_a);
    p.v.end_coroutine(reg_addr_a);
    p.n_temp_reg = 0;
    p.n_range_reg = 0;
    p.v.jump_here(addr1);

    // Generate a coroutine to evaluate the SELECT statement on the right - the "B" select
    let addr_select_b = p.v.current_addr() + 1;
    addr1 = p.v.add_op3(OP_InitCoroutine, reg_addr_b, 0, addr_select_b);
    let saved_limit = p.s(sel).i_limit;
    let saved_offset = p.s(sel).i_offset;
    p.sm(sel).i_limit = reg_limit_b;
    p.sm(sel).i_offset = 0;
    crate::build::sqlite3_vdbe_explain(p, true, "RIGHT".to_owned());
    sqlite3_select(p, sel, &mut dest_b);
    p.sm(sel).i_limit = saved_limit;
    p.sm(sel).i_offset = saved_offset;
    p.v.end_coroutine(reg_addr_b);
    p.n_temp_reg = 0;
    p.n_range_reg = 0;

    // Generate a subroutine that outputs the current row of the A select as the next output
    // row of the compound select.
    let addr_out_a = generate_output_subroutine(p, sel, &dest_a, dest, reg_out_a, reg_prev, key_dup.clone(), label_end);

    // Generate a subroutine that outputs the current row of the B select as the next output
    // row of the compound select.
    let mut addr_out_b = 0;
    if op == TK_ALL || op == TK_UNION {
        addr_out_b = generate_output_subroutine(p, sel, &dest_b, dest, reg_out_b, reg_prev, key_dup.clone(), label_end);
    }

    // Generate a subroutine to run when the results from select A are exhausted and only
    // data in select B remains.
    let (addr_eof_a, addr_eof_a_no_b);
    if op == TK_EXCEPT || op == TK_INTERSECT {
        addr_eof_a = label_end;
        addr_eof_a_no_b = label_end;
    } else {
        addr_eof_a = p.v.add_op2(OP_Gosub, reg_out_b, addr_out_b);
        addr_eof_a_no_b = p.v.add_op2(OP_Yield, reg_addr_b, label_end);
        p.v.goto(addr_eof_a);
        let n = sqlite3_log_est_add(p.s(sel).n_select_row, p.s(prior).n_select_row);
        p.sm(sel).n_select_row = n;
    }

    // Generate a subroutine to run when the results from select B are exhausted and only
    // data in select A remains.
    let addr_eof_b;
    if op == TK_INTERSECT {
        addr_eof_b = addr_eof_a;
        if p.s(sel).n_select_row > p.s(prior).n_select_row {
            let n = p.s(prior).n_select_row;
            p.sm(sel).n_select_row = n;
        }
    } else {
        addr_eof_b = p.v.add_op2(OP_Gosub, reg_out_a, addr_out_a);
        p.v.add_op2(OP_Yield, reg_addr_a, label_end);
        p.v.goto(addr_eof_b);
    }

    // Generate code to handle the case of A<B
    let mut addr_alt_b = p.v.add_op2(OP_Gosub, reg_out_a, addr_out_a);
    p.v.add_op2(OP_Yield, reg_addr_a, addr_eof_a);
    p.v.goto(label_cmpr);

    // Generate code to handle the case of A==B
    let addr_aeq_b;
    if op == TK_ALL {
        addr_aeq_b = addr_alt_b;
    } else if op == TK_INTERSECT {
        addr_aeq_b = addr_alt_b;
        addr_alt_b += 1;
    } else {
        addr_aeq_b = addr_alt_b + 1;
    }

    // Generate code to handle the case of A>B
    let mut addr_agt_b = p.v.current_addr();
    if op == TK_ALL || op == TK_UNION {
        p.v.add_op2(OP_Gosub, reg_out_b, addr_out_b);
        p.v.add_op2(OP_Yield, reg_addr_b, addr_eof_b);
        p.v.goto(label_cmpr);
    } else {
        addr_agt_b += 1; // Just do next-B. Might as well use the next-B call in the next code block
    }

    // This code runs once to initialize everything.
    p.v.jump_here(addr1);
    p.v.add_op2(OP_Yield, reg_addr_a, addr_eof_a_no_b);
    p.v.add_op2(OP_Yield, reg_addr_b, addr_eof_b);

    // Implement the main merge loop
    let has_permute = a_permute.is_some();
    if let Some(perm) = a_permute {
        p.v.add_op4(OP_Permutation, 0, 0, 0, P4::IntArray(perm));
    }
    p.v.resolve_label(label_cmpr);
    p.v.add_op4(OP_Compare, dest_a.i_sdst, dest_b.i_sdst, n_order_by, P4::KeyInfo(Arc::new(key_merge)));
    if has_permute {
        p.v.change_p5(OPFLAG_PERMUTE as u16);
    }
    p.v.add_op3(OP_Jump, addr_alt_b, addr_aeq_b, addr_agt_b);

    // Jump to the this point in order to terminate the query.
    p.v.resolve_label(label_end);

    // Make arrangements to free the 2nd and subsequent arms of the compound after the parse
    // has finished
    p.sm(split).p_prior = prior;
    p.sm(prior).p_next = split;
    p.sm(prior).p_order_by = ExprListId::NULL;

    // TBD: Insert subroutine calls to close cursors on incomplete subqueries
    crate::build::sqlite3_vdbe_explain_pop(p);
    (p.n_err != 0) as i32
}
