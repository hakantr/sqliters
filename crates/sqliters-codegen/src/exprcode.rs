//! `expr.c`, second part: VDBE code generation for expressions (`sqlite3ExprCode`,
//! `sqlite3ExprCodeTarget`, `sqlite3ExprIfTrue` / `sqlite3ExprIfFalse`, the IN operator,
//! sub-selects, constant factoring).
//!
//! Functions appear in the order of `expr.c` and keep its names. The order of register,
//! cursor and label allocation is C's: all three are visible in `EXPLAIN`.

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{CollSeq, KeyInfo, P4, SubrtnSig};

use crate::build::sqlite3_may_abort;
use crate::callback::*;
use crate::consts::*;
use crate::expr::*;
use crate::select::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::util::*;

fn p4_coll(c: Option<Arc<CollSeq>>) -> P4 {
    // sqlite3VdbeAddOp4(..., (void*)p4, P4_COLLSEQ) with p4==0 leaves the operand unset
    match c {
        Some(c) => P4::CollSeq(c),
        None => P4::NotUsed,
    }
}

/// `codeCompare(pParse, pLeft, pRight, opcode, in1, in2, dest, jumpIfNull, isCommuted)`:
/// generate code for a comparison operator.
#[allow(clippy::too_many_arguments)]
fn code_compare(
    p: &mut Parse,
    left: ExprId,
    right: ExprId,
    opcode: u8,
    in1: i32,
    in2: i32,
    dest: i32,
    jump_if_null: i32,
    is_commuted: bool,
) -> i32 {
    if p.n_err != 0 {
        return 0;
    }
    let p4 = if is_commuted {
        sqlite3_binary_compare_coll_seq(p, right, left)
    } else {
        sqlite3_binary_compare_coll_seq(p, left, right)
    };
    let p5 = binary_compare_p5(p, left, right, jump_if_null);
    let addr = p.v.add_op4(opcode, in2, dest, in1, p4_coll(p4));
    p.v.change_p5(p5 as u16);
    addr
}

/// `exprCodeSubselect(pParse, pExpr)`: if the expression is a subquery, generate code to
/// evaluate it and return the first result register; 0 otherwise.
fn expr_code_subselect(p: &mut Parse, e: ExprId) -> i32 {
    if p.e(e).op == TK_SELECT { sqlite3_code_subselect(p, e) } else { 0 }
}

/// `exprVectorRegister(pParse, pVector, iField, regSelect, pTmp, ppExpr, pRegFree)`: the
/// register holding field `i_field` of vector `vector`, generating code for it if necessary.
/// Returns (register, the expression of the field, register to free).
fn expr_vector_register(p: &mut Parse, vector: ExprId, i_field: i32, reg_select: i32) -> (i32, ExprId, i32) {
    let op = p.e(vector).op;
    if op == TK_REGISTER {
        let e = sqlite3_vector_field_subexpr(p, vector, i_field);
        return (p.e(vector).i_table + i_field, e, 0);
    }
    if op == TK_SELECT {
        let n = p.l(p.s(p.e(vector).x_select()).p_e_list).n_expr();
        let tmp = p.new_expr(Expr {
            op: TK_SELECT_COLUMN,
            p_left: vector,
            i_column: i_field as i16,
            i_table: n,
            ..Expr::default()
        });
        return (reg_select + i_field, tmp, 0);
    }
    if op == TK_VECTOR {
        let e = p.l(p.e(vector).x_list()).a[i_field as usize].p_expr;
        let mut reg_free = 0;
        let r = sqlite3_expr_code_temp(p, e, &mut reg_free);
        return (r, e, reg_free);
    }
    (0, ExprId::NULL, 0)
}

/// `codeVectorCompare(pParse, pExpr, dest, op, p5)`: `e` is a vector comparison; generate code
/// that stores the result (1, 0 or NULL) in register `dest`.
fn code_vector_compare(p: &mut Parse, e: ExprId, dest: i32, op: u8, p5: u8) {
    let left = p.e(e).p_left;
    let right = p.e(e).p_right;
    let n_left = sqlite3_expr_vector_size(p, left);
    let mut opx = op;
    let mut addr_cmp = 0;
    let addr_done = p.v.make_label();
    let is_commuted = p.e(e).has(EP_Commuted);
    if p.n_err != 0 {
        return;
    }
    if n_left != sqlite3_expr_vector_size(p, right) {
        p.error_msg("row value misused");
        return;
    }
    if op == TK_LE {
        opx = TK_LT;
    }
    if op == TK_GE {
        opx = TK_GT;
    }
    if op == TK_NE {
        opx = TK_EQ;
    }
    let reg_left = expr_code_subselect(p, left);
    let reg_right = expr_code_subselect(p, right);
    p.v.add_op2(OP_Integer, 1, dest);
    let mut i = 0;
    loop {
        if addr_cmp != 0 {
            p.v.jump_here(addr_cmp);
        }
        let (r1, p_l, reg_free1) = expr_vector_register(p, left, i, reg_left);
        let (r2, p_r, reg_free2) = expr_vector_register(p, right, i, reg_right);
        addr_cmp = p.v.current_addr();
        code_compare(p, p_l, p_r, opx, r1, r2, addr_done, p5 as i32, is_commuted);
        sqlite3_release_temp_reg(p, reg_free1);
        sqlite3_release_temp_reg(p, reg_free2);
        if (opx == TK_LT || opx == TK_GT) && i < n_left - 1 {
            addr_cmp = p.v.add_op0(OP_ElseEq);
        }
        if p5 as i32 == SQLITE_NULLEQ {
            p.v.add_op2(OP_Integer, 0, dest);
        } else {
            p.v.add_op3(OP_ZeroOrNull, r1, dest, r2);
        }
        if i == n_left - 1 {
            break;
        }
        if opx == TK_EQ {
            p.v.add_op2(OP_NotNull, dest, addr_done);
        } else {
            p.v.add_op2(OP_Goto, 0, addr_done);
            if i == n_left - 2 {
                opx = op;
            }
        }
        i += 1;
    }
    p.v.jump_here(addr_cmp);
    p.v.resolve_label(addr_done);
    if op == TK_NE {
        p.v.add_op2(OP_Not, dest, dest);
    }
}

/// `exprEvalRhsFirst(pExpr)`: true if the right operand of a binary operator should be
/// evaluated before the left one (the left operand contains a subquery and the right does
/// not).
fn expr_eval_rhs_first(p: &Parse, e: ExprId) -> bool {
    p.e(p.e(e).p_left).has(EP_Subquery) && !p.e(p.e(e).p_right).has(EP_Subquery)
}

/// `exprComputeOperands(pParse, pExpr, &r1, &r2, &free1, &free2)`: compute the two operands of
/// a binary operator. If either operand contains a subquery, code is generated that skips the
/// subquery when the other operand is NULL. Returns (addrIsNull, r1, r2).
fn expr_compute_operands(p: &mut Parse, e: ExprId, free1: &mut i32, free2: &mut i32) -> (i32, i32, i32) {
    let mut addr_is_null;
    let mut r2;
    let (left, right) = (p.e(e).p_left, p.e(e).p_right);
    if expr_eval_rhs_first(p, e) && sqlite3_expr_can_be_null(p, right) {
        r2 = sqlite3_expr_code_temp(p, right, free2);
        addr_is_null = p.v.add_op1(OP_IsNull, r2);
    } else {
        r2 = 0;
        addr_is_null = 0;
    }
    let r1 = sqlite3_expr_code_temp(p, left, free1);
    if addr_is_null == 0 {
        // If the RHS is a subquery and the LHS can be NULL, skip the RHS when the LHS is NULL
        if p.e(right).has(EP_Subquery) && sqlite3_expr_can_be_null(p, left) {
            addr_is_null = p.v.add_op1(OP_IsNull, r1);
        }
        r2 = sqlite3_expr_code_temp(p, right, free2);
    }
    (addr_is_null, r1, r2)
}

/// `isCandidateForInOpt(pX)`: pX is the RHS of an IN operator. If pX is a SELECT statement
/// that can be simplified to a direct table access, then return a pointer to the SELECT
/// statement. If pX is not a SELECT statement, or if the SELECT statement needs to be
/// manifested into a transient table, then return NULL.
fn is_candidate_for_in_opt(p: &Parse, x: ExprId) -> SelectId {
    let ex = p.e(x);
    if !ex.use_x_select() {
        return SelectId::NULL; // Not a subquery
    }
    if ex.has(EP_VarSelect) {
        return SelectId::NULL; // Correlated subq
    }
    let sel = ex.x_select();
    let s = p.s(sel);
    if s.p_prior.nn() {
        return SelectId::NULL; // Not a compound SELECT
    }
    if s.sel_flags & SF_Aggregate != 0 {
        return SelectId::NULL; // No GROUP BY keyword or aggregate functions
    }
    if s.p_limit.nn() {
        return SelectId::NULL; // Has no LIMIT clause
    }
    if s.p_where.nn() {
        return SelectId::NULL; // Has no WHERE clause
    }
    let src = p.src(s.p_src);
    if src.a.len() != 1 {
        return SelectId::NULL; // Single term in FROM clause
    }
    if src.a[0].is_subquery {
        return SelectId::NULL; // FROM is not a subquery or view
    }
    let tab = src.a[0].p_s_tab;
    if p.tab(tab).is_virtual() {
        return SelectId::NULL; // FROM clause not a virtual table
    }
    // All SELECT results must be columns.
    for it in &p.l(s.p_e_list).a {
        if p.e(it.p_expr).op != TK_COLUMN {
            return SelectId::NULL;
        }
    }
    sel
}

/// `sqlite3SetHasNullFlag(v, iCur, regHasNull, eSortOrder)`: generate code that checks the
/// left-most column of index `i_cur` to see if it contains any NULL entries; register
/// `reg_has_null` is set to NULL if it does and to a non-NULL value otherwise.
fn sqlite3_set_has_null_flag(p: &mut Parse, i_cur: i32, reg_has_null: i32, e_sort_order: i32) {
    p.v.add_op2(OP_Integer, 0, reg_has_null);
    let op = if e_sort_order == SQLITE_SO_ASC { OP_Rewind } else { OP_Last };
    let addr1 = p.v.add_op1(op, i_cur);
    p.v.add_op3(OP_Column, i_cur, 0, reg_has_null);
    p.v.change_p5(OPFLAG_TYPEOFARG as u16);
    p.v.jump_here(addr1);
}

/// `sqlite3InRhsIsConstant(pParse, pIn)`: the argument is an IN operator with a list (not a
/// subquery) on the right-hand side; return TRUE if that list is constant.
fn sqlite3_in_rhs_is_constant(p: &mut Parse, p_in: ExprId) -> bool {
    let lhs = p.e(p_in).p_left;
    p.em(p_in).p_left = ExprId::NULL;
    let res = sqlite3_expr_is_constant(p, p_in);
    p.em(p_in).p_left = lhs;
    res != 0
}

/// `sqlite3FindInIndex(pParse, pX, inFlags, prRhsHasNull, aiMap, piTab)`: find (or create) the
/// b-tree that implements the right-hand side of an IN operator. Returns the `IN_INDEX_*`
/// type and stores the cursor number in `*pi_tab`.
pub fn sqlite3_find_in_index(
    p: &mut Parse,
    x: ExprId,
    in_flags: u32,
    mut pr_rhs_has_null: Option<&mut i32>,
    mut ai_map: Option<&mut Vec<i32>>,
    pi_tab: &mut i32,
) -> i32 {
    let mut e_type = 0;
    sqlite3_get_vdbe(p);
    let i_tab = p.n_tab;
    p.n_tab += 1;

    // If the RHS of this IN(...) operator is a SELECT, and if it matters whether or not the
    // SELECT result contains NULL values, check whether or not NULL values are actually
    // possible (may not be if there are NOT NULL constraints). If no NULL values are possible,
    // set prRhsHasNull to 0.
    if pr_rhs_has_null.is_some() && p.e(x).use_x_select() {
        let e_list = p.s(p.e(x).x_select()).p_e_list;
        let n = p.l(e_list).a.len();
        let mut i = 0;
        while i < n {
            if sqlite3_expr_can_be_null(p, p.l(e_list).a[i].p_expr) {
                break;
            }
            i += 1;
        }
        if i == n {
            pr_rhs_has_null = None;
        }
    }

    // Check to see if an existing table or index can be used to satisfy the query. This is
    // preferable to generating a new ephemeral table.
    let cand = if p.n_err == 0 { is_candidate_for_in_opt(p, x) } else { SelectId::NULL };
    if cand.nn() {
        let e_list = p.s(cand).p_e_list;
        let n_expr = p.l(e_list).n_expr();
        let tab = p.src(p.s(cand).p_src).a[0].p_s_tab;

        // Code an OP_Transaction and OP_TableLock for <table>.
        let i_db = p.tab(tab).i_db;
        crate::build::sqlite3_code_verify_schema(p, i_db);
        let (tnum, name) = (p.tab(tab).tnum, p.tab(tab).z_name.clone());
        crate::schema::sqlite3_table_lock(p, i_db, tnum, 0, &name);

        if n_expr == 1 && p.e(p.l(e_list).a[0].p_expr).i_column < 0 {
            // The "x IN (SELECT rowid FROM table)" case
            let i_addr = p.v.add_op0(OP_Once);
            crate::schema::sqlite3_open_table(p, i_tab, i_db, tab, OP_OpenRead);
            e_type = IN_INDEX_ROWID;
            crate::build::sqlite3_vdbe_explain(p, false, format!("USING ROWID SEARCH ON TABLE {name} FOR IN-OPERATOR"));
            p.v.jump_here(i_addr);
        } else {
            let mut affinity_ok = true;
            let left = p.e(x).p_left;

            // Check that the affinity that will be used to perform each comparison is the
            // same as the affinity of each column in table on the RHS of the IN operator. If
            // it not, it is not possible to use any index of the RHS table.
            let mut i = 0;
            while i < n_expr && affinity_ok {
                let lhs = sqlite3_vector_field_subexpr(p, left, i);
                let i_col = p.e(p.l(e_list).a[i as usize].p_expr).i_column as i32;
                let idxaff = sqlite3_table_column_affinity(p, tab, i_col); // RHS table
                let cmpaff = sqlite3_compare_affinity(p, lhs, idxaff);
                match cmpaff {
                    SQLITE_AFF_BLOB => {}
                    SQLITE_AFF_TEXT => {
                        // sqlite3CompareAffinity() only returns TEXT if one side or the other
                        // has no affinity and the other side is TEXT. Hence, the only way for
                        // cmpaff to be TEXT is for idxaff to be TEXT. So the next statement
                        // cannot be true.
                    }
                    _ => affinity_ok = idxaff >= SQLITE_AFF_NUMERIC,
                }
                i += 1;
            }

            if affinity_ok {
                let must_be_unique = (in_flags & IN_INDEX_LOOP as u32) != 0;
                // Search for an existing index that will work for this IN operator
                let list = p.tab(tab).p_index.clone();
                for idx in list {
                    if e_type != 0 {
                        break;
                    }
                    let (n_column, n_key_col, partial, unique) = {
                        let ix = p.idx(idx);
                        (ix.n_column as i32, ix.n_key_col as i32, ix.p_part_idx_where.nn(), ix.is_unique())
                    };
                    if n_column < n_expr {
                        continue;
                    }
                    if partial {
                        continue;
                    }
                    // Maximum nColumn is BMS-2, not BMS-1, so that we can compute BITMASK(nExpr)
                    // without overflowing
                    if n_column >= BMS - 1 {
                        continue;
                    }
                    if must_be_unique && (n_key_col > n_expr || (n_column > n_expr && !unique)) {
                        continue; // This index is not unique over the IN RHS columns
                    }

                    let mut col_used: Bitmask = 0; // Columns of index used so far
                    for i in 0..n_expr {
                        let lhs = sqlite3_vector_field_subexpr(p, left, i);
                        let rhs = p.l(e_list).a[i as usize].p_expr;
                        let req = sqlite3_binary_compare_coll_seq(p, lhs, rhs);
                        let rhs_col = p.e(rhs).i_column;
                        let mut j = 0;
                        while j < n_expr {
                            let ix = p.idx(idx);
                            if ix.ai_column[j as usize] != rhs_col {
                                j += 1;
                                continue;
                            }
                            if let Some(r) = &req
                                && !r.name.eq_ignore_ascii_case(&ix.az_coll[j as usize])
                            {
                                j += 1;
                                continue;
                            }
                            break;
                        }
                        if j == n_expr {
                            break;
                        }
                        let m_col: Bitmask = 1u64 << j;
                        if m_col & col_used != 0 {
                            break; // Each column used only once
                        }
                        col_used |= m_col;
                        if let Some(m) = ai_map.as_deref_mut() {
                            m[i as usize] = j;
                        }
                    }

                    if col_used == (1u64 << n_expr) - 1 {
                        // If we reach this point, that means the index pIdx is usable
                        let i_addr = p.v.add_op0(OP_Once);
                        let (idx_name, idx_tnum, sort0) = {
                            let ix = p.idx(idx);
                            (ix.z_name.clone(), ix.tnum, ix.a_sort_order[0] as i32)
                        };
                        crate::build::sqlite3_vdbe_explain(p, false, format!("USING INDEX {idx_name} FOR IN-OPERATOR"));
                        p.v.add_op3(OP_OpenRead, i_tab, idx_tnum as i32, i_db);
                        crate::schema::sqlite3_vdbe_set_p4_key_info(p, idx);
                        e_type = IN_INDEX_INDEX_ASC + sort0;

                        if let Some(r) = pr_rhs_has_null.as_deref_mut() {
                            p.n_mem += 1;
                            *r = p.n_mem;
                            if n_expr == 1 {
                                let reg = p.n_mem;
                                sqlite3_set_has_null_flag(p, i_tab, reg, sort0);
                            }
                        }
                        p.v.jump_here(i_addr);
                    }
                } // End loop over indexes
            } // End if( affinity_ok )
        } // End if not an rowid index
    } // End attempt to optimize using an index
    find_in_index_tail(p, x, in_flags, pr_rhs_has_null, ai_map, pi_tab, i_tab, e_type)
}

/// The rest of `sqlite3FindInIndex()` after the attempt to use an existing table or index.
#[allow(clippy::too_many_arguments)]
fn find_in_index_tail(
    p: &mut Parse,
    x: ExprId,
    in_flags: u32,
    mut pr_rhs_has_null: Option<&mut i32>,
    ai_map: Option<&mut Vec<i32>>,
    pi_tab: &mut i32,
    mut i_tab: i32,
    mut e_type: i32,
) -> i32 {

    // If no preexisting index is available for the IN clause and IN_INDEX_NOOP is an allowed
    // reply and the RHS of the IN operator is a list, not a subquery and the RHS is not
    // constant or has two or fewer terms, then it is not worth creating an ephemeral table to
    // evaluate the IN operator so return IN_INDEX_NOOP.
    if e_type == 0
        && (in_flags & IN_INDEX_NOOP_OK as u32) != 0
        && p.e(x).use_x_list()
        && (!sqlite3_in_rhs_is_constant(p, x) || p.l(p.e(x).x_list()).n_expr() <= 2)
    {
        p.n_tab -= 1; // Back out the allocation of the unused cursor
        i_tab = -1; // Cursor is not allocated
        e_type = IN_INDEX_NOOP;
    }

    if e_type == 0 {
        // Could not find an existing table or index to use as the RHS b-tree. We will have to
        // generate an ephemeral table to do the job.
        let saved_n_query_loop = p.n_query_loop;
        let mut r_may_have_null = 0;
        let mut bloom_ok = (in_flags & IN_INDEX_MEMBERSHIP as u32) != 0;
        e_type = IN_INDEX_EPH;
        if in_flags & IN_INDEX_LOOP as u32 != 0 {
            p.n_query_loop = 0;
        } else if let Some(r) = pr_rhs_has_null.as_deref_mut() {
            p.n_mem += 1;
            r_may_have_null = p.n_mem;
            *r = r_may_have_null;
        }
        if !bloom_ok && p.e(x).use_x_select() && (p.s(p.e(x).x_select()).sel_flags & SF_ClonedRhsIn) != 0 {
            bloom_ok = true;
        }
        sqlite3_code_rhs_of_in(p, x, i_tab, bloom_ok);
        if r_may_have_null != 0 {
            sqlite3_set_has_null_flag(p, i_tab, r_may_have_null, SQLITE_SO_ASC);
        }
        p.n_query_loop = saved_n_query_loop;
    }

    if let Some(map) = ai_map
        && e_type != IN_INDEX_INDEX_ASC
        && e_type != IN_INDEX_INDEX_DESC
    {
        let n = sqlite3_expr_vector_size(p, p.e(x).p_left);
        for i in 0..n {
            map[i as usize] = i;
        }
    }
    *pi_tab = i_tab;
    e_type
}

/// `exprINAffinity(pParse, pExpr)`: the affinity string for the comparisons of an IN
/// operator: one byte per LHS column. A byte may be 0 (no affinity).
fn expr_in_affinity(p: &Parse, e: ExprId) -> Vec<u8> {
    let left = p.e(e).p_left;
    let n_val = sqlite3_expr_vector_size(p, left);
    let select = if p.e(e).use_x_select() { p.e(e).x_select() } else { SelectId::NULL };
    let mut z_ret = Vec::with_capacity(n_val as usize);
    for i in 0..n_val {
        let p_a = sqlite3_vector_field_subexpr(p, left, i);
        let a = sqlite3_expr_affinity(p, p_a);
        if select.nn() {
            let r = p.l(p.s(select).p_e_list).a[i as usize].p_expr;
            z_ret.push(sqlite3_compare_affinity(p, r, a));
        } else {
            z_ret.push(a);
        }
    }
    z_ret
}

/// The C string value of an affinity buffer (up to the first NUL).
fn aff_cstr(z: &[u8]) -> String {
    let end = z.iter().position(|&c| c == 0).unwrap_or(z.len());
    String::from_utf8_lossy(&z[..end]).into_owned()
}

/// `sqlite3SubselectError(pParse, nActual, nExpect)`: load the Parse object with an error
/// message of the form "sub-select returns N columns - expected M".
pub fn sqlite3_subselect_error(p: &mut Parse, n_actual: i32, n_expect: i32) {
    if p.n_err == 0 {
        p.error_msg(format!("sub-select returns {n_actual} columns - expected {n_expect}"));
    }
}

/// `sqlite3VectorErrorMsg(pParse, pExpr)`: `e` is a vector that has been used in a context
/// where it is not permitted.
pub fn sqlite3_vector_error_msg(p: &mut Parse, e: ExprId) {
    if p.e(e).use_x_select() {
        let n = p.l(p.s(p.e(e).x_select()).p_e_list).n_expr();
        sqlite3_subselect_error(p, n, 1);
    } else {
        p.error_msg("row value misused");
    }
}

/// `findCompatibleInRhsSubrtn(pParse, pExpr, pNewSig)`: scan all previously generated
/// bytecode looking for an OP_BeginSubrtn that is compatible with the subquery of the IN
/// operator `e`. If one is found, the subroutine is reused.
fn find_compatible_in_rhs_subrtn(p: &mut Parse, e: ExprId, new_sig: Option<&SubrtnSig>) -> bool {
    let Some(new_sig) = new_sig else { return false };
    if (p.m_subrtn_sig & (1 << (new_sig.sel_id & 7))) == 0 {
        return false;
    }
    let n = p.v.a_op.len();
    // for(pOp=GetOp(v,1); pOp<GetLastOp(v); pOp++)
    for i in 1..n.saturating_sub(1) {
        let P4::SubrtnSig(sig) = &p.v.a_op[i].p4 else { continue };
        if !sig.b_complete {
            continue;
        }
        if new_sig.sel_id != sig.sel_id {
            continue;
        }
        if new_sig.z_aff != sig.z_aff {
            continue;
        }
        let (i_addr, reg_return, i_table) = (sig.i_addr, sig.reg_return, sig.i_table);
        let x = p.em(e);
        x.y_sub_i_addr = i_addr;
        x.y_sub_reg_return = reg_return;
        x.i_table = i_table;
        x.flags |= EP_Subrtn;
        return true;
    }
    false
}

/// `sqlite3CodeRhsOfIN(pParse, pExpr, iTab, allowBloom)`: generate code that will construct an
/// ephemeral table containing all terms in the RHS of an IN operator. The table is cursor
/// `i_tab`. If the RHS is constant the construction is wrapped in a subroutine that runs once.
pub fn sqlite3_code_rhs_of_in(p: &mut Parse, e: ExprId, i_tab: i32, mut allow_bloom: bool) {
    let mut addr_once = 0;
    let mut sig: Option<SubrtnSig> = None;
    let mut addr_sig = -1;

    // The evaluation of the IN must be repeated every time it is encountered if it contains
    // any correlated variables (EP_VarSelect) or is within a CHECK constraint / index
    // expression (iSelfTab). Otherwise the result is constant and can be computed once.
    if !p.e(e).has(EP_VarSelect) && p.i_self_tab == 0 {
        // Reuse of the RHS is allowed. If this is the first time, compute and record the
        // RHS in a subroutine; otherwise reuse the subroutine.
        if p.e(e).use_x_select() && (p.s(p.e(e).x_select()).sel_flags & SF_All) == 0 {
            sig = Some(SubrtnSig {
                sel_id: p.s(p.e(e).x_select()).sel_id as i32,
                b_complete: false,
                z_aff: aff_cstr(&expr_in_affinity(p, e)),
                i_table: 0,
                i_addr: 0,
                reg_return: 0,
            });
        }
        // Check to see if there is a prior materialization of the RHS of this IN operator.
        // If there is, then make use of that prior materialization rather than recomputing it.
        if p.e(e).has(EP_Subrtn) || find_compatible_in_rhs_subrtn(p, e, sig.as_ref()) {
            addr_once = p.v.add_op0(OP_Once);
            if p.e(e).use_x_select() {
                let id = p.s(p.e(e).x_select()).sel_id;
                crate::build::sqlite3_vdbe_explain(p, false, format!("REUSE LIST SUBQUERY {id}"));
            }
            let (reg_return, i_addr, tab) = (p.e(e).y_sub_reg_return, p.e(e).y_sub_i_addr, p.e(e).i_table);
            p.v.add_op2(OP_Gosub, reg_return, i_addr);
            p.v.add_op2(OP_OpenDup, i_tab, tab);
            p.v.jump_here(addr_once);
            return;
        }

        // Begin coding the subroutine
        p.em(e).flags |= EP_Subrtn;
        p.n_mem += 1;
        let reg_return = p.n_mem;
        let i_addr = p.v.add_op2(OP_BeginSubrtn, 0, reg_return) + 1;
        p.em(e).y_sub_reg_return = reg_return;
        p.em(e).y_sub_i_addr = i_addr;
        if let Some(s) = sig.as_mut() {
            s.b_complete = false;
            s.i_addr = i_addr;
            s.reg_return = reg_return;
            s.i_table = i_tab;
            p.m_subrtn_sig = 1 << (s.sel_id & 7);
            addr_sig = p.v.current_addr() - 1;
            p.v.change_p4(-1, P4::SubrtnSig(Box::new(s.clone())));
        }
        addr_once = p.v.add_op0(OP_Once);
    }

    // Check to see if this is a vector IN operator
    let left = p.e(e).p_left;
    let n_val = sqlite3_expr_vector_size(p, left);

    // Construct the ephemeral table that will contain the content of RHS of the IN operator.
    p.em(e).i_table = i_tab;
    let addr = p.v.add_op2(OP_OpenEphemeral, i_tab, n_val);
    let mut key_info: Option<KeyInfo> = Some(KeyInfo::alloc(p.enc(), n_val as usize, 1));

    if p.e(e).use_x_select() {
        // Case 1:     expr IN (SELECT ...)
        //
        // Generate code to write the results of the select into the temporary table allocated
        // and opened above.
        let select = p.e(e).x_select();
        let e_list = p.s(select).p_e_list;
        {
            let id = p.s(select).sel_id;
            let corr = if addr_once != 0 { "" } else { "CORRELATED " };
            crate::build::sqlite3_vdbe_explain(p, true, format!("{corr}LIST SUBQUERY {id}"));
        }
        // If the LHS and RHS of the IN operator do not match, that error will have been
        // caught earlier.
        if p.l(e_list).n_expr() == n_val {
            let mut addr_bloom = 0;
            let mut dest = SelectDest { e_dest: SRT_Set as u8, i_sd_parm: i_tab, ..SelectDest::default() };
            dest.z_aff_sdst = Some(expr_in_affinity(p, e));
            p.sm(select).i_limit = 0;
            for i in 0..n_val {
                let x = sqlite3_vector_field_subexpr(p, left, i);
                let rhs = p.l(e_list).a[i as usize].p_expr;
                let coll = sqlite3_binary_compare_coll_seq(p, x, rhs);
                if !sqlite3_is_binary(coll.as_deref()) {
                    allow_bloom = false;
                }
                if let Some(k) = key_info.as_mut() {
                    k.a_coll[i as usize] = coll;
                }
            }
            if addr_once != 0 && allow_bloom && p.db.optimization_enabled(SQLITE_BloomFilter) {
                p.n_mem += 1;
                let reg_bloom = p.n_mem;
                addr_bloom = p.v.add_op2(OP_Blob, 10000, reg_bloom);
                dest.i_sd_parm2 = reg_bloom;
                p.v.change_p4(addr, P4::KeyInfo(Arc::new(key_info.take().expect("key info"))));
            }
            let copy = sqlite3_select_dup(p, select, 0);
            let rc = sqlite3_select(p, copy, &mut dest);
            if addr_bloom != 0 {
                // Remember that location of the Bloom filter in the P3 operand of the OP_Once
                // that began this subroutine.
                p.v.get_op_mut(addr_once).p3 = dest.i_sd_parm2;
                if dest.i_sd_parm2 == 0 {
                    // If the Bloom filter won't actually be used, keep it small
                    p.v.get_op_mut(addr_bloom).p1 = 10;
                }
            }
            if rc != 0 {
                return;
            }
        }
    } else {
        // Case 2:     expr IN (exprlist)
        //
        // For each expression, build an index key from the evaluation and store it in the
        // temporary table. If <expr> is a column, then use that columns affinity when
        // building index keys. If <expr> is not a column, use numeric affinity.
        let list = p.e(e).x_list();
        let mut affinity = sqlite3_expr_affinity(p, left);
        if affinity <= SQLITE_AFF_NONE {
            affinity = SQLITE_AFF_BLOB;
        } else if affinity == SQLITE_AFF_REAL {
            affinity = SQLITE_AFF_NUMERIC;
        }
        let coll = sqlite3_expr_coll_seq(p, left);
        if let Some(k) = key_info.as_mut() {
            k.a_coll[0] = coll;
        }

        // Loop through each expression in <exprlist>.
        let r1 = sqlite3_get_temp_reg(p);
        let r2 = sqlite3_get_temp_reg(p);
        for i in 0..p.l(list).a.len() {
            let e2 = p.l(list).a[i].p_expr;
            // If the expression is not constant then we will need to disable the test that
            // was generated above that makes sure this code only executes once. Because for a
            // non-constant expression we need to rerun this code each time.
            if addr_once != 0 && sqlite3_expr_is_constant(p, e2) == 0 {
                p.v.change_to_noop(addr_once - 1);
                p.v.change_to_noop(addr_once);
                p.em(e).flags &= !EP_Subrtn;
                addr_once = 0;
            }
            // Evaluate the expression and insert it into the temp table
            sqlite3_expr_code(p, e2, r1);
            p.v.add_op4(OP_MakeRecord, r1, 1, r2, P4::Dynamic(vec![affinity]));
            p.v.add_op4_int(OP_IdxInsert, i_tab, r2, r1, 1);
        }
        sqlite3_release_temp_reg(p, r1);
        sqlite3_release_temp_reg(p, r2);
    }
    if sig.is_some() && addr_sig >= 0 {
        // pSig->bComplete = 1  (the SubrtnSig object is the P4 of the OP_BeginSubrtn)
        if let P4::SubrtnSig(s) = &mut p.v.get_op_mut(addr_sig).p4 {
            s.b_complete = true;
        }
    }
    if let Some(k) = key_info {
        p.v.change_p4(addr, P4::KeyInfo(Arc::new(k)));
    }
    if addr_once != 0 {
        p.v.add_op1(OP_NullRow, i_tab);
        p.v.jump_here(addr_once);
        // Subroutine return
        let (reg_return, i_addr) = (p.e(e).y_sub_reg_return, p.e(e).y_sub_i_addr);
        p.v.add_op3(OP_Return, reg_return, i_addr, 1);
        sqlite3_clear_temp_reg_cache(p);
    }
}

/// `sqlite3IsBinary(p)`: true if the collating sequence is BINARY (or NULL).
pub fn sqlite3_is_binary(c: Option<&CollSeq>) -> bool {
    match c {
        None => true,
        Some(c) => c.name.eq_ignore_ascii_case("BINARY"),
    }
}

/// `sqlite3CodeSubselect(pParse, pExpr)`: generate code for scalar subqueries used as a
/// subquery expression or EXISTS operator: `(SELECT a FROM b)`, `EXISTS (SELECT a FROM b)`.
/// Returns the register that holds the result (the first of several for a row value).
pub fn sqlite3_code_subselect(p: &mut Parse, e: ExprId) -> i32 {
    let mut addr_once = 0;
    if p.n_err != 0 {
        return 0;
    }
    let sel = p.e(e).x_select();

    // If this routine has already been coded, then invoke it as a subroutine.
    if p.e(e).has(EP_Subrtn) {
        let id = p.s(sel).sel_id;
        crate::build::sqlite3_vdbe_explain(p, false, format!("REUSE SUBQUERY {id}"));
        let (reg_return, i_addr) = (p.e(e).y_sub_reg_return, p.e(e).y_sub_i_addr);
        p.v.add_op2(OP_Gosub, reg_return, i_addr);
        return p.e(e).i_table;
    }

    // Begin coding the subroutine
    p.em(e).flags |= EP_Subrtn;
    p.n_mem += 1;
    let reg_return = p.n_mem;
    let i_addr = p.v.add_op2(OP_BeginSubrtn, 0, reg_return) + 1;
    p.em(e).y_sub_reg_return = reg_return;
    p.em(e).y_sub_i_addr = i_addr;

    // The evaluation of the EXISTS/SELECT must be repeated every time it is encountered if it
    // contains correlated variables. If all variables are constant, it is done once.
    if !p.e(e).has(EP_VarSelect) {
        addr_once = p.v.add_op0(OP_Once);
    }

    // For a SELECT, generate code to put the values for all columns of the first row into an
    // array of registers and return the index of the first register. If this is an EXISTS,
    // write an integer 0 (not exists) or 1 (exists) into a memory cell and record that memory
    // cell in iColumn.
    {
        let id = p.s(sel).sel_id;
        let corr = if addr_once != 0 { "" } else { "CORRELATED " };
        crate::build::sqlite3_vdbe_explain(p, true, format!("{corr}SCALAR SUBQUERY {id}"));
    }
    let n_reg = if p.e(e).op == TK_SELECT { p.l(p.s(sel).p_e_list).n_expr() } else { 1 };
    let mut dest = SelectDest { e_dest: 0, i_sd_parm: p.n_mem + 1, ..SelectDest::default() };
    p.n_mem += n_reg;
    if p.e(e).op == TK_SELECT {
        dest.e_dest = SRT_Mem as u8;
        let lim = p.s(sel).p_limit;
        if (p.s(sel).sel_flags & SF_Distinct) != 0 && lim.nn() && p.e(lim).p_right.nn() {
            // If there is both a DISTINCT and an OFFSET clause, then allocate a separate
            // dest.iSdst array for sqlite3Select() and other routines to hold temporary
            // values for each output row, since without a separate array, the values might
            // overwrite the outer row.
            dest.i_sdst = p.n_mem + 1;
            p.n_mem += n_reg;
        } else {
            dest.i_sdst = dest.i_sd_parm;
        }
        dest.n_sdst = n_reg;
        p.v.add_op3(OP_Null, 0, dest.i_sd_parm, p.n_mem);
    } else {
        dest.e_dest = SRT_Exists as u8;
        p.v.add_op2(OP_Integer, 0, dest.i_sd_parm);
    }
    if p.s(sel).p_limit.nn() {
        // The subquery already has a limit. If the pre-existing limit X is other than 0 or 1,
        // then make the new limit X<>0 so that the new limit is either 1 or 0
        let lim = p.s(sel).p_limit;
        let left = p.e(lim).p_left;
        if !p.e(left).has(EP_IntValue) || (p.e(left).i_value != 1 && p.e(left).i_value != 0) {
            let zero = sqlite3_expr_int32(p, 0);
            p.em(zero).aff_expr = SQLITE_AFF_NUMERIC;
            let dup = sqlite3_expr_dup(p, left, 0);
            let ne = sqlite3_p_expr(p, TK_NE, dup, zero);
            p.em(lim).p_left = ne;
        }
    } else {
        // If there is no pre-existing limit add a limit of 1
        let one = sqlite3_expr_int32(p, 1);
        let lim = sqlite3_p_expr(p, TK_LIMIT, one, ExprId::NULL);
        p.sm(sel).p_limit = lim;
    }
    p.sm(sel).i_limit = 0;
    if sqlite3_select(p, sel, &mut dest) != 0 {
        let op = p.e(e).op;
        p.em(e).op2 = op;
        p.em(e).op = TK_ERROR;
        return 0;
    }
    let r_reg = dest.i_sd_parm;
    p.em(e).i_table = r_reg;
    if addr_once != 0 {
        p.v.jump_here(addr_once);
    }
    if dest.e_dest == SRT_Mem as u8 {
        let first = p.l(p.s(sel).p_e_list).a[0].p_expr;
        if sqlite3_expr_can_return_subtype(p, first) {
            p.v.add_op1(OP_ClrSubtype, r_reg);
        }
    }

    // Subroutine return
    p.v.add_op3(OP_Return, reg_return, i_addr, 1);
    sqlite3_clear_temp_reg_cache(p);
    r_reg
}

/// `sqlite3ExprCheckIN(pParse, pIn)`: `p_in` is an IN operator. If the IN operator is a
/// sub-select on the RHS, check the vector sizes match; otherwise the LHS must be a scalar.
/// Returns true (and leaves an error) on a mismatch.
pub fn sqlite3_expr_check_in(p: &mut Parse, p_in: ExprId) -> bool {
    let left = p.e(p_in).p_left;
    let n_vector = sqlite3_expr_vector_size(p, left);
    if p.e(p_in).use_x_select() {
        let n = p.l(p.s(p.e(p_in).x_select()).p_e_list).n_expr();
        if n_vector != n {
            sqlite3_subselect_error(p, n, n_vector);
            return true;
        }
    } else if n_vector != 1 {
        sqlite3_vector_error_msg(p, left);
        return true;
    }
    false
}

/// `sqlite3ExprCodeIN(pParse, pExpr, destIfFalse, destIfNull)`: generate code for an IN
/// expression `x IN (SELECT ...)` / `x IN (value, value, ...)`. Jumps to `dest_if_false` if
/// the LHS is not contained within the RHS and to `dest_if_null` if the result is NULL; falls
/// through if the LHS is contained within the RHS.
fn sqlite3_expr_code_in(p: &mut Parse, e: ExprId, dest_if_false: i32, dest_if_null: i32) {
    let mut r_rhs_has_null = 0;
    let dest_step2;
    let mut dest_step6 = 0;
    let addr_truth_op;
    let mut i_tab = 0;
    let ok_const_factor = p.ok_const_factor;

    let left = p.e(e).p_left;
    if sqlite3_expr_check_in(p, e) {
        return;
    }
    let z_aff = expr_in_affinity(p, e);
    let n_vector = sqlite3_expr_vector_size(p, left);
    let mut ai_map: Vec<i32> = vec![0; n_vector as usize];

    // Attempt to compute the RHS. After this step, if anything other than IN_INDEX_NOOP is
    // returned, the table opened with cursor iTab contains the RHS values. If no preexisting
    // index is available and the RHS is a short constant list, IN_INDEX_NOOP is returned.
    let e_type = sqlite3_find_in_index(
        p,
        e,
        (IN_INDEX_MEMBERSHIP | IN_INDEX_NOOP_OK) as u32,
        if dest_if_false == dest_if_null { None } else { Some(&mut r_rhs_has_null) },
        Some(&mut ai_map),
        &mut i_tab,
    );

    // Code the LHS, the <expr> from "<expr> IN (...)". If the LHS is a vector, then
    // sqlite3ExprCodeVector() allocates an array of memory cells and stores the results there.
    // The LHS cannot be factored out because it is used as a Yield argument etc.
    p.ok_const_factor = false;
    let mut i_dummy = 0;
    let mut r_lhs = expr_code_vector(p, left, &mut i_dummy);
    p.ok_const_factor = ok_const_factor;

    // If sqlite3FindInIndex() did not find or create an index that is suitable for evaluating
    // the IN operator, then evaluate using a sequence of comparisons.
    if e_type == IN_INDEX_NOOP {
        let label_ok = p.v.make_label();
        let mut reg_ck_null = 0;
        let list = p.e(e).x_list();
        let coll = sqlite3_expr_coll_seq(p, left);
        if dest_if_null != dest_if_false {
            reg_ck_null = sqlite3_get_temp_reg(p);
            p.v.add_op3(OP_BitAnd, r_lhs, r_lhs, reg_ck_null);
        }
        let n = p.l(list).a.len();
        for ii in 0..n {
            let item = p.l(list).a[ii].p_expr;
            let mut reg_to_free = 0;
            let r2 = sqlite3_expr_code_temp(p, item, &mut reg_to_free);
            if reg_ck_null != 0 && sqlite3_expr_can_be_null(p, item) {
                p.v.add_op3(OP_BitAnd, reg_ck_null, r2, reg_ck_null);
            }
            sqlite3_release_temp_reg(p, reg_to_free);
            if ii < n - 1 || dest_if_null != dest_if_false {
                let op = if r_lhs != r2 { OP_Eq } else { OP_NotNull };
                p.v.add_op4(op, r_lhs, label_ok, r2, p4_coll(coll.clone()));
                p.v.change_p5(z_aff[0] as u16);
            } else {
                let op = if r_lhs != r2 { OP_Ne } else { OP_IsNull };
                p.v.add_op4(op, r_lhs, dest_if_false, r2, p4_coll(coll.clone()));
                p.v.change_p5(z_aff[0] as u16 | SQLITE_JUMPIFNULL as u16);
            }
        }
        if reg_ck_null != 0 {
            p.v.add_op2(OP_IsNull, reg_ck_null, dest_if_null);
            p.v.goto(dest_if_false);
        }
        p.v.resolve_label(label_ok);
        sqlite3_release_temp_reg(p, reg_ck_null);
        return;
    }

    if e_type != IN_INDEX_ROWID {
        // If this IN operator will use an index, then the order of columns in the vector
        // might be different from the order in the index. In that case, we need to reorder
        // the LHS values to be in index order. Hence the Copy and Affinity operations.
        if n_vector == 1 {
            let aff = z_aff[0];
            if aff >= SQLITE_AFF_TEXT && aff != sqlite3_expr_affinity(p, left) {
                let r_tmp = sqlite3_get_temp_reg(p);
                p.v.add_op3(OP_Copy, r_lhs, r_tmp, 0);
                r_lhs = r_tmp;
                p.v.add_op4(OP_Affinity, r_lhs, 1, 0, P4::Dynamic(vec![aff]));
            }
        } else {
            p.v.add_op4(OP_Affinity, r_lhs, n_vector, 0, P4::Dynamic(z_aff.clone()));
        }
        let mut i = 0;
        while i < n_vector && ai_map[i as usize] == i {
            i += 1;
        }
        if i != n_vector {
            // Need to reorder the LHS fields
            let r_lhs_orig = r_lhs;
            r_lhs = sqlite3_get_temp_range(p, n_vector);
            for i in 0..n_vector {
                p.v.add_op3(OP_Copy, r_lhs_orig + i, r_lhs + ai_map[i as usize], 0);
            }
        }
    }

    // Step 2: Check to see if the LHS contains any NULL columns. If the LHS contains NULLs
    // then the result must be either FALSE or NULL. We will then skip the binary search of
    // the RHS.
    if dest_if_null == dest_if_false {
        dest_step2 = dest_if_false;
    } else {
        dest_step6 = p.v.make_label();
        dest_step2 = dest_step6;
    }
    for i in 0..n_vector {
        let x = sqlite3_vector_field_subexpr(p, left, i);
        if p.n_err != 0 {
            return;
        }
        if sqlite3_expr_can_be_null(p, x) {
            p.v.add_op2(OP_IsNull, r_lhs + ai_map[i as usize], dest_step2);
        }
    }

    // Step 3. The LHS is now known to be non-NULL. Do the binary search of the RHS using the
    // LHS as a probe. If found, the result is true.
    if e_type == IN_INDEX_ROWID {
        // In this case, the RHS is the ROWID of table b-tree and so we also know that the
        // RHS is non-NULL. Hence, we combine steps 3 and 4 into a single opcode.
        p.v.add_op3(OP_SeekRowid, i_tab, dest_if_false, r_lhs);
        addr_truth_op = p.v.add_op0(OP_Goto); // Return True
    } else {
        if dest_if_false == dest_if_null {
            // Combine Step 3 and Step 5 into a single opcode
            if p.e(e).has(EP_Subrtn) {
                let p3 = p.v.get_op(p.e(e).y_sub_i_addr).p3;
                if p3 > 0 {
                    p.v.add_op4_int(OP_Filter, p3, dest_if_false, r_lhs, n_vector);
                }
            }
            p.v.add_op4_int(OP_NotFound, i_tab, dest_if_false, r_lhs, n_vector);
            return;
        }
        // Ordinary Step 3, for the case where FALSE and NULL are distinct
        addr_truth_op = p.v.add_op4_int(OP_Found, i_tab, 0, r_lhs, n_vector);
    }

    // Step 4. If the RHS is known to be non-NULL and we did not find an match on the RHS,
    // then the answer is FALSE.
    if r_rhs_has_null != 0 && n_vector == 1 {
        p.v.add_op2(OP_NotNull, r_rhs_has_null, dest_if_false);
    }

    // Step 5. If we do not care about the difference between NULL and FALSE, then just
    // return false.
    if dest_if_false == dest_if_null {
        p.v.goto(dest_if_false);
    }

    // Step 6: Loop through rows of the RHS. Compare each row to the LHS. If any comparison
    // is NULL, then the result is NULL. If all comparisons are FALSE then the final result
    // is FALSE.
    if dest_step6 != 0 {
        p.v.resolve_label(dest_step6);
    }
    let addr_top = if e_type == IN_INDEX_INDEX_DESC {
        p.v.add_op2(OP_Last, i_tab, dest_if_false)
    } else {
        p.v.add_op2(OP_Rewind, i_tab, dest_if_false)
    };
    let dest_not_null = if n_vector > 1 { p.v.make_label() } else { dest_if_false };
    for i in 0..n_vector {
        let r3 = sqlite3_get_temp_reg(p);
        let x = sqlite3_vector_field_subexpr(p, left, i);
        let coll = if p.e(e).use_x_select() {
            let rhs = p.l(p.s(p.e(e).x_select()).p_e_list).a[i as usize].p_expr;
            sqlite3_binary_compare_coll_seq(p, x, rhs)
        } else {
            sqlite3_expr_coll_seq(p, x)
        };
        p.v.add_op3(OP_Column, i_tab, ai_map[i as usize], r3);
        p.v.add_op4(OP_Ne, r_lhs + ai_map[i as usize], dest_not_null, r3, p4_coll(coll));
        sqlite3_release_temp_reg(p, r3);
    }
    p.v.add_op2(OP_Goto, 0, dest_if_null);
    if n_vector > 1 {
        p.v.resolve_label(dest_not_null);
        p.v.add_op2(OP_Next, i_tab, addr_top + 1);
        // Step 7: If we reach this point, we know that the result must be false.
        p.v.add_op2(OP_Goto, 0, dest_if_false);
    }

    // Jumps here in order to return true.
    p.v.jump_here(addr_truth_op);
}

/// `codeReal(v, z, negateFlag, iMem)`: generate an instruction that will put the floating
/// point value described by `z` into register `i_mem`.
fn code_real(p: &mut Parse, z: &str, negate_flag: bool, i_mem: i32) {
    let (mut value, _) = sqliters_types::fp::sqlite_atof(z.as_bytes());
    if negate_flag {
        value = -value;
    }
    p.v.add_double(i_mem, value);
}

/// `codeInteger(pParse, pExpr, negFlag, iMem)`: generate an instruction that will put the
/// integer described by expression `e` into register `i_mem`.
fn code_integer(p: &mut Parse, e: ExprId, neg_flag: bool, i_mem: i32) {
    if p.e(e).flags & EP_IntValue != 0 {
        let mut i = p.e(e).i_value;
        if neg_flag {
            i = i.wrapping_neg();
        }
        p.v.add_op2(OP_Integer, i, i_mem);
    } else {
        let z = p.e(e).tok().to_owned();
        let (mut value, c) = sqlite3_dec_or_hex_to_i64(&z);
        if (c == 3 && !neg_flag) || c == 2 || (neg_flag && value == i64::MIN) {
            if z.len() >= 2 && z.as_bytes()[..2].eq_ignore_ascii_case(b"0x") {
                let msg = format!("hex literal too big: {}{}", if neg_flag { "-" } else { "" }, z);
                p.error_msg_expr(msg, e);
            } else {
                code_real(p, &z, neg_flag, i_mem);
            }
        } else {
            if neg_flag {
                value = if c == 3 { i64::MIN } else { value.wrapping_neg() };
            }
            p.v.add_int64(i_mem, value);
        }
    }
}

/// `sqlite3ExprCodeLoadIndexColumn(pParse, pIdx, iTabCur, iIdxCol, regOut)`: generate code
/// that will load into register regOut a value that is appropriate for the iIdxCol-th
/// column of index pIdx.
pub fn sqlite3_expr_code_load_index_column(p: &mut Parse, idx: IndexId, i_tab_cur: i32, i_idx_col: i32, reg_out: i32) {
    let i_tab_col = p.idx(idx).ai_column[i_idx_col as usize] as i32;
    if i_tab_col == XN_EXPR {
        let e = p.l(p.idx(idx).a_col_expr).a[i_idx_col as usize].p_expr;
        p.i_self_tab = i_tab_cur + 1;
        sqlite3_expr_code_copy(p, e, reg_out);
        p.i_self_tab = 0;
    } else {
        let tab = p.idx(idx).p_table;
        sqlite3_expr_code_get_column_of_table(p, tab, i_tab_cur, i_tab_col, reg_out);
    }
}

/// `sqlite3ColumnDefault(v, pTab, i, iReg)`: the most recently coded instruction was an
/// OP_Column to retrieve the i-th column of table pTab. This routine sets the P4 parameter
/// of the OP_Column to the default value, if any.
///
/// The default value of a column is specified by a DEFAULT clause in the column definition.
/// This was either supplied by the user when the table was created, or added later to the
/// table definition by an ALTER TABLE command. If the latter, then the row-records in the
/// table btree on disk may not contain a value for the column and the default value, taken
/// from the P4 parameter of the OP_Column instruction, is returned instead. If the former,
/// then all row-records are guaranteed to include a value for the column and the P4 value is
/// not required.
///
/// Column definitions created by an ALTER TABLE command may only have literal default
/// values. Other, more complex expressions, are stored without a P4 value.
///
/// If column as REAL affinity and the table is an ordinary b-tree table (not a virtual
/// table) then the value might have been stored as an integer. In that case, add an
/// OP_RealAffinity opcode to make sure it has been converted into REAL.
pub fn sqlite3_column_default(p: &mut Parse, tab: TableId, i: i32, i_reg: i32) {
    let (has_dflt, aff) = {
        let c = &p.tab(tab).a_col[i as usize];
        (c.i_dflt != 0, c.affinity)
    };
    if has_dflt {
        // sqlite3ValueFromExpr(db, sqlite3ColumnExpr(pTab,pCol), enc, pCol->affinity, &pValue)
        let x = crate::build_tab::sqlite3_column_expr(p, tab, i as usize);
        if let Some(v) = crate::build_tab::sqlite3_value_from_expr(p, x, aff) {
            p.v.append_p4(P4::Mem(v));
        }
    }
    if aff == SQLITE_AFF_REAL {
        p.v.add_op1(OP_RealAffinity, i_reg);
    }
}

/// `sqlite3ExprCodeGeneratedColumn(pParse, pTab, pCol, regOut)`: generate code that will
/// compute the value of generated column pCol and store the result in register regOut.
pub fn sqlite3_expr_code_generated_column(p: &mut Parse, tab: TableId, i_col: usize, reg_out: i32) {
    let n_err = p.n_err;
    let i_addr = if p.i_self_tab > 0 { p.v.add_op3(OP_IfNullRow, p.i_self_tab - 1, 0, reg_out) } else { 0 };
    let x = crate::build_tab::sqlite3_column_expr(p, tab, i_col);
    sqlite3_expr_code_copy(p, x, reg_out);
    let (col_flags, aff) = (p.tab(tab).a_col[i_col].col_flags, p.tab(tab).a_col[i_col].affinity);
    if (col_flags & COLFLAG_VIRTUAL as u16) != 0 && (p.tab(tab).tab_flags & TF_Strict) != 0 {
        let p3 = 2 + i_col as i32;
        let p4 = crate::schema::p4_table_of(p, tab);
        p.v.add_op4(OP_TypeCheck, reg_out, 1, p3, p4);
    } else if aff >= SQLITE_AFF_TEXT {
        const Z_AFF: [&str; 5] = ["B", "C", "D", "E", "F"];
        p.v.add_op4(OP_Affinity, reg_out, 1, 0, P4::Static(Z_AFF[(aff - b'B') as usize]));
    }
    if i_addr != 0 {
        p.v.jump_here(i_addr);
    }
    if p.n_err > n_err {
        p.err_byte_offset = -1;
    }
}

/// `sqlite3ExprCodeGetColumnOfTable(v, pTab, iTabCur, iCol, regOut)`: generate code to extract
/// the value of the iCol-th column of a table.
pub fn sqlite3_expr_code_get_column_of_table(p: &mut Parse, tab: TableId, i_tab_cur: i32, i_col: i32, reg_out: i32) {
    if i_col < 0 || i_col == p.tab(tab).i_p_key as i32 {
        p.v.add_op2(OP_Rowid, i_tab_cur, reg_out);
    } else {
        let x;
        if p.tab(tab).is_virtual() {
            p.v.add_op3(OP_VColumn, i_tab_cur, i_col, reg_out);
            return;
        } else if p.tab(tab).a_col[i_col as usize].col_flags & COLFLAG_VIRTUAL as u16 != 0 {
            if p.tab(tab).a_col[i_col as usize].col_flags & COLFLAG_BUSY as u16 != 0 {
                let msg = format!("generated column loop on \"{}\"", p.tab(tab).a_col[i_col as usize].z_cn_name);
                p.error_msg(msg);
            } else {
                let saved_self_tab = p.i_self_tab;
                p.tabm(tab).a_col[i_col as usize].col_flags |= COLFLAG_BUSY as u16;
                p.i_self_tab = i_tab_cur + 1;
                sqlite3_expr_code_generated_column(p, tab, i_col as usize, reg_out);
                p.i_self_tab = saved_self_tab;
                p.tabm(tab).a_col[i_col as usize].col_flags &= !(COLFLAG_BUSY as u16);
            }
            return;
        } else if !p.tab(tab).has_rowid() {
            let pk = crate::schema::sqlite3_primary_key_index(p, tab);
            x = crate::schema::sqlite3_table_column_to_index(p.idx(pk), i_col);
        } else {
            x = crate::schema::sqlite3_table_column_to_storage(p.tab(tab), i_col);
        }
        p.v.add_op3(OP_Column, i_tab_cur, x, reg_out);
        sqlite3_column_default(p, tab, i_col, reg_out);
    }
}

/// `sqlite3ExprCodeGetColumn(pParse, pTab, iColumn, iTable, iReg, p5)`: generate code that
/// will extract the `i_column`-th column from table `tab` and store the column value in
/// register `i_reg`.
pub fn sqlite3_expr_code_get_column(p: &mut Parse, tab: TableId, i_column: i32, i_table: i32, i_reg: i32, p5: u8) -> i32 {
    sqlite3_expr_code_get_column_of_table(p, tab, i_table, i_column, i_reg);
    if p5 != 0 && p.n_err == 0 {
        let op = p.v.get_last_op_mut();
        if op.opcode == OP_Column {
            op.p5 = p5 as u16;
        }
        if op.opcode == OP_VColumn {
            op.p5 = (p5 as i32 & OPFLAG_NOCHNG) as u16;
        }
    }
    i_reg
}

/// `sqlite3ExprCodeMove(pParse, iFrom, iTo, nReg)`: generate code to move content from
/// registers iFrom...iFrom+nReg-1 over to iTo..iTo+nReg-1.
pub fn sqlite3_expr_code_move(p: &mut Parse, i_from: i32, i_to: i32, n_reg: i32) {
    p.v.add_op3(OP_Move, i_from, i_to, n_reg);
}

/// `sqlite3ExprToRegister(pExpr, iReg)`: convert a scalar expression node to a TK_REGISTER
/// referencing register `i_reg`. The caller must ensure that `i_reg` already contains the
/// correct value for the expression.
pub fn sqlite3_expr_to_register(p: &mut Parse, e: ExprId, i_reg: i32) {
    let x = sqlite3_expr_skip_collate_and_likely(p, e);
    if x.is_null() {
        return;
    }
    let n = p.em(x);
    if n.op == TK_REGISTER {
        // (already a register)
    } else {
        n.op2 = n.op;
        n.op = TK_REGISTER;
        n.i_table = i_reg;
        n.flags &= !EP_Skip;
    }
}

/// `exprCodeVector(pParse, p, piFreeable)`: evaluate an expression (either a vector or a
/// scalar expression) and store the result in a contiguous array of registers. Return the
/// index of the first register used to store the result.
fn expr_code_vector(p: &mut Parse, e: ExprId, pi_freeable: &mut i32) -> i32 {
    let i_result;
    let n_result = sqlite3_expr_vector_size(p, e);
    if n_result == 1 {
        i_result = sqlite3_expr_code_temp(p, e, pi_freeable);
    } else {
        *pi_freeable = 0;
        if p.e(e).op == TK_SELECT {
            i_result = sqlite3_code_subselect(p, e);
        } else {
            i_result = p.n_mem + 1;
            p.n_mem += n_result;
            let list = p.e(e).x_list();
            for i in 0..n_result {
                let it = p.l(list).a[i as usize].p_expr;
                sqlite3_expr_code_factorable(p, it, i + i_result);
            }
        }
    }
    i_result
}

/// `setDoNotMergeFlagOnCopy(v)`: if the last opcode is a OP_Copy, then set the do-not-merge
/// flag (bit mask 0x01) on that opcode.
fn set_do_not_merge_flag_on_copy(p: &mut Parse) {
    if p.v.get_last_op().opcode == OP_Copy {
        p.v.change_p5(1); // Tag trailing OP_Copy as not mergeable
    }
}

/// `exprCodeInlineFunction(pParse, pFarg, iFuncId, target)`: generate code to implement
/// special SQL functions that are implemented in-line rather than by using the usual
/// callbacks.
fn expr_code_inline_function(p: &mut Parse, farg: ExprListId, i_func_id: i64, mut target: i32) -> i32 {
    let n_farg = p.l(farg).a.len();
    match i_func_id as i32 {
        INLINEFUNC_coalesce => {
            // Attempt a direct implementation of the built-in COALESCE() and IFNULL()
            // functions. This avoids unnecessary evaluation of arguments past the first
            // non-NULL argument.
            let end_coalesce = p.v.make_label();
            let e0 = p.l(farg).a[0].p_expr;
            sqlite3_expr_code(p, e0, target);
            for i in 1..n_farg {
                p.v.add_op2(OP_NotNull, target, end_coalesce);
                let ei = p.l(farg).a[i].p_expr;
                sqlite3_expr_code(p, ei, target);
            }
            set_do_not_merge_flag_on_copy(p);
            p.v.resolve_label(end_coalesce);
        }
        INLINEFUNC_iif => {
            let case_expr = p.new_expr(Expr { op: TK_CASE, x: farg.0, ..Expr::default() });
            return sqlite3_expr_code_target(p, case_expr, target);
        }
        INLINEFUNC_expr_compare | INLINEFUNC_expr_implies_expr | INLINEFUNC_implies_nonnull_row | INLINEFUNC_affinity
        | INLINEFUNC_sqlite_offset => {
            // Internal test functions (only reachable with SQLITE_TESTCTRL_INTERNAL_FUNCTIONS)
            // and sqlite_offset() (not compiled into the reference build).
            p.not_supported("internal in-line test function");
        }
        _ => {
            // The UNLIKELY() function is a no-op. The result is the value of the first
            // argument.
            let e0 = p.l(farg).a[0].p_expr;
            target = sqlite3_expr_code_target(p, e0, target);
        }
    }
    target
}

/// `exprNodeCanReturnSubtype(pWalker, pExpr)`
fn expr_node_can_return_subtype(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    let x = p.e(e);
    if x.op == TK_CASE || x.op == TK_UPLUS || x.op == TK_COLLATE || x.op == TK_CAST {
        return WRC_Continue;
    }
    if x.op != TK_FUNCTION && x.op != TK_AGG_FUNCTION {
        return WRC_Prune;
    }
    let n = if x.x_list().nn() { p.l(x.x_list()).n_expr() } else { 0 };
    let name = x.tok().to_owned();
    let def = sqlite3_find_function(p, &name, n, 0);
    if let Some(d) = def
        && (d.func_flags & SQLITE_RESULT_SUBTYPE as u32) != 0
    {
        w.e_code = 1;
        return WRC_Abort;
    }
    WRC_Continue
}

/// `sqlite3ExprCanReturnSubtype(pParse, pExpr)`: return TRUE if the expression might return a
/// subtype (it is, or is a transparent wrapper around, a function marked
/// SQLITE_RESULT_SUBTYPE).
pub fn sqlite3_expr_can_return_subtype(p: &mut Parse, e: ExprId) -> bool {
    let mut w = Walker { x_expr_callback: Some(expr_node_can_return_subtype), ..Walker::default() };
    crate::walker::sqlite3_walk_expr(p, &mut w, e);
    w.e_code != 0
}

/// `exprCodeTargetAndOr(pParse, pExpr, target, pTmpReg)`: generate code that evaluates an AND
/// or OR operator leaving a boolean result in a register.
fn expr_code_target_and_or(p: &mut Parse, e: ExprId, target: i32, p_tmp_reg: &mut i32) -> i32 {
    let op = p.e(e).op;
    let alt = sqlite3_expr_simplified_and_or(p, e);
    if alt != e {
        let r1 = sqlite3_expr_code_target(p, alt, target);
        p.v.add_op3(OP_And, r1, r1, target);
        return target;
    }
    let skip_op = if op == TK_AND { OP_IfNot } else { OP_If };
    let (left, right) = (p.e(e).p_left, p.e(e).p_right);
    let (r1, r2, addr_skip, reg_ss);
    if expr_eval_rhs_first(p, e) {
        // Compute the right operand first. Skip the computation of the left operand if the
        // right operand fully determines the result
        r2 = sqlite3_expr_code_target(p, right, target);
        reg_ss = r2;
        addr_skip = p.v.add_op1(skip_op, r2);
        r1 = sqlite3_expr_code_temp(p, left, p_tmp_reg);
    } else {
        r1 = sqlite3_expr_code_target(p, left, target);
        if p.e(right).has(EP_Subquery) {
            // Compute the left operand first. Skip the computation of the right operand if the
            // left operand fully determines the result
            reg_ss = r1;
            addr_skip = p.v.add_op1(skip_op, r1);
        } else {
            reg_ss = 0;
            addr_skip = 0;
        }
        r2 = sqlite3_expr_code_temp(p, right, p_tmp_reg);
    }
    p.v.add_op3(op, r2, r1, target);
    if addr_skip != 0 {
        let a = p.v.current_addr() + 2;
        p.v.add_op2(OP_Goto, 0, a);
        p.v.jump_here(addr_skip);
        p.v.add_op3(OP_Or, reg_ss, reg_ss, target);
    }
    target
}

/// `sqlite3IndexedExprLookup(pParse, pExpr, target)`: check to see if pExpr is one of the
/// indexed expressions on pParse->pIdxEpr. If it is, then resolve the expression by reading
/// from the index, and return the register into which the value has been read. If pExpr is
/// not an indexed expression, then return negative.
fn sqlite3_indexed_expr_lookup(p: &mut Parse, e: ExprId, target: i32) -> i32 {
    // C walks the list from the most recently added entry
    for k in (0..p.p_idx_epr.len()).rev() {
        let ie = p.p_idx_epr[k].clone();
        let mut i_data_cur = ie.i_data_cur;
        if i_data_cur < 0 {
            continue;
        }
        if p.i_self_tab != 0 {
            if ie.i_data_cur != p.i_self_tab - 1 {
                continue;
            }
            i_data_cur = -1;
        }
        if sqlite3_expr_compare(p, false, e, ie.p_expr, i_data_cur) != 0 {
            continue;
        }
        let expr_aff = sqlite3_expr_affinity(p, e);
        if (expr_aff <= SQLITE_AFF_BLOB && ie.aff != SQLITE_AFF_BLOB)
            || (expr_aff == SQLITE_AFF_TEXT && ie.aff != SQLITE_AFF_TEXT)
            || (expr_aff >= SQLITE_AFF_NUMERIC && ie.aff != SQLITE_AFF_NUMERIC)
        {
            // Affinity mismatch on a generated column
            continue;
        }
        // Functions that might set a subtype should not be replaced by the value taken from
        // an expression index if they are themselves an argument to another scalar function
        // or aggregate. https://sqlite.org/forum/forumpost/68d284c86b082c3e
        if p.e(e).has(EP_SubtArg) && sqlite3_expr_can_return_subtype(p, e) {
            continue;
        }
        if ie.b_maybe_null_row || sqlite3_expr_is_constant(p, e) != 0 {
            // The value of pExpr should be computed from the original expression, rather
            // than from the index, if the index row is a NULL row of an outer join (or the
            // expression is a constant: an index-only constant is cheaper to evaluate).
            let addr = p.v.current_addr();
            p.v.add_op3(OP_IfNullRow, ie.i_idx_cur, addr + 3, target);
            p.v.add_op3(OP_Column, ie.i_idx_cur, ie.i_idx_col, target);
            p.v.goto(0);
            let saved = std::mem::take(&mut p.p_idx_epr);
            sqlite3_expr_code(p, e, target);
            p.p_idx_epr = saved;
            p.v.jump_here(addr + 2);
        } else {
            p.v.add_op3(OP_Column, ie.i_idx_cur, ie.i_idx_col, target);
        }
        return target;
    }
    -1 // Not found
}

/// `exprPartidxExprLookup(pParse, pExpr, iTarget)`: expression pExpr is guaranteed to be a
/// TK_COLUMN or equivalent. This function checks the Parse.pIdxPartExpr list to see if this
/// column might be replaced with a constant value. If so, it generates code to store the
/// constant value in a register and returns the register number. Otherwise 0.
fn expr_partidx_expr_lookup(p: &mut Parse, e: ExprId, i_target: i32) -> i32 {
    for k in (0..p.p_idx_part_expr.len()).rev() {
        let ie = p.p_idx_part_expr[k].clone();
        if p.e(e).i_column as i32 == ie.i_idx_col && p.e(e).i_table == ie.i_data_cur {
            let mut addr = 0;
            if ie.b_maybe_null_row {
                addr = p.v.add_op1(OP_IfNullRow, ie.i_idx_cur);
            }
            let ret = sqlite3_expr_code_target(p, ie.p_expr, i_target);
            p.v.add_op4(OP_Affinity, ret, 1, 0, P4::Dynamic(vec![ie.aff]));
            if sqlite3_expr_can_return_subtype(p, ie.p_expr) {
                // If the expression value may have a sub-type, clear it. Values read from
                // the index never have a sub-type.
                p.v.add_op1(OP_ClrSubtype, ret);
            }
            if addr != 0 {
                p.v.jump_here(addr);
                p.v.change_p3(addr, ret);
            }
            return ret;
        }
    }
    0
}

/// `sqlite3ExprCodeTarget(pParse, pExpr, target)`: generate code into the current Vdbe to
/// evaluate the given expression. Attempt to store the results in register `target`. Return
/// the register where results are stored.
///
/// With a little work, this routine can be made to return any register; callers must copy the
/// result if they need it in `target` (see `sqlite3ExprCode`).
pub fn sqlite3_expr_code_target(p: &mut Parse, mut e: ExprId, target: i32) -> i32 {
    let mut in_reg = target;
    let mut reg_free1 = 0;
    let mut reg_free2 = 0;
    let mut p5 = 0;
    let mut op;
    loop {
        // expr_code_doover:
        if e.nn() && !p.p_idx_epr.is_empty() && !p.e(e).has(EP_Leaf) {
            let r1 = sqlite3_indexed_expr_lookup(p, e, target);
            if r1 >= 0 {
                return r1;
            }
        }
        op = if e.is_null() { TK_NULL } else { p.e(e).op };
        match op {
            TK_COLLATE => {
                if !p.e(e).has(EP_Collate) {
                    // A TK_COLLATE Expr node without the EP_Collate tag is a so-called "soft"
                    // collate that is implied by the "collate(X,Y)" function.
                    let left = p.e(e).p_left;
                    sqlite3_expr_code(p, left, target);
                    p.v.add_op1(OP_ClrSubtype, target);
                    return target;
                } else {
                    e = p.e(e).p_left;
                    continue;
                }
            }
            TK_SPAN | TK_UPLUS => {
                e = p.e(e).p_left;
                continue;
            }
            _ => break,
        }
    }
    match op {
        TK_AGG_COLUMN | TK_COLUMN => {
            if op == TK_AGG_COLUMN {
                let agg = p.e(e).p_agg_info;
                let i_agg = p.e(e).i_agg as i32;
                if i_agg >= p.agg(agg).n_column() {
                    // Happens when the left table of a RIGHT JOIN is null and is using an
                    // expression index
                    p.v.add_op2(OP_Null, 0, target);
                    sqlite3_release_temp_reg(p, reg_free1);
                    sqlite3_release_temp_reg(p, reg_free2);
                    return in_reg;
                }
                if p.agg(agg).direct_mode == 0 {
                    return p.agg(agg).column_reg(i_agg);
                } else if p.agg(agg).use_sorting_idx != 0 {
                    let (ptab, i_sorter) = (p.agg(agg).sorting_idx_p_tab, p.agg(agg).a_col[i_agg as usize].i_sorter_column);
                    let col = p.agg(agg).a_col[i_agg as usize].clone();
                    p.v.add_op3(OP_Column, ptab, i_sorter, target);
                    if col.p_tab.nn() && col.i_column >= 0 && p.tab(col.p_tab).a_col[col.i_column as usize].affinity == SQLITE_AFF_REAL {
                        p.v.add_op1(OP_RealAffinity, target);
                    }
                    return target;
                } else if p.e(e).y_tab.is_null() {
                    // This case happens when the argument to an aggregate function is rewritten
                    // by aggregateConvertIndexedExprRefToColumn()
                    let (i_table, i_column) = (p.e(e).i_table, p.e(e).i_column as i32);
                    p.v.add_op3(OP_Column, i_table, i_column, target);
                    return target;
                }
                // Otherwise, fall thru into the TK_COLUMN case
            }
            let i_tab = p.e(e).i_table;
            if p.e(e).has(EP_FixedCol) {
                // This COLUMN expression is really a constant due to WHERE clause constraints,
                // and that constant is coded by the pExpr->pLeft expression.
                let left = p.e(e).p_left;
                let i_reg = sqlite3_expr_code_target(p, left, target);
                let aff = sqlite3_table_column_affinity(p, p.e(e).y_tab, p.e(e).i_column as i32);
                if aff > SQLITE_AFF_BLOB {
                    const Z_AFF: [&str; 5] = ["B", "C", "D", "E", "F"];
                    p.v.add_op4(OP_Affinity, i_reg, 1, 0, P4::Static(Z_AFF[(aff - b'B') as usize]));
                }
                return i_reg;
            }
            let mut i_tab = i_tab;
            if i_tab < 0 {
                if p.i_self_tab < 0 {
                    // Other columns in the same row for CHECK constraints or generated
                    // columns: belongs to DML.
                    p.not_supported("column reference relative to a row under construction (iSelfTab<0)");
                    return target;
                } else {
                    // Coding an expression that is part of an index where column names in
                    // the index refer to the table to which the index belongs
                    i_tab = p.i_self_tab - 1;
                }
            } else if !p.p_idx_part_expr.is_empty() {
                let r1 = expr_partidx_expr_lookup(p, e, target);
                if r1 != 0 {
                    return r1;
                }
            }
            let (tab, i_column, op2) = (p.e(e).y_tab, p.e(e).i_column as i32, p.e(e).op2);
            sqlite3_expr_code_get_column(p, tab, i_column, i_tab, target, op2)
        }
        TK_INTEGER => {
            code_integer(p, e, false, target);
            target
        }
        TK_TRUEFALSE => {
            let v = sqlite3_expr_truth_value(p, e);
            p.v.add_op2(OP_Integer, v, target);
            target
        }
        TK_FLOAT => {
            let z = p.e(e).tok().to_owned();
            code_real(p, &z, false, target);
            target
        }
        TK_STRING => {
            let z = p.e(e).tok().to_owned();
            p.v.load_string(target, &z);
            target
        }
        TK_NULLS => {
            // Set a range of registers to NULL. pExpr->y.nReg registers starting with target
            // are affected.
            let n_reg = p.e(e).y_n_reg;
            p.v.add_op3(OP_Null, 0, target, target + n_reg - 1);
            target
        }
        TK_BLOB => {
            // z = &pExpr->u.zToken[2]; n = strlen(z)-1
            let tok = p.e(e).tok().as_bytes().to_vec();
            let z = &tok[2..];
            let n = z.len() as i32 - 1;
            let blob = sqlite3_hex_to_blob(&z[..n.max(0) as usize]);
            p.v.add_op4(OP_Blob, n / 2, target, 0, P4::Dynamic(blob));
            target
        }
        TK_VARIABLE => {
            let i_column = p.e(e).i_column as i32;
            p.v.add_op2(OP_Variable, i_column, target);
            target
        }
        TK_REGISTER => p.e(e).i_table,
        TK_CAST => {
            // Expressions of the form:   CAST(pLeft AS token)
            let left = p.e(e).p_left;
            sqlite3_expr_code(p, left, target);
            let aff = sqlite3_affinity_type(p.e(e).tok());
            p.v.add_op2(OP_Cast, target, aff as i32);
            in_reg
        }
        TK_IS | TK_ISNOT | TK_LT | TK_LE | TK_GT | TK_GE | TK_NE | TK_EQ => {
            if op == TK_IS || op == TK_ISNOT {
                op = if op == TK_IS { TK_EQ } else { TK_NE };
                p5 = SQLITE_NULLEQ;
            }
            let left = p.e(e).p_left;
            let right = p.e(e).p_right;
            let mut addr_is_null = 0;
            if sqlite3_expr_is_vector(p, left) {
                code_vector_compare(p, e, target, op, p5 as u8);
            } else {
                let (r1, r2);
                if p.e(e).has(EP_Subquery) && p5 != SQLITE_NULLEQ {
                    let (a, x1, x2) = expr_compute_operands(p, e, &mut reg_free1, &mut reg_free2);
                    addr_is_null = a;
                    r1 = x1;
                    r2 = x2;
                } else {
                    r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
                    r2 = sqlite3_expr_code_temp(p, right, &mut reg_free2);
                }
                p.v.add_op2(OP_Integer, 1, in_reg);
                let dest = p.v.current_addr() + 2;
                let commuted = p.e(e).has(EP_Commuted);
                code_compare(p, left, right, op, r1, r2, dest, p5, commuted);
                if p5 == SQLITE_NULLEQ {
                    p.v.add_op2(OP_Integer, 0, in_reg);
                } else {
                    p.v.add_op3(OP_ZeroOrNull, r1, in_reg, r2);
                    if addr_is_null != 0 {
                        let a = p.v.current_addr() + 2;
                        p.v.add_op2(OP_Goto, 0, a);
                        p.v.jump_here(addr_is_null);
                        p.v.add_op2(OP_Null, 0, in_reg);
                    }
                }
            }
            sqlite3_release_temp_reg(p, reg_free1);
            sqlite3_release_temp_reg(p, reg_free2);
            in_reg
        }
        TK_AND | TK_OR => {
            in_reg = expr_code_target_and_or(p, e, target, &mut reg_free1);
            sqlite3_release_temp_reg(p, reg_free1);
            sqlite3_release_temp_reg(p, reg_free2);
            in_reg
        }
        TK_PLUS | TK_STAR | TK_MINUS | TK_REM | TK_BITAND | TK_BITOR | TK_SLASH | TK_LSHIFT | TK_RSHIFT | TK_CONCAT => {
            let (addr_is_null, r1, r2);
            if p.e(e).has(EP_Subquery) {
                let (a, x1, x2) = expr_compute_operands(p, e, &mut reg_free1, &mut reg_free2);
                addr_is_null = a;
                r1 = x1;
                r2 = x2;
            } else {
                let (left, right) = (p.e(e).p_left, p.e(e).p_right);
                r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
                r2 = sqlite3_expr_code_temp(p, right, &mut reg_free2);
                addr_is_null = 0;
            }
            p.v.add_op3(op, r2, r1, target);
            if addr_is_null != 0 {
                let a = p.v.current_addr() + 2;
                p.v.add_op2(OP_Goto, 0, a);
                p.v.jump_here(addr_is_null);
                p.v.add_op2(OP_Null, 0, target);
            }
            sqlite3_release_temp_reg(p, reg_free1);
            sqlite3_release_temp_reg(p, reg_free2);
            in_reg
        }
        TK_UMINUS => {
            let left = p.e(e).p_left;
            if p.e(left).op == TK_INTEGER {
                code_integer(p, left, true, target);
                return target;
            } else if p.e(left).op == TK_FLOAT {
                let z = p.e(left).tok().to_owned();
                code_real(p, &z, true, target);
                return target;
            } else {
                let temp_x = p.new_expr(Expr {
                    op: TK_INTEGER,
                    flags: EP_IntValue | EP_TokenOnly,
                    i_value: 0,
                    ..Expr::default()
                });
                let r1 = sqlite3_expr_code_temp(p, temp_x, &mut reg_free1);
                let r2 = sqlite3_expr_code_temp(p, left, &mut reg_free2);
                p.v.add_op3(OP_Subtract, r2, r1, target);
            }
            sqlite3_release_temp_reg(p, reg_free1);
            sqlite3_release_temp_reg(p, reg_free2);
            in_reg
        }
        TK_BITNOT | TK_NOT => {
            let left = p.e(e).p_left;
            let r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
            p.v.add_op2(op, r1, in_reg);
            sqlite3_release_temp_reg(p, reg_free1);
            sqlite3_release_temp_reg(p, reg_free2);
            in_reg
        }
        TK_TRUTH => {
            // IS TRUE or IS FALSE
            let (left, right) = (p.e(e).p_left, p.e(e).p_right);
            let r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
            let is_true = sqlite3_expr_truth_value(p, right);
            let b_normal = (p.e(e).op2 == TK_IS) as i32;
            p.v.add_op4_int(OP_IsTrue, r1, in_reg, (is_true == 0) as i32, is_true ^ b_normal);
            sqlite3_release_temp_reg(p, reg_free1);
            sqlite3_release_temp_reg(p, reg_free2);
            in_reg
        }
        TK_ISNULL | TK_NOTNULL => {
            p.v.add_op2(OP_Integer, 1, target);
            let left = p.e(e).p_left;
            let r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
            let addr = p.v.add_op1(op, r1);
            p.v.add_op2(OP_Integer, 0, target);
            p.v.jump_here(addr);
            sqlite3_release_temp_reg(p, reg_free1);
            sqlite3_release_temp_reg(p, reg_free2);
            in_reg
        }
        TK_AGG_FUNCTION => {
            let info = p.e(e).p_agg_info;
            let i_agg = p.e(e).i_agg as i32;
            if info.is_null() || i_agg < 0 || i_agg >= p.agg(info).n_func() {
                let msg = format!("misuse of aggregate: {}()", p.e(e).tok());
                p.error_msg_expr(msg, e);
            } else {
                return p.agg(info).func_reg(i_agg);
            }
            in_reg
        }
        TK_FUNCTION => {
            let mut const_mask: u32 = 0;
            // (EP_WinFunc: window functions are not ported in step V1)
            if p.ok_const_factor && sqlite3_expr_is_constant_not_join(p, e) != 0 {
                // SQL functions can be expensive. So try to avoid running them multiple times
                // if we know they always give the same result.
                return sqlite3_expr_code_run_just_once(p, e, -1);
            }
            let farg = p.e(e).x_list();
            let n_farg = if farg.nn() { p.l(farg).n_expr() } else { 0 };
            let z_id = p.e(e).tok().to_owned();
            let def = sqlite3_find_function(p, &z_id, n_farg, 0);
            let def = match def {
                Some(d)
                    if !d.x_finalize
                        && !((d.func_flags & SQLITE_FUNC_INTERNAL) != 0
                            && p.nested == 0
                            && (p.db.m_db_flags & DBFLAG_InternalFunc as u32) == 0) =>
                {
                    d
                }
                _ => {
                    let msg = format!("unknown function: {}()", p.e(e).tok());
                    p.error_msg_expr(msg, e);
                    return in_reg;
                }
            };
            if (def.func_flags & SQLITE_FUNC_INLINE) != 0 && farg.nn() {
                return expr_code_inline_function(p, farg, def.user_data, target);
            } else if def.func_flags & (SQLITE_FUNC_DIRECT | SQLITE_FUNC_UNSAFE) != 0 {
                sqlite3_expr_function_usable(p, e, &def);
            }
            let mut coll: Option<Arc<CollSeq>> = None;
            for i in 0..n_farg {
                let a = p.l(farg).a[i as usize].p_expr;
                if i < 32 && sqlite3_expr_is_constant(p, a) != 0 {
                    const_mask |= 1u32 << i;
                }
                if (def.func_flags & SQLITE_FUNC_NEEDCOLL) != 0 && coll.is_none() {
                    coll = sqlite3_expr_coll_seq(p, a);
                }
            }
            let r1;
            if farg.nn() {
                if const_mask != 0 {
                    r1 = p.n_mem + 1;
                    p.n_mem += n_farg;
                } else {
                    r1 = sqlite3_get_temp_range(p, n_farg);
                }
                // For length() and typeof() and octet_length() functions, set the P5 parameter
                // to the OP_Column opcode to OPFLAG_LENGTHARG or OPFLAG_TYPEOFARG or
                // OPFLAG_BYTELENARG so that the content can be loaded without reading it.
                if (def.func_flags & (SQLITE_FUNC_LENGTH | SQLITE_FUNC_TYPEOF)) != 0 {
                    let a0 = p.l(farg).a[0].p_expr;
                    let expr_op = p.e(a0).op;
                    if expr_op == TK_COLUMN || expr_op == TK_AGG_COLUMN {
                        p.em(a0).op2 = (def.func_flags & OPFLAG_BYTELENARG as u32) as u8;
                    }
                }
                sqlite3_expr_code_expr_list(p, farg, r1, 0, SQLITE_ECEL_FACTOR as u8);
            } else {
                r1 = 0;
            }
            // (sqlite3VtabOverloadFunction: the first argument is never a virtual-table column
            // in step V1)
            if def.func_flags & SQLITE_FUNC_NEEDCOLL != 0 {
                let c = match coll {
                    Some(c) => c,
                    None => dflt_coll(p),
                };
                p.v.add_op4(OP_CollSeq, 0, 0, 0, P4::CollSeq(c));
            }
            let op2 = p.e(e).op2 as i32;
            p.v.add_function_call(const_mask as i32, r1, target, n_farg, def, op2, NC_SelfRef);
            sqlite3_may_abort(p);
            if n_farg != 0 && const_mask == 0 {
                sqlite3_release_temp_range(p, r1, n_farg);
            }
            target
        }
        TK_EXISTS | TK_SELECT => {
            if op == TK_SELECT && p.e(e).use_x_select() && {
                let n_col = p.l(p.s(p.e(e).x_select()).p_e_list).n_expr();
                n_col != 1
            } {
                let n_col = p.l(p.s(p.e(e).x_select()).p_e_list).n_expr();
                sqlite3_subselect_error(p, n_col, 1);
            } else {
                return sqlite3_code_subselect(p, e);
            }
            in_reg
        }
        TK_SELECT_COLUMN => {
            let left = p.e(e).p_left;
            if p.e(left).i_table == 0 || p.within_rj_subrtn > p.e(left).op2 {
                let r = sqlite3_code_subselect(p, left);
                p.em(left).i_table = r;
                p.em(left).op2 = p.within_rj_subrtn;
            }
            let n = sqlite3_expr_vector_size(p, left);
            if p.e(e).i_table != n {
                let msg = format!("{} columns assigned {} values", p.e(e).i_table, n);
                p.error_msg(msg);
            }
            p.e(left).i_table + p.e(e).i_column as i32
        }
        TK_IN => {
            let dest_if_false = p.v.make_label();
            let dest_if_null = p.v.make_label();
            p.v.add_op2(OP_Null, 0, target);
            sqlite3_expr_code_in(p, e, dest_if_false, dest_if_null);
            p.v.add_op2(OP_Integer, 1, target);
            p.v.resolve_label(dest_if_false);
            p.v.add_op2(OP_AddImm, target, 0);
            p.v.resolve_label(dest_if_null);
            target
        }
        // x BETWEEN y AND z
        //
        // This is equivalent to
        //
        //    x>=y AND x<=z
        //
        // X is stored in pExpr->pLeft. Y is stored in pExpr->pList->a[0].pExpr. Z is stored in
        // pExpr->pList->a[1].pExpr.
        TK_BETWEEN => {
            expr_code_between(p, e, target, None, 0);
            target
        }
        TK_TRIGGER => {
            p.not_supported("OLD.* / NEW.* reference of a trigger program");
            in_reg
        }
        TK_VECTOR => {
            p.error_msg("row value misused");
            in_reg
        }
        // TK_IF_NULL_ROW Expr nodes are inserted ahead of expressions that derive from the
        // right-hand table of a LEFT JOIN (flattened subqueries, where.c).
        TK_IF_NULL_ROW => {
            let ok_const_factor = p.ok_const_factor;
            let agg = p.e(e).p_agg_info;
            if agg.nn() {
                let i_agg = p.e(e).i_agg as i32;
                if p.agg(agg).direct_mode == 0 {
                    return p.agg(agg).column_reg(i_agg);
                }
                if p.agg(agg).use_sorting_idx != 0 {
                    let (ptab, i_sorter) = (p.agg(agg).sorting_idx_p_tab, p.agg(agg).a_col[i_agg as usize].i_sorter_column);
                    p.v.add_op3(OP_Column, ptab, i_sorter, target);
                    return target;
                }
            }
            let i_table = p.e(e).i_table;
            let addr_inr = p.v.add_op3(OP_IfNullRow, i_table, 0, target);
            // The OP_IfNullRow opcode above can overwrite the result register with NULL. So
            // we must ensure that the result register is not one that is used for
            // constant-factored expressions. Disable factoring of constants within the
            // IF_NULL_ROW.
            p.ok_const_factor = false;
            let left = p.e(e).p_left;
            sqlite3_expr_code(p, left, target);
            p.ok_const_factor = ok_const_factor;
            p.v.jump_here(addr_inr);
            in_reg
        }
        // Form A:
        //   CASE x WHEN e1 THEN r1 WHEN e2 THEN r2 ... WHEN eN THEN rN ELSE y END
        //
        // Form B:
        //   CASE WHEN e1 THEN r1 WHEN e2 THEN r2 ... WHEN eN THEN rN ELSE y END
        //
        // Form A can be transformed into the equivalent form B as follows:
        //   CASE WHEN x=e1 THEN r1 WHEN x=e2 THEN r2 ...
        //        WHEN x=eN THEN rN ELSE y END
        //
        // X (if it exists) is in pExpr->pLeft. Y is in the last element of pExpr->x.pList if
        // pExpr->x.pList->nExpr is odd. Ei is in pExpr->pList->a[i*2] and Ri is
        // pExpr->pList->a[i*2+1].
        TK_CASE => {
            let e_list = p.e(e).x_list();
            let n_expr = p.l(e_list).a.len();
            let end_label = p.v.make_label();
            let px = p.e(e).p_left;
            let mut p_test = ExprId::NULL;
            let mut op_compare = ExprId::NULL;
            if px.nn() {
                let p_del = sqlite3_expr_dup(p, px, 0);
                let r = expr_code_vector(p, p_del, &mut reg_free1);
                sqlite3_expr_to_register(p, p_del, r);
                op_compare = p.new_expr(Expr { op: TK_EQ, p_left: p_del, ..Expr::default() });
                p_test = op_compare;
                // Ticket b351d95f9cd5ef17e9d9dbae18f5ca8611190001: The value in regFree1
                // might get SCopy-ed into the result. So make sure not to reuse it.
                reg_free1 = 0;
            }
            let mut i = 0;
            while i + 1 < n_expr {
                let ai = p.l(e_list).a[i].p_expr;
                if px.nn() {
                    p.em(op_compare).p_right = ai;
                } else {
                    p_test = ai;
                }
                let next_case = p.v.make_label();
                sqlite3_expr_if_false(p, p_test, next_case, SQLITE_JUMPIFNULL);
                let ar = p.l(e_list).a[i + 1].p_expr;
                sqlite3_expr_code(p, ar, target);
                p.v.goto(end_label);
                p.v.resolve_label(next_case);
                i += 2;
            }
            if (n_expr & 1) != 0 {
                let last = p.l(e_list).a[n_expr - 1].p_expr;
                sqlite3_expr_code(p, last, target);
            } else {
                p.v.add_op2(OP_Null, 0, target);
            }
            set_do_not_merge_flag_on_copy(p);
            p.v.resolve_label(end_label);
            sqlite3_release_temp_reg(p, reg_free1);
            sqlite3_release_temp_reg(p, reg_free2);
            in_reg
        }
        TK_RAISE => {
            if p.p_trigger_tab.is_null() && p.nested == 0 {
                p.error_msg("RAISE() may only be used within a trigger-program");
                return 0;
            }
            p.not_supported("RAISE() inside a trigger program");
            in_reg
        }
        _ => {
            // Make NULL the default case so that if a bug causes an illegal Expr node to be
            // passed into this function, it will be handled sanely and not crash.
            p.v.add_op2(OP_Null, 0, target);
            target
        }
    }
}

/// `sqlite3ExprCodeRunJustOnce(pParse, pExpr, regDest)`: generate code that will evaluate
/// expression `e` just one time per prepared statement execution.
///
/// If the expression uses functions (that might throw an exception) then guard them with an
/// OP_Once opcode to ensure that the code is only executed once. If no functions are
/// involved, then factor the code out and put it at the end of the prepared statement in the
/// initialization section.
///
/// If `reg_dest>0` then the result is always stored in that register and the result is not
/// reusable. If `reg_dest<0` then this routine is free to store the value wherever it wants;
/// the register where the expression is stored is returned.
pub fn sqlite3_expr_code_run_just_once(p: &mut Parse, e: ExprId, mut reg_dest: i32) -> i32 {
    let list = p.p_const_expr;
    if reg_dest < 0 && list.nn() {
        for i in 0..p.l(list).a.len() {
            let (reusable, ie, reg) = {
                let it = &p.l(list).a[i];
                (it.reusable, it.p_expr, it.i_const_expr_reg)
            };
            if reusable && sqlite3_expr_compare(p, false, ie, e, -1) == 0 {
                return reg;
            }
        }
    }
    let e = sqlite3_expr_dup(p, e, 0);
    if e.nn() && p.e(e).has(EP_HasFunc) {
        let addr = p.v.add_op0(OP_Once);
        p.ok_const_factor = false;
        if reg_dest < 0 {
            p.n_mem += 1;
            reg_dest = p.n_mem;
        }
        sqlite3_expr_code(p, e, reg_dest);
        p.ok_const_factor = true;
        p.v.jump_here(addr);
    } else {
        let list = sqlite3_expr_list_append(p, list, e);
        {
            let reusable = reg_dest < 0;
            if reg_dest < 0 {
                p.n_mem += 1;
                reg_dest = p.n_mem;
            }
            let item = p.lm(list).a.last_mut().expect("item");
            item.reusable = reusable;
            item.i_const_expr_reg = reg_dest;
        }
        p.p_const_expr = list;
    }
    reg_dest
}

/// `sqlite3ExprNullRegisterRange(pParse, iReg, nReg)`: make arrangements to invoke OP_Null on
/// a range of registers during initialization.
pub fn sqlite3_expr_null_register_range(p: &mut Parse, i_reg: i32, n_reg: i32) {
    let ok_const_factor = p.ok_const_factor;
    let t = p.new_expr(Expr { op: TK_NULLS, y_n_reg: n_reg, ..Expr::default() });
    p.ok_const_factor = true;
    sqlite3_expr_code_run_just_once(p, t, i_reg);
    p.ok_const_factor = ok_const_factor;
}

/// `sqlite3ExprCodeTemp(pParse, pExpr, pReg)`: generate code to evaluate an expression and
/// store the results into a register. Return the register number where the results are
/// stored.
///
/// If the register is a temporary register that needs to be deallocated, then write its
/// number into `*p_reg`. If the result register is not a temporary register then write zero
/// into `*p_reg`.
///
/// If `e` is a constant, then this routine might generate this code to fill the register in
/// the initialization section of the VDBE program so that it is constant for the life of the
/// statement.
pub fn sqlite3_expr_code_temp(p: &mut Parse, e: ExprId, p_reg: &mut i32) -> i32 {
    let r2;
    let e = sqlite3_expr_skip_collate_and_likely(p, e);
    if p.ok_const_factor && e.nn() && p.e(e).op != TK_REGISTER && sqlite3_expr_is_constant_not_join(p, e) != 0 {
        *p_reg = 0;
        r2 = sqlite3_expr_code_run_just_once(p, e, -1);
    } else {
        let r1 = sqlite3_get_temp_reg(p);
        r2 = sqlite3_expr_code_target(p, e, r1);
        if r2 == r1 {
            *p_reg = r1;
        } else {
            sqlite3_release_temp_reg(p, r1);
            *p_reg = 0;
        }
    }
    r2
}

/// `sqlite3ExprCode(pParse, pExpr, target)`: generate code that will evaluate expression `e`
/// and store the results in register `target`. The results are guaranteed to appear in
/// register `target`.
pub fn sqlite3_expr_code(p: &mut Parse, e: ExprId, target: i32) {
    if !p.has_vdbe {
        return;
    }
    let in_reg = sqlite3_expr_code_target(p, e, target);
    if in_reg != target {
        let x = sqlite3_expr_skip_collate_and_likely(p, e);
        let op = if x.nn() && (p.e(x).has(EP_Subquery) || p.e(x).op == TK_REGISTER) { OP_Copy } else { OP_SCopy };
        p.v.add_op2(op, in_reg, target);
    }
}

/// `sqlite3ExprCodeCopy(pParse, pExpr, target)`: make a transient copy of expression `e` and
/// then code using sqlite3ExprCode(). This routine works just like sqlite3ExprCode() except
/// that the input expression is guaranteed to be unchanged.
pub fn sqlite3_expr_code_copy(p: &mut Parse, e: ExprId, target: i32) {
    let c = sqlite3_expr_dup(p, e, 0);
    sqlite3_expr_code(p, c, target);
}

/// `sqlite3ExprCodeFactorable(pParse, pExpr, target)`: generate code that will evaluate
/// expression `e` and store the results in register `target`. If the expression is constant,
/// then this routine might choose to code the expression at initialization time.
pub fn sqlite3_expr_code_factorable(p: &mut Parse, e: ExprId, target: i32) {
    if p.ok_const_factor && sqlite3_expr_is_constant_not_join(p, e) != 0 {
        sqlite3_expr_code_run_just_once(p, e, target);
    } else {
        sqlite3_expr_code_copy(p, e, target);
    }
}

/// `sqlite3ExprCodeExprList(pParse, pList, target, srcReg, flags)`: generate code that pushes
/// the value of every element of the given expression list into a sequence of registers
/// beginning at `target`. Return the number of elements evaluated.
///
/// The `SQLITE_ECEL_DUP` flag prevents the arguments from being filled using OP_SCopy
/// (OP_Copy must be used instead). The `SQLITE_ECEL_FACTOR` argument allows constant
/// expressions to be factored out into initialization code. The `SQLITE_ECEL_REF` flag means
/// that expressions in the list with a non-zero `iOrderByCol` are not evaluated but copied
/// from register `src_reg + iOrderByCol - 1`; with `SQLITE_ECEL_OMITREF` they are omitted.
pub fn sqlite3_expr_code_expr_list(p: &mut Parse, list: ExprListId, target: i32, src_reg: i32, mut flags: u8) -> i32 {
    let copy_op = if flags & SQLITE_ECEL_DUP as u8 != 0 { OP_Copy } else { OP_SCopy };
    let mut n = p.l(list).n_expr();
    if !p.ok_const_factor {
        flags &= !(SQLITE_ECEL_FACTOR as u8);
    }
    let cnt = p.l(list).a.len();
    let mut i: i32 = 0;
    for idx in 0..cnt {
        let (e, j) = {
            let it = &p.l(list).a[idx];
            (it.p_expr, it.i_order_by_col as i32)
        };
        if (flags & SQLITE_ECEL_REF as u8) != 0 && j > 0 {
            if flags & SQLITE_ECEL_OMITREF as u8 != 0 {
                i -= 1;
                n -= 1;
            } else {
                p.v.add_op2(copy_op, j + src_reg - 1, target + i);
            }
        } else if (flags & SQLITE_ECEL_FACTOR as u8) != 0 && sqlite3_expr_is_constant_not_join(p, e) != 0 {
            sqlite3_expr_code_run_just_once(p, e, target + i);
        } else {
            let in_reg = sqlite3_expr_code_target(p, e, target + i);
            if in_reg != target + i {
                let merged = if copy_op == OP_Copy && !p.v.a_op.is_empty() {
                    let op = p.v.get_last_op_mut();
                    if op.opcode == OP_Copy && op.p1 + op.p3 + 1 == in_reg && op.p2 + op.p3 + 1 == target + i && op.p5 == 0 {
                        op.p3 += 1;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !merged {
                    p.v.add_op2(copy_op, in_reg, target + i);
                }
            }
        }
        i += 1;
    }
    n
}

/// `exprCodeBetween(pParse, pExpr, dest, xJump, jumpIfNull)`: generate code for a BETWEEN
/// operator `x BETWEEN y AND z`, implemented as `x>=y AND x<=z` with x evaluated only once.
///
/// If `x_jump` is `sqlite3ExprIfTrue` then jump to `dest` if true; if it is
/// `sqlite3ExprIfFalse` then jump to `dest` if false; if it is `None` then store the result
/// in register `dest`.
fn expr_code_between(
    p: &mut Parse,
    e: ExprId,
    dest: i32,
    x_jump: Option<fn(&mut Parse, ExprId, i32, i32)>,
    jump_if_null: i32,
) {
    let mut reg_free1 = 0;
    let left = p.e(e).p_left;
    let p_del = sqlite3_expr_dup(p, left, 0);
    let list = p.e(e).x_list();
    let (lo, hi) = (p.l(list).a[0].p_expr, p.l(list).a[1].p_expr);
    let comp_left = p.new_expr(Expr { op: TK_GE, p_left: p_del, p_right: lo, ..Expr::default() });
    let comp_right = p.new_expr(Expr { op: TK_LE, p_left: p_del, p_right: hi, ..Expr::default() });
    let expr_and = p.new_expr(Expr { op: TK_AND, p_left: comp_left, p_right: comp_right, ..Expr::default() });
    let r = expr_code_vector(p, p_del, &mut reg_free1);
    sqlite3_expr_to_register(p, p_del, r);
    match x_jump {
        Some(f) => f(p, expr_and, dest, jump_if_null),
        None => {
            // Mark the expression is being from the ON or USING clause of a join so that the
            // sqlite3ExprCodeTarget() routine will not attempt to move it into register
            // initialization code.
            p.em(p_del).flags |= EP_OuterON;
            sqlite3_expr_code_target(p, expr_and, dest);
        }
    }
    sqlite3_release_temp_reg(p, reg_free1);
}

/// `sqlite3ExprIfTrue(pParse, pExpr, dest, jumpIfNull)`: generate code for a boolean
/// expression such that a jump is made to the label `dest` if the expression is true but
/// execution continues straight thru if the expression is false.
///
/// If the expression evaluates to NULL (neither true nor false), then take the jump if the
/// `jump_if_null` flag is SQLITE_JUMPIFNULL.
pub fn sqlite3_expr_if_true(p: &mut Parse, e: ExprId, dest: i32, mut jump_if_null: i32) {
    let mut reg_free1 = 0;
    let mut reg_free2 = 0;
    if e.is_null() {
        return;
    }
    let mut op = p.e(e).op;
    let mut default_expr = false;
    match op {
        TK_AND | TK_OR => {
            let alt = sqlite3_expr_simplified_and_or(p, e);
            if alt != e {
                sqlite3_expr_if_true(p, alt, dest, jump_if_null);
            } else {
                let (first, second) = if expr_eval_rhs_first(p, e) {
                    (p.e(e).p_right, p.e(e).p_left)
                } else {
                    (p.e(e).p_left, p.e(e).p_right)
                };
                if op == TK_AND {
                    let d2 = p.v.make_label();
                    sqlite3_expr_if_false(p, first, d2, jump_if_null ^ SQLITE_JUMPIFNULL);
                    sqlite3_expr_if_true(p, second, dest, jump_if_null);
                    p.v.resolve_label(d2);
                } else {
                    sqlite3_expr_if_true(p, first, dest, jump_if_null);
                    sqlite3_expr_if_true(p, second, dest, jump_if_null);
                }
            }
        }
        TK_NOT => {
            let left = p.e(e).p_left;
            sqlite3_expr_if_false(p, left, dest, jump_if_null);
        }
        TK_TRUTH => {
            let is_not = p.e(e).op2 == TK_ISNOT;
            let is_true = sqlite3_expr_truth_value(p, p.e(e).p_right) != 0;
            let left = p.e(e).p_left;
            if is_true ^ is_not {
                sqlite3_expr_if_true(p, left, dest, if is_not { SQLITE_JUMPIFNULL } else { 0 });
            } else {
                sqlite3_expr_if_false(p, left, dest, if is_not { SQLITE_JUMPIFNULL } else { 0 });
            }
        }
        TK_IS | TK_ISNOT | TK_LT | TK_LE | TK_GT | TK_GE | TK_NE | TK_EQ => {
            if op == TK_IS || op == TK_ISNOT {
                op = if op == TK_IS { TK_EQ } else { TK_NE };
                jump_if_null = SQLITE_NULLEQ;
            }
            let (left, right) = (p.e(e).p_left, p.e(e).p_right);
            if sqlite3_expr_is_vector(p, left) {
                default_expr = true;
            } else {
                let (addr_is_null, r1, r2);
                if p.e(e).has(EP_Subquery) && jump_if_null != SQLITE_NULLEQ {
                    let (a, x1, x2) = expr_compute_operands(p, e, &mut reg_free1, &mut reg_free2);
                    addr_is_null = a;
                    r1 = x1;
                    r2 = x2;
                } else {
                    r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
                    r2 = sqlite3_expr_code_temp(p, right, &mut reg_free2);
                    addr_is_null = 0;
                }
                let commuted = p.e(e).has(EP_Commuted);
                code_compare(p, left, right, op, r1, r2, dest, jump_if_null, commuted);
                if addr_is_null != 0 {
                    if jump_if_null != 0 {
                        p.v.change_p2(addr_is_null, dest);
                    } else {
                        p.v.jump_here(addr_is_null);
                    }
                }
            }
        }
        TK_ISNULL | TK_NOTNULL => {
            let left = p.e(e).p_left;
            let r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
            if reg_free1 != 0 {
                p.v.typeof_column(r1, OPFLAG_TYPEOFARG as u16);
            }
            p.v.add_op2(op, r1, dest);
        }
        TK_BETWEEN => {
            expr_code_between(p, e, dest, Some(sqlite3_expr_if_true), jump_if_null);
        }
        TK_IN => {
            let dest_if_false = p.v.make_label();
            let dest_if_null = if jump_if_null != 0 { dest } else { dest_if_false };
            sqlite3_expr_code_in(p, e, dest_if_false, dest_if_null);
            p.v.goto(dest);
            p.v.resolve_label(dest_if_false);
        }
        _ => default_expr = true,
    }
    if default_expr {
        if p.e(e).always_true() {
            p.v.goto(dest);
        } else if p.e(e).always_false() {
            // No-op
        } else {
            let r1 = sqlite3_expr_code_temp(p, e, &mut reg_free1);
            p.v.add_op3(OP_If, r1, dest, (jump_if_null != 0) as i32);
        }
    }
    sqlite3_release_temp_reg(p, reg_free1);
    sqlite3_release_temp_reg(p, reg_free2);
}

/// `sqlite3ExprIfFalse(pParse, pExpr, dest, jumpIfNull)`: generate code for a boolean
/// expression such that a jump is made to the label `dest` if the expression is false but
/// execution continues straight thru if the expression is true.
///
/// If the expression evaluates to NULL (neither true nor false) then jump if `jump_if_null`
/// is SQLITE_JUMPIFNULL or fall through if `jump_if_null` is 0.
pub fn sqlite3_expr_if_false(p: &mut Parse, e: ExprId, dest: i32, mut jump_if_null: i32) {
    let mut reg_free1 = 0;
    let mut reg_free2 = 0;
    if e.is_null() {
        return;
    }
    // The value of pExpr->op and op are related as follows:
    //
    //       pExpr->op            op
    //       ---------          ----------
    //       TK_ISNULL          OP_NotNull
    //       TK_NOTNULL         OP_IsNull
    //       TK_NE              OP_Eq
    //       TK_EQ              OP_Ne
    //       TK_GT              OP_Le
    //       TK_LE              OP_Gt
    //       TK_GE              OP_Lt
    //       TK_LT              OP_Ge
    //
    // For other values of pExpr->op, op is undefined and unused.
    let eop = p.e(e).op;
    let mut op = (((eop as i32 + (TK_ISNULL as i32 & 1)) ^ 1) - (TK_ISNULL as i32 & 1)) as u8;
    let mut default_expr = false;
    match eop {
        TK_AND | TK_OR => {
            let alt = sqlite3_expr_simplified_and_or(p, e);
            if alt != e {
                sqlite3_expr_if_false(p, alt, dest, jump_if_null);
            } else {
                let (first, second) = if expr_eval_rhs_first(p, e) {
                    (p.e(e).p_right, p.e(e).p_left)
                } else {
                    (p.e(e).p_left, p.e(e).p_right)
                };
                if eop == TK_AND {
                    sqlite3_expr_if_false(p, first, dest, jump_if_null);
                    sqlite3_expr_if_false(p, second, dest, jump_if_null);
                } else {
                    let d2 = p.v.make_label();
                    sqlite3_expr_if_true(p, first, d2, jump_if_null ^ SQLITE_JUMPIFNULL);
                    sqlite3_expr_if_false(p, second, dest, jump_if_null);
                    p.v.resolve_label(d2);
                }
            }
        }
        TK_NOT => {
            let left = p.e(e).p_left;
            sqlite3_expr_if_true(p, left, dest, jump_if_null);
        }
        TK_TRUTH => {
            let is_not = p.e(e).op2 == TK_ISNOT;
            let is_true = sqlite3_expr_truth_value(p, p.e(e).p_right) != 0;
            let left = p.e(e).p_left;
            if is_true ^ is_not {
                // IS TRUE and IS NOT FALSE
                sqlite3_expr_if_false(p, left, dest, if is_not { 0 } else { SQLITE_JUMPIFNULL });
            } else {
                // IS FALSE and IS NOT TRUE
                sqlite3_expr_if_true(p, left, dest, if is_not { 0 } else { SQLITE_JUMPIFNULL });
            }
        }
        TK_IS | TK_ISNOT | TK_LT | TK_LE | TK_GT | TK_GE | TK_NE | TK_EQ => {
            if eop == TK_IS || eop == TK_ISNOT {
                op = if eop == TK_IS { TK_NE } else { TK_EQ };
                jump_if_null = SQLITE_NULLEQ;
            }
            let (left, right) = (p.e(e).p_left, p.e(e).p_right);
            if sqlite3_expr_is_vector(p, left) {
                default_expr = true;
            } else {
                let (addr_is_null, r1, r2);
                if p.e(e).has(EP_Subquery) && jump_if_null != SQLITE_NULLEQ {
                    let (a, x1, x2) = expr_compute_operands(p, e, &mut reg_free1, &mut reg_free2);
                    addr_is_null = a;
                    r1 = x1;
                    r2 = x2;
                } else {
                    r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
                    r2 = sqlite3_expr_code_temp(p, right, &mut reg_free2);
                    addr_is_null = 0;
                }
                let commuted = p.e(e).has(EP_Commuted);
                code_compare(p, left, right, op, r1, r2, dest, jump_if_null, commuted);
                if addr_is_null != 0 {
                    if jump_if_null != 0 {
                        p.v.change_p2(addr_is_null, dest);
                    } else {
                        p.v.jump_here(addr_is_null);
                    }
                }
            }
        }
        TK_ISNULL | TK_NOTNULL => {
            let left = p.e(e).p_left;
            let r1 = sqlite3_expr_code_temp(p, left, &mut reg_free1);
            if reg_free1 != 0 {
                p.v.typeof_column(r1, OPFLAG_TYPEOFARG as u16);
            }
            p.v.add_op2(op, r1, dest);
        }
        TK_BETWEEN => {
            expr_code_between(p, e, dest, Some(sqlite3_expr_if_false), jump_if_null);
        }
        TK_IN => {
            if jump_if_null != 0 {
                sqlite3_expr_code_in(p, e, dest, dest);
            } else {
                let dest_if_null = p.v.make_label();
                sqlite3_expr_code_in(p, e, dest, dest_if_null);
                p.v.resolve_label(dest_if_null);
            }
        }
        _ => default_expr = true,
    }
    if default_expr {
        if p.e(e).always_false() {
            p.v.goto(dest);
        } else if p.e(e).always_true() {
            // no-op
        } else {
            let r1 = sqlite3_expr_code_temp(p, e, &mut reg_free1);
            p.v.add_op3(OP_IfNot, r1, dest, (jump_if_null != 0) as i32);
        }
    }
    sqlite3_release_temp_reg(p, reg_free1);
    sqlite3_release_temp_reg(p, reg_free2);
}

/// `sqlite3ExprIfFalseDup(pParse, pExpr, dest, jumpIfNull)`: like sqlite3ExprIfFalse() except
/// that a copy is made of `e` before code generation, and that copy is deleted after code
/// generation. This ensures that the original `e` is unchanged.
pub fn sqlite3_expr_if_false_dup(p: &mut Parse, e: ExprId, dest: i32, jump_if_null: i32) {
    let c = sqlite3_expr_dup(p, e, 0);
    sqlite3_expr_if_false(p, c, dest, jump_if_null);
}
