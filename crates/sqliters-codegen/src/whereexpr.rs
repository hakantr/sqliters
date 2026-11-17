//! `whereexpr.c`: this module contains C code that generates VDBE code used to process the
//! WHERE clause of SQL statements. This file was originally part of where.c but was split
//! out to improve readability and editability. This file contains utility routines for
//! analyzing Expr objects in the WHERE clause.

use std::sync::Arc;

use sqliters_vdbe::vdbeaux::CollSeq;

use crate::callback::sqlite3_find_function;
use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::util::is_numeric_affinity;
use crate::where_::sqlite3_where_get_mask;
use crate::whereint::*;

/// `whereClauseInsert(pWC, p, wtFlags)`: add a single new WhereTerm entry to the WhereClause
/// object pWC. The new WhereTerm object is constructed from Expr p and with wtFlags. The
/// index in pWC->a[] of the new WhereTerm is returned on success.
///
/// This routine will increase the size of the pWC->a[] array as necessary.
///
/// If the wtFlags argument includes TERM_DYNAMIC, then responsibility for freeing the
/// expression p is assumed by the WhereClause object pWC. This is true even if this routine
/// fails to allocate a new WhereTerm.
///
/// WARNING: This routine might reallocate the space used to store WhereTerms. All pointers
/// to WhereTerms should be invalidated after calling this routine. Such pointers may be
/// reinitialized by referencing the pWC->a[] array.
pub fn where_clause_insert(p: &Parse, w: &mut WhereInfo, wc: WcId, e: ExprId, wt_flags: u16) -> usize {
    let truth_prob = if e.nn() && p.e(e).has(EP_Unlikely) {
        crate::select::sqlite3_log_est(p.e(e).i_table as u64) - 270
    } else {
        1
    };
    let term = WhereTerm {
        p_expr: sqlite3_expr_skip_collate_and_likely(p, e),
        truth_prob,
        wt_flags,
        // pTerm->iParent = -1, then memset(&pTerm->eOperator, 0, ...) clears it again
        // (iParent follows eOperator in struct WhereTerm): a term that exprAnalyze() never
        // sees, such as a LIMIT / OFFSET term of a virtual table, keeps iParent 0.
        i_parent: 0,
        ..WhereTerm::default()
    };
    let c = &mut w.a_wc[wc];
    c.a.push(term);
    let idx = c.a.len() - 1;
    if (wt_flags & TERM_VIRTUAL) == 0 {
        c.n_base = c.a.len() as i32;
    }
    idx
}

/// `allowedOp(op)`: return TRUE if the given operator is one of the operators that is allowed
/// for an indexable WHERE clause term. The allowed operators are "=", "<", ">", "<=", ">=",
/// "IN", "IS", and "IS NULL"
pub fn allowed_op(op: u8) -> bool {
    if op > TK_GE {
        return false;
    }
    if op >= TK_EQ {
        return true;
    }
    op == TK_IN || op == TK_ISNULL || op == TK_IS
}

/// Pointer equality of two `CollSeq *`.
pub fn coll_ptr_eq(a: &Option<Arc<CollSeq>>, b: &Option<Arc<CollSeq>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => Arc::ptr_eq(x, y) || (x.enc == y.enc && x.name.eq_ignore_ascii_case(&y.name)),
        _ => false,
    }
}

/// `exprCommute(pParse, pExpr)`: commute a comparison operator. Expressions of the form
/// "X op Y" are converted into "Y op X".
fn expr_commute(p: &mut Parse, e: ExprId) -> u16 {
    let (l, r) = (p.e(e).p_left, p.e(e).p_right);
    let flip = p.e(l).op == TK_VECTOR || p.e(r).op == TK_VECTOR || {
        let a = sqlite3_binary_compare_coll_seq(p, l, r);
        let b = sqlite3_binary_compare_coll_seq(p, r, l);
        !coll_ptr_eq(&a, &b)
    };
    let x = p.em(e);
    if flip {
        x.flags ^= EP_Commuted;
    }
    std::mem::swap(&mut x.p_right, &mut x.p_left);
    if x.op >= TK_GT {
        x.op = ((x.op - TK_GT) ^ 2) + TK_GT;
    }
    0
}

/// `operatorMask(op)`: translate from TK_xx operator to WO_xx bitmask.
fn operator_mask(op: u8) -> u16 {
    if op >= TK_EQ {
        WO_EQ << (op - TK_EQ)
    } else if op == TK_IN {
        WO_IN
    } else if op == TK_ISNULL {
        WO_ISNULL
    } else {
        WO_IS
    }
}

/// The validity result of `sqlite3AtoF(z, &r)`: true if the text is a number with nothing
/// but white space after it (the function returns a positive value).
#[allow(unused_assignments)] // (mState is tracked as in C)
fn atof_is_num(z: &[u8]) -> bool {
    let at = |i: usize| -> u8 { if i < z.len() { z[i] } else { 0 } };
    let is_space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let mut i = 0;
    let mut m_state = 0;
    loop {
        // start_of_text:
        if at(i).is_ascii_digit() {
            m_state = 1;
        } else if at(i) == b'-' || at(i) == b'+' {
            i += 1;
            if at(i).is_ascii_digit() {
                m_state = 1;
            }
        } else if is_space(at(i)) {
            while is_space(at(i)) {
                i += 1;
            }
            continue;
        }
        break;
    }
    while at(i).is_ascii_digit() {
        i += 1;
    }
    // if decimal point is present
    if at(i) == b'.' {
        i += 1;
        if at(i).is_ascii_digit() {
            m_state |= 1;
            while at(i).is_ascii_digit() {
                i += 1;
            }
        } else if m_state == 0 {
            return false;
        }
        m_state |= 2;
    } else if m_state == 0 {
        return false;
    }
    // if exponent is present
    if at(i) == b'e' || at(i) == b'E' {
        let save = i;
        i += 1;
        let mut signed = false;
        if at(i) == b'-' || at(i) == b'+' {
            i += 1;
            signed = true;
        }
        if at(i).is_ascii_digit() {
            while at(i).is_ascii_digit() {
                i += 1;
            }
        } else {
            // Leave z[0] at 'e' or '+' or '-', so that the return is 0 or -1
            i = if signed { save + 1 } else { save };
        }
    }
    // return true if number and no extra non-whitespace characters after
    if at(i) == 0 && i >= z.len() {
        return true;
    }
    if is_space(at(i)) {
        while is_space(at(i)) {
            i += 1;
        }
        if i >= z.len() {
            return true;
        }
    }
    false
}

/// `sqlite3IsLikeFunction(db, pExpr, pIsNocase, aWc)`: pExpr points to an expression which
/// implements a function. If it is appropriate to apply the LIKE optimization to that
/// function then set aWc[0] through aWc[2] to the wildcard characters and the escape
/// character and then return TRUE. If the function is not a LIKE-style function then return
/// FALSE.
///
/// The expression "a LIKE b ESCAPE c" is only considered a valid LIKE operator if c is a
/// string literal that is exactly one byte in length. That one byte is stored in aWc[3].
/// aWc[3] is set to zero if there is no ESCAPE clause.
///
/// *pIsNocase is set to true if uppercase and lowercase are equivalent for the function and
/// false if the function is case sensitive. An example of where this might be false is
/// LIKE when PRAGMA case_sensitive_like is turned on, or GLOB.
pub fn sqlite3_is_like_function(p: &mut Parse, e: ExprId, is_nocase: &mut bool, a_wc: &mut [u8; 4]) -> bool {
    let list = p.e(e).x_list();
    if list.is_null() {
        return false;
    }
    let n_expr = p.l(list).n_expr();
    let name = p.e(e).tok().to_owned();
    let Some(def) = sqlite3_find_function(p, &name, n_expr, 0) else {
        return false;
    };
    if (def.func_flags & SQLITE_FUNC_LIKE) == 0 {
        return false;
    }
    // memcpy(aWc, pDef->pUserData, 3): the compareInfo of the function — globInfo for GLOB,
    // likeInfoNorm / likeInfoAlt for LIKE
    if def.name.eq_ignore_ascii_case("glob") {
        a_wc[..3].copy_from_slice(b"*?[");
    } else {
        a_wc[..3].copy_from_slice(&[b'%', b'_', 0]);
    }
    if n_expr < 3 {
        a_wc[3] = 0;
    } else {
        let esc = p.l(list).a[2].p_expr;
        if p.e(esc).op != TK_STRING {
            return false;
        }
        let z = p.e(esc).tok().as_bytes();
        if z.len() != 1 {
            return false;
        }
        if z[0] == a_wc[0] {
            return false;
        }
        if z[0] == a_wc[1] {
            return false;
        }
        a_wc[3] = z[0];
    }
    *is_nocase = (def.func_flags & SQLITE_FUNC_CASE) == 0;
    true
}

/// `sqlite3Utf8Read()` as used by `isLikeOrGlob`: decode the character that starts at `z[i]`;
/// returns (code point, index of the byte after it).
fn utf8_read(z: &[u8], mut i: usize) -> (u32, usize) {
    const TRANS1: [u8; 64] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12,
        0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
        0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x00,
        0x01, 0x02, 0x03, 0x00, 0x01, 0x00, 0x00,
    ];
    let at = |i: usize| -> u8 { if i < z.len() { z[i] } else { 0 } };
    let mut c = at(i) as u32;
    i += 1;
    if c >= 0xc0 {
        c = TRANS1[(c - 0xc0) as usize] as u32;
        while (at(i) & 0xc0) == 0x80 {
            c = (c << 6) + (0x3f & at(i)) as u32;
            i += 1;
        }
        if c < 0x80 || (c & 0xFFFF_F800) == 0xD800 || (c & 0xFFFF_FFFE) == 0xFFFE {
            c = 0xFFFD;
        }
    }
    (c, i)
}

/// `isLikeOrGlob(pParse, pExpr, ppPrefix, pisComplete, pnoCase)`: check to see if the given
/// expression is a LIKE or GLOB operator that can be optimized using inequality constraints.
/// Return TRUE if it is so and false if not.
///
/// In order for the operator to be optimizible, the RHS must be a string literal that does
/// not begin with a wildcard. The LHS must be a column that may only be NULL, a string, or a
/// BLOB, never a number. (This means that virtual tables cannot participate in the LIKE
/// optimization.) The collating sequence for the column on the LHS must be appropriate for
/// the operator.
fn is_like_or_glob(p: &mut Parse, e: ExprId, pp_prefix: &mut ExprId, pis_complete: &mut bool, pno_case: &mut bool) -> bool {
    let mut wc = [0u8; 4];
    if !sqlite3_is_like_function(p, e, pno_case, &mut wc) {
        return false;
    }
    let list = p.e(e).x_list();
    let p_left = p.l(list).a[1].p_expr;
    let p_right = sqlite3_expr_skip_collate(p, p.l(list).a[0].p_expr);
    let op = p.e(p_right).op;
    let mut z: Option<Vec<u8>> = None;
    if op == TK_VARIABLE && (p.db.flags & SQLITE_EnableQPSG) == 0 {
        let i_col = p.e(p_right).i_column as i32;
        // whereexpr.c:207–218: BLOB affinity leaves the storage class unchanged.
        // Only a bound TEXT supplies the prefix; NULL/BLOB/numeric values do not.
        if let Some(sqliters_types::Value::Text(text)) = p.db.host.bound_value(i_col as usize) {
            z = Some(text.into_bytes());
        }
        p.v.reprepare_on_bind(i_col);
    } else if op == TK_STRING {
        z = Some(p.e(p_right).tok().as_bytes().to_vec());
    }
    let Some(zb) = z else {
        return false;
    };
    let at = |i: usize| -> u8 { if i < zb.len() { zb[i] } else { 0 } };
    // Count the number of prefix bytes prior to the first wildcard, U+fffd character, or
    // malformed utf-8. If the underlying database has a UTF16LE encoding, then only consider
    // ASCII characters. Note that the encoding of z[] is UTF8 - we are dealing with only
    // UTF8 here in this code - but the database engine itself might be processing content
    // using a different encoding.
    let mut cnt: usize = 0;
    let mut c;
    loop {
        c = at(cnt);
        if c == 0 || c == wc[0] || c == wc[1] || c == wc[2] {
            break;
        }
        cnt += 1;
        if c == wc[3] && at(cnt) > 0 && at(cnt) < 0x80 {
            cnt += 1;
        } else if c >= 0x80 {
            let start = cnt - 1;
            let (cp, next) = utf8_read(&zb, start);
            if c == 0xff || cp == 0xfffd || p.enc() == sqliters_vdbe::vdbeaux::SQLITE_UTF16LE {
                cnt -= 1;
                break;
            } else {
                cnt = next;
            }
        }
    }

    // The optimization is possible only if (1) the pattern does not begin with a wildcard
    // or quote character, and (2) the last character before the first wildcard is not 0xff.
    if (cnt > 1 || (cnt > 0 && at(0) != wc[3])) && 255 != at(cnt - 1) {
        // A "complete" match if the pattern ends with "*" or "%"
        *pis_complete = c == wc[0] && at(cnt + 1) == 0 && p.enc() != sqliters_vdbe::vdbeaux::SQLITE_UTF16LE;

        // Get the pattern prefix. Remove all escapes from the prefix.
        let mut z_new: Vec<u8> = Vec::with_capacity(cnt);
        let mut i_from = 0;
        while i_from < cnt {
            if zb[i_from] == wc[3] {
                i_from += 1;
            }
            z_new.push(at(i_from));
            i_from += 1;
        }
        // (zNew[iTo] = 0: a NUL copied from an escape at the very end terminates the string)
        if let Some(pos) = z_new.iter().position(|&b| b == 0) {
            z_new.truncate(pos);
        }
        let i_to = z_new.len();

        // If the LHS is not an ordinary column with TEXT affinity, then the pattern prefix
        // boundaries (both the start and end boundaries) must not look like a number.
        // Otherwise the range constraints we generate might match numbers and BLOBs that the
        // LIKE or GLOB would not match.
        let l = p.e(p_left);
        if l.op != TK_COLUMN || sqlite3_expr_affinity(p, p_left) != SQLITE_AFF_TEXT || (l.y_tab.nn() && p.tab(l.y_tab).is_virtual()) {
            let mut is_num = atof_is_num(&z_new);
            if !is_num {
                if i_to == 1 && z_new[0] == b'-' {
                    is_num = true;
                } else if i_to > 0 {
                    z_new[i_to - 1] = z_new[i_to - 1].wrapping_add(1);
                    is_num = atof_is_num(&z_new);
                    z_new[i_to - 1] = z_new[i_to - 1].wrapping_sub(1);
                }
            }
            if is_num {
                return false;
            }
        }
        // pPrefix = sqlite3Expr(db, TK_STRING, (char*)z) with the token cut and unescaped
        let s = String::from_utf8_lossy(&z_new).into_owned();
        let prefix = sqlite3_expr(p, TK_STRING, Some(&s));
        // the bytes of the prefix are kept exactly (the prefix can end inside a multi-byte
        // character only when the pattern itself is malformed)
        *pp_prefix = prefix;
        // whereexpr.c:313–329: retain the named/numbered parameter even when
        // the complete prefix removes the residual LIKE function.
        if op == TK_VARIABLE {
            let i_col = p.e(p_right).i_column as i32;
            p.v.reprepare_on_bind(i_col);
            if *pis_complete && p.e(p_right).tok().len() > 1 {
                let reg = sqlite3_get_temp_reg(p);
                sqlite3_expr_code_target(p, p_right, reg);
                let addr = p.v.current_addr() - 1;
                p.v.change_p3(addr, 0);
                sqlite3_release_temp_reg(p, reg);
            }
        }
        true
    } else {
        false
    }
}

/// `transferJoinMarkings(pDerived, pBase)`: if the pBase expression originated in the ON or
/// USING clause of a join, then transfer the appropriate markings over to derived.
fn transfer_join_markings(p: &mut Parse, derived: ExprId, base: ExprId) {
    if derived.nn() && p.e(base).has(EP_OuterON | EP_InnerON) {
        let (f, j) = (p.e(base).flags & (EP_OuterON | EP_InnerON), p.e(base).w);
        let d = p.em(derived);
        d.flags |= f;
        d.w = j;
    }
}

/// `markTermAsChild(pWC, iChild, iParent)`: mark term iChild as being a child of term
/// iParent
fn mark_term_as_child(w: &mut WhereInfo, wc: WcId, i_child: usize, i_parent: usize) {
    let c = &mut w.a_wc[wc];
    c.a[i_child].i_parent = i_parent as i32;
    c.a[i_child].truth_prob = c.a[i_parent].truth_prob;
    c.a[i_parent].n_child += 1;
}

/// `whereNthSubterm(pTerm, N)`: return the N-th AND-connected subterm of pTerm. Or if pTerm
/// is not a conjunction, then return just pTerm when N==0. If N is exceeds the number of
/// available subterms, return NULL.
fn where_nth_subterm(w: &WhereInfo, t: TermRef, n: usize) -> Option<TermRef> {
    let term = w.term(t);
    if term.e_operator != WO_AND {
        return if n == 0 { Some(t) } else { None };
    }
    let and_wc = term.p_and_info.expect("pAndInfo");
    if n < w.a_wc[and_wc].a.len() {
        return Some(TermRef::new(and_wc, n));
    }
    None
}

/// `whereCombineDisjuncts(pSrc, pWC, pOne, pTwo)`: subterms pOne and pTwo are contained
/// within WHERE clause pWC. The two subterms are in disjunction - they are OR-ed together.
///
/// If these two terms are both of the form: "A op B" with the same A and B values but
/// different operators and if the operators are compatible, then generate a new virtual term
/// that captures the valid result of both subexpressions. The following list shows the
/// operators that are compatible:
///
/// ```text
///    Combination     Simplified Expression
///    -----------     ---------------------
///    x<y OR x=y      x<=y
///    x=y OR x=y      x=y
///    x<=y OR x<y     x<=y
/// ```
///
/// The following is NOT generated:
///
/// ```text
///    x<y OR x>y  -->     x!=y
/// ```
fn where_combine_disjuncts(p: &mut Parse, w: &mut WhereInfo, wc: WcId, one: TermRef, two: TermRef) {
    let (o, t) = (w.term(one).clone(), w.term(two).clone());
    let mut e_op = o.e_operator | t.e_operator;
    if (o.wt_flags | t.wt_flags) & TERM_VNULL != 0 {
        return;
    }
    if (o.e_operator & (WO_EQ | WO_LT | WO_LE | WO_GT | WO_GE)) == 0 {
        return;
    }
    if (t.e_operator & (WO_EQ | WO_LT | WO_LE | WO_GT | WO_GE)) == 0 {
        return;
    }
    if (e_op & (WO_EQ | WO_LT | WO_LE)) != e_op && (e_op & (WO_EQ | WO_GT | WO_GE)) != e_op {
        return;
    }
    let (pa, pb) = (o.p_expr, t.p_expr);
    if sqlite3_expr_compare(p, false, p.e(pa).p_left, p.e(pb).p_left, -1) != 0 {
        return;
    }
    if sqlite3_expr_compare(p, false, p.e(pa).p_right, p.e(pb).p_right, -1) != 0 {
        return;
    }
    if p.e(pa).has(EP_Commuted) != p.e(pb).has(EP_Commuted) {
        return;
    }
    // If we reach this point, it means the two subterms can be combined
    if (e_op & (e_op - 1)) != 0 {
        if e_op & (WO_LT | WO_LE) != 0 {
            e_op = WO_LE;
        } else {
            e_op = WO_GE;
        }
    }
    let new = sqlite3_expr_dup(p, pa, 0);
    if new.is_null() {
        return;
    }
    let mut op = TK_EQ;
    while e_op != (WO_EQ << (op - TK_EQ)) {
        op += 1;
    }
    p.em(new).op = op;
    let idx_new = where_clause_insert(p, w, wc, new, TERM_VIRTUAL | TERM_DYNAMIC);
    expr_analyze(p, w, wc, idx_new);
}

/// `exprAnalyzeOrTerm(pSrc, pWC, idxTerm)`: analyze a term that consists of two or more OR-
/// connected subterms. So in:
///
/// ```text
///     ... WHERE  (a=5) AND (b=7 OR c=9 OR d=13) AND (d=13)
///                          ^^^^^^^^^^^^^^^^^^^^
/// ```
///
/// This routine analyzes terms such as the middle term in the above example. A WhereOrTerm
/// object is computed and attached to the term under analysis, regardless of the outcome of
/// the analysis. Hence:
///
/// ```text
///     WhereTerm.wtFlags   |=  TERM_ORINFO
///     WhereTerm.u.pOrInfo  =  a dynamically allocated WhereOrTerm object
/// ```
///
/// The term being analyzed must have two or more of OR-connected subterms. A single subterm
/// might be a set of AND-connected sub-subterms. Examples of terms under analysis:
///
/// ```text
///     (A)     t1.x=t2.y OR t1.x=t2.z OR t1.y=15 OR t1.z=t3.a+5
///     (B)     x=expr1 OR expr2=x OR x=expr3
///     (C)     t1.x=t2.y OR (t1.x=t2.z AND t1.y=15)
///     (D)     x=expr1 OR (y>11 AND y<22 AND z LIKE '*hello*')
///     (E)     (p.a=1 AND q.b=2 AND r.c=3) OR (p.x=4 AND q.y=5 AND r.z=6)
///     (F)     x>A OR (x=A AND y>=B)
/// ```
///
/// CASE 1: if all subterms are of the form T.C=expr for some single column of some single
/// table T (as shown in example B above) then create a new virtual term that is an
/// equivalent IN expression. In other words, if the term being analyzed is:
///
/// ```text
///      x = expr1  OR  expr2 = x  OR  x = expr3
/// ```
///
/// then create a new virtual term like this:
///
/// ```text
///      x IN (expr1,expr2,expr3)
/// ```
///
/// CASE 2: if there are exactly two disjuncts and one side has x>A and the other side has
/// x=A (for the same x and A) then add a new virtual conjunct term to the WHERE clause of
/// the form "x>=A".
///
/// CASE 3: if all subterms are indexable by a single table T, then set
///
/// ```text
///     WhereTerm.eOperator              =  WO_OR
///     WhereTerm.u.pOrInfo->indexable  |=  the cursor number for table T
/// ```
///
/// A subterm is "indexable" if it is of the form "T.C <op> <expr>" where C is any column of
/// table T and <op> is one of "=", "<", "<=", ">", ">=", "IS NULL", or "IN". A subterm is
/// also indexable if it is an AND of two or more subsubterms at least one of which is
/// indexable. Indexable AND subterms have their eOperator set to WO_AND and they have
/// u.pAndInfo set to a dynamically allocated WhereAndTerm object.
fn expr_analyze_or_term(p: &mut Parse, w: &mut WhereInfo, wc: WcId, idx_term: usize) {
    let e = w.a_wc[wc].a[idx_term].p_expr;

    // Break the OR clause into its separate subterms. The subterms are stored in a
    // WhereClause structure containing within the WhereOrInfo object that is attached to the
    // original OR clause term.
    w.a_wc.push(WhereClause::default());
    let or_wc = w.a_wc.len() - 1;
    w.a_wc[wc].a[idx_term].p_or_info = Some(WhereOrInfo { wc: or_wc, indexable: 0 });
    w.a_wc[wc].a[idx_term].wt_flags |= TERM_ORINFO;
    sqlite3_where_split(p, w, or_wc, e, TK_OR);
    sqlite3_where_expr_analyze(p, w, or_wc);

    // Compute the set of tables that might satisfy cases 1 or 3.
    let mut indexable: Bitmask = !0;
    let mut chng_to_in: Bitmask = !0;
    let n_or = w.a_wc[or_wc].a.len();
    let mut k = 0;
    // for(i=pOrWc->nTerm-1, pOrTerm=pOrWc->a; i>=0 && indexable; i--, pOrTerm++)
    while k < n_or && indexable != 0 {
        let (e_operator, wt_flags, or_expr) = {
            let t = &w.a_wc[or_wc].a[k];
            (t.e_operator, t.wt_flags, t.p_expr)
        };
        if (e_operator & WO_SINGLE) == 0 {
            chng_to_in = 0;
            w.a_wc.push(WhereClause::default());
            let and_wc = w.a_wc.len() - 1;
            let mut b: Bitmask = 0;
            {
                let t = &mut w.a_wc[or_wc].a[k];
                t.p_and_info = Some(and_wc);
                t.wt_flags |= TERM_ANDINFO;
                t.e_operator = WO_AND;
                t.left_cursor = -1;
            }
            sqlite3_where_split(p, w, and_wc, or_expr, TK_AND);
            sqlite3_where_expr_analyze(p, w, and_wc);
            w.a_wc[and_wc].p_outer = Some(wc);
            for j in 0..w.a_wc[and_wc].a.len() {
                let (ae, aop, acur) = {
                    let t = &w.a_wc[and_wc].a[j];
                    (t.p_expr, t.e_operator, t.left_cursor)
                };
                if allowed_op(p.e(ae).op) || aop == WO_AUX {
                    b |= sqlite3_where_get_mask(&w.s_mask_set, acur);
                }
            }
            indexable &= b;
        } else if wt_flags & TERM_COPIED != 0 {
            // Skip this term for now. We revisit it when we process the corresponding
            // TERM_VIRTUAL term
        } else {
            let (left_cursor, i_parent) = {
                let t = &w.a_wc[or_wc].a[k];
                (t.left_cursor, t.i_parent)
            };
            let mut b = sqlite3_where_get_mask(&w.s_mask_set, left_cursor);
            if wt_flags & TERM_VIRTUAL != 0 {
                let other_cursor = w.a_wc[or_wc].a[i_parent as usize].left_cursor;
                b |= sqlite3_where_get_mask(&w.s_mask_set, other_cursor);
            }
            indexable &= b;
            if (e_operator & WO_EQ) == 0 {
                chng_to_in = 0;
            } else {
                chng_to_in &= b;
            }
        }
        k += 1;
    }

    // Record the set of tables that satisfy case 3. The set might be empty.
    {
        let t = &mut w.a_wc[wc].a[idx_term];
        t.p_or_info = Some(WhereOrInfo { wc: or_wc, indexable });
        t.e_operator = WO_OR;
        t.left_cursor = -1;
    }
    if indexable != 0 {
        w.a_wc[wc].has_or = true;
    }

    // For a two-way OR, attempt to implementation case 2.
    if indexable != 0 && w.a_wc[or_wc].a.len() == 2 {
        let mut i_one = 0;
        while let Some(one) = where_nth_subterm(w, TermRef::new(or_wc, 0), i_one) {
            i_one += 1;
            let mut i_two = 0;
            while let Some(two) = where_nth_subterm(w, TermRef::new(or_wc, 1), i_two) {
                i_two += 1;
                where_combine_disjuncts(p, w, wc, one, two);
            }
        }
    }

    // chngToIN holds a set of tables that *might* satisfy case 1. But we have to do some
    // additional checking to see if case 1 really is satisfied.
    //
    // chngToIN will hold either 0, 1, or 2 bits. The 0-bit case means that there is no
    // possibility of transforming the OR clause into an IN operator because one or more
    // subterm were not indexable. The 1-bit case means that every OR subterm has a common
    // table ... The 2-bit case is when the all terms are of the form "table1.column=
    // table2.column". It might be possible to form an IN operator with either table1.column
    // or table2.column as the LHS if either is common to every term of the OR clause.
    //
    // Note that terms of the form "table.column1=table.column2" (the same table on both sizes
    // of the ==) cannot be optimized.
    if chng_to_in != 0 {
        let mut ok_to_chng_to_in = false; // True if the conversion to IN is valid
        let mut i_column = -1; // Column index on lhs of IN operator
        let mut i_cursor = -1; // Table cursor common to all terms
        let n_or = w.a_wc[or_wc].a.len();

        // Search for a table and column that appears on one side or the other of the ==
        // operator in every subterm. That table and column will be recorded in iCursor and
        // iColumn. There might not be any such table and column. Set okToChngToIN if an
        // appropriate table and column are found.
        let mut j = 0;
        while j < 2 && !ok_to_chng_to_in {
            let mut p_left = ExprId::NULL;
            let mut k = 0; // index of pOrTerm
            let mut found = false;
            while k < n_or {
                w.a_wc[or_wc].a[k].wt_flags &= !TERM_OK;
                let (left_cursor, left_column, oe) = {
                    let t = &w.a_wc[or_wc].a[k];
                    (t.left_cursor, t.left_column, t.p_expr)
                };
                if left_cursor == i_cursor {
                    // This is the 2-bit case and we are on the second iteration and current
                    // term is from the first iteration. So skip this term.
                    k += 1;
                    continue;
                }
                if (chng_to_in & sqlite3_where_get_mask(&w.s_mask_set, left_cursor)) == 0 {
                    // This term must be of the form t1.a==t2.b where t2 is in the chngToIN
                    // set but t1 is not. This term will be either preceded or followed by a
                    // copy of the same term with t1.a and t2.b swapped.
                    k += 1;
                    continue;
                }
                i_column = left_column;
                i_cursor = left_cursor;
                p_left = p.e(oe).p_left;
                found = true;
                break;
            }
            if !found {
                // No candidate table+column was found. This can only occur on the second
                // iteration
                break;
            }

            // We have found a candidate table and column. Check to see if that table and
            // column is common to every term in the OR clause
            ok_to_chng_to_in = true;
            while k < n_or && ok_to_chng_to_in {
                let (left_cursor, left_column, oe) = {
                    let t = &w.a_wc[or_wc].a[k];
                    (t.left_cursor, t.left_column, t.p_expr)
                };
                if left_cursor != i_cursor {
                    w.a_wc[or_wc].a[k].wt_flags &= !TERM_OK;
                } else if left_column != i_column
                    || (i_column == XN_EXPR && sqlite3_expr_compare(p, true, p.e(oe).p_left, p_left, -1) != 0)
                {
                    ok_to_chng_to_in = false;
                } else {
                    // If the right-hand side is also a column, then the affinities of both
                    // right and left sides must be such that no type conversions are
                    // required on the right. (Ticket #2249)
                    let aff_right = sqlite3_expr_affinity(p, p.e(oe).p_right);
                    let aff_left = sqlite3_expr_affinity(p, p.e(oe).p_left);
                    if aff_right != 0 && aff_right != aff_left {
                        ok_to_chng_to_in = false;
                    } else {
                        w.a_wc[or_wc].a[k].wt_flags |= TERM_OK;
                    }
                }
                k += 1;
            }
            j += 1;
        }

        // At this point, okToChngToIN is true if original pTerm is a case 1 type, and
        // iCursor,iColumn identify the left-hand side of the IN operator
        if ok_to_chng_to_in {
            let mut list = ExprListId::NULL; // The RHS of the IN operator
            let mut p_left = ExprId::NULL; // The LHS of the IN operator
            let mut coll_seq: Option<Arc<CollSeq>> = None; // Collating sequence to use
            let mut mismatch = false;
            for k in 0..n_or {
                let (wt_flags, this) = {
                    let t = &w.a_wc[or_wc].a[k];
                    (t.wt_flags, t.p_expr)
                };
                if (wt_flags & TERM_OK) == 0 {
                    continue;
                }
                let dup = sqlite3_expr_dup(p, p.e(this).p_right, 0);
                list = sqlite3_expr_list_append(p, list, dup);
                if p_left.is_null() {
                    p_left = p.e(this).p_left;
                    coll_seq = sqlite3_expr_compare_coll_seq(p, this);
                } else {
                    let c = sqlite3_expr_compare_coll_seq(p, this);
                    if !coll_ptr_eq(&coll_seq, &c) {
                        mismatch = true; // Collating sequence mismatch
                        break;
                    }
                }
            }
            if !mismatch && p_left.nn() {
                let mut dup = sqlite3_expr_dup(p, p_left, 0);
                let c = sqlite3_expr_coll_seq(p, dup);
                if !coll_ptr_eq(&c, &coll_seq)
                    && let Some(cs) = &coll_seq
                {
                    let name = cs.name.clone();
                    dup = sqlite3_expr_add_collate_string(p, dup, &name);
                }
                let new = sqlite3_p_expr(p, TK_IN, dup, ExprId::NULL);
                if new.nn() {
                    transfer_join_markings(p, new, e);
                    p.em(new).x = list.0;
                    let idx_new = where_clause_insert(p, w, wc, new, TERM_VIRTUAL | TERM_DYNAMIC);
                    expr_analyze(p, w, wc, idx_new);
                    mark_term_as_child(w, wc, idx_new, idx_term);
                }
            }
        }
    }
}

/// `termIsEquivalence(pParse, pExpr, pSrc)`: we already know that pExpr is a binary operator
/// where both operands are column references. This routine checks to see if pExpr is an
/// equivalence relation:
///   1.  The SQLITE_Transitive optimization must be enabled
///   2.  Must be either an == or an IS operator
///   3.  Not originating in the ON clause of an OUTER JOIN
///   4.  The operator is not IS or else the query does not contain RIGHT JOIN
///   5.  The affinities of A and B must be compatible
///   6a. Both operands use the same collating sequence OR
///   6b. The overall collating sequence is BINARY
/// If this routine returns TRUE, that means that the RHS can be substituted for the LHS
/// anyplace else in the WHERE clause where the LHS column occurs. This is an optimization.
/// No harm comes from returning 0. But if 1 is returned when 0 is correct, then wrong
/// results might be computed.
fn term_is_equivalence(p: &mut Parse, e: ExprId, src: SrcListId) -> bool {
    if !p.db.optimization_enabled(SQLITE_Transitive) {
        return false; // (1)
    }
    let op = p.e(e).op;
    if op != TK_EQ && op != TK_IS {
        return false; // (2)
    }
    if p.e(e).has(EP_OuterON | EP_Collate) {
        return false; // (3)
    }
    if op == TK_IS && p.src(src).a.len() >= 2 && (p.src(src).a[0].jointype as i32 & JT_LTORJ) != 0 {
        return false; // (4)
    }
    let (l, r) = (p.e(e).p_left, p.e(e).p_right);
    let aff1 = sqlite3_expr_affinity(p, l);
    let aff2 = sqlite3_expr_affinity(p, r);
    if aff1 != aff2 && (!is_numeric_affinity(aff1) || !is_numeric_affinity(aff2)) {
        return false; // (5)
    }
    if !sqlite3_expr_coll_seq_match(p, l, r) {
        return false; // (6)
    }
    true
}

/// `exprSelectUsage(pMaskSet, pS)`: recursively walk the expressions of a SELECT statement
/// and generate a bitmask indicating which tables are used in that expression tree.
fn expr_select_usage(p: &Parse, m: &mut WhereMaskSet, mut s: SelectId) -> Bitmask {
    let mut mask: Bitmask = 0;
    while s.nn() {
        let sel = p.s(s).clone();
        mask |= sqlite3_where_expr_list_usage(p, m, sel.p_e_list);
        mask |= sqlite3_where_expr_list_usage(p, m, sel.p_group_by);
        mask |= sqlite3_where_expr_list_usage(p, m, sel.p_order_by);
        mask |= sqlite3_where_expr_usage(p, m, sel.p_where);
        mask |= sqlite3_where_expr_usage(p, m, sel.p_having);
        if sel.p_src.nn() {
            for i in 0..p.src(sel.p_src).a.len() {
                let it = &p.src(sel.p_src).a[i];
                if it.is_subquery
                    && let Some(q) = &it.p_subq
                {
                    mask |= expr_select_usage(p, m, q.p_select);
                }
                if !it.is_using {
                    mask |= sqlite3_where_expr_usage(p, m, it.p_on);
                }
                if it.is_tab_func {
                    mask |= sqlite3_where_expr_list_usage(p, m, it.p_func_arg);
                }
            }
        }
        s = sel.p_prior;
    }
    mask
}

/// `exprMightBeIndexed2(pFrom, aiCurCol, pExpr, j)` / `exprMightBeIndexed(pFrom, aiCurCol,
/// pExpr, op)`: check to see if pExpr is an expression that might be used by an index.
///
/// If pExpr is a TK_COLUMN column reference, then this routine always returns true even if
/// that particular column is not indexed, because the column might be added to an automatic
/// index later.
fn expr_might_be_indexed(p: &mut Parse, from: SrcListId, ai_cur_col: &mut [i32; 2], mut e: ExprId, op: u8) -> bool {
    // If this expression is a vector to the left or right of a inequality constraint (>, <,
    // >= or <=), perform the processing on the first element of the vector.
    if p.e(e).op == TK_VECTOR && (op >= TK_GT && op <= TK_GE) {
        e = p.l(p.e(e).x_list()).a[0].p_expr;
    }
    if p.e(e).op == TK_COLUMN {
        ai_cur_col[0] = p.e(e).i_table;
        ai_cur_col[1] = p.e(e).i_column as i32;
        return true;
    }
    let n_src = p.src(from).a.len();
    for i in 0..n_src {
        let tab = p.src(from).a[i].p_s_tab;
        let has_expr_idx = p.tab(tab).p_index.iter().any(|&ix| p.idx(ix).a_col_expr.nn());
        if has_expr_idx {
            // exprMightBeIndexed2(pFrom, aiCurCol, pExpr, i)
            let mut j = i;
            loop {
                let (i_cur, tab) = {
                    let it = &p.src(from).a[j];
                    (it.i_cursor, it.p_s_tab)
                };
                let idxs = p.tab(tab).p_index.clone();
                for ix in idxs {
                    let ce = p.idx(ix).a_col_expr;
                    if ce.is_null() {
                        continue;
                    }
                    for k in 0..p.idx(ix).n_key_col as usize {
                        if p.idx(ix).ai_column[k] as i32 != XN_EXPR {
                            continue;
                        }
                        let ie = p.l(ce).a[k].p_expr;
                        if sqlite3_expr_compare_skip(p, e, ie, i_cur) == 0 && sqlite3_expr_is_constant_no_parse(p, ie) == 0 {
                            ai_cur_col[0] = i_cur;
                            ai_cur_col[1] = XN_EXPR;
                            return true;
                        }
                    }
                }
                j += 1;
                if j >= n_src {
                    break;
                }
            }
            return false;
        }
    }
    false
}

/// `exprAnalyze(pSrc, pWC, idxTerm)`: the input to this routine is an WhereTerm structure
/// with only the "pExpr" field filled in. The job of this routine is to analyze the
/// subexpression and populate all the other fields of the WhereTerm structure.
///
/// If the expression is of the form "<expr> <op> X" it gets commuted to the standard form of
/// "X <op> <expr>".
///
/// If the expression is of the form "X <op> Y" where both X and Y are columns, then the
/// original expression is unchanged and a new virtual term of the form "Y <op> X" is added
/// to the WHERE clause and analyzed separately. The original term is marked with TERM_COPIED
/// and the new term is marked with TERM_DYNAMIC (because it's pExpr needs to be freed with
/// the WhereClause) and TERM_VIRTUAL (because it is the result of a transformation). The new
/// term will have its parent set to the original term.
pub fn expr_analyze(p: &mut Parse, w: &mut WhereInfo, wc: WcId, idx_term: usize) {
    let src = w.p_tab_list;
    let mut extra_right: Bitmask = 0; // Extra dependencies on LEFT JOIN
    let mut p_str1 = ExprId::NULL; // RHS of LIKE/GLOB operator
    let mut is_complete = false; // RHS of LIKE/GLOB ends with wildcard
    let mut no_case = false; // uppercase equivalent to lowercase
    let mut e = w.a_wc[wc].a[idx_term].p_expr;
    let (mut prereq_left, mut prereq_all, mut op);
    loop {
        // exprAnalyze_restart:
        w.s_mask_set.b_var_select = false;
        prereq_left = sqlite3_where_expr_usage(p, &mut w.s_mask_set, p.e(e).p_left);
        op = p.e(e).op;
        if op == TK_IN {
            if sqlite3_expr_check_in(p, e) {
                return;
            }
            let pr = if p.e(e).use_x_select() {
                expr_select_usage(p, &mut w.s_mask_set, p.e(e).x_select())
            } else {
                sqlite3_where_expr_list_usage(p, &mut w.s_mask_set, p.e(e).x_list())
            };
            w.a_wc[wc].a[idx_term].prereq_right = pr;
            prereq_all = prereq_left | pr;
        } else {
            let pr = sqlite3_where_expr_usage(p, &mut w.s_mask_set, p.e(e).p_right);
            w.a_wc[wc].a[idx_term].prereq_right = pr;
            if p.e(e).p_left.is_null() || p.e(e).has(EP_xIsSelect | EP_IfNullRow) || p.e(e).x != 0 {
                prereq_all = sqlite3_where_expr_usage_nn(p, &mut w.s_mask_set, e);
            } else {
                prereq_all = prereq_left | pr;
            }
        }
        if w.s_mask_set.b_var_select {
            w.a_wc[wc].a[idx_term].wt_flags |= TERM_VARSELECT;
        }

        if p.e(e).has(EP_OuterON | EP_InnerON) {
            let x = sqlite3_where_get_mask(&w.s_mask_set, p.e(e).w);
            if p.e(e).has(EP_OuterON) {
                prereq_all |= x;
                // ON clause terms may not be used with an index on left table of a LEFT
                // JOIN. Ticket #3015
                extra_right = x.wrapping_sub(1);
            } else if (prereq_all >> 1) >= x {
                p.em(e).flags &= !EP_InnerON;
            }
        }
        {
            let t = &mut w.a_wc[wc].a[idx_term];
            t.prereq_all = prereq_all;
            t.left_cursor = -1;
            t.i_parent = -1;
            t.e_operator = 0;
        }
        if allowed_op(op) {
            let mut ai_cur_col = [0i32; 2];
            let mut p_left = sqlite3_expr_skip_collate(p, p.e(e).p_left);
            let p_right = sqlite3_expr_skip_collate(p, p.e(e).p_right);
            let op_mask: u16 = if (w.a_wc[wc].a[idx_term].prereq_right & prereq_left) == 0 { WO_ALL } else { WO_EQUIV };

            let i_field = w.a_wc[wc].a[idx_term].i_field;
            if i_field > 0 {
                p_left = p.l(p.e(p_left).x_list()).a[(i_field - 1) as usize].p_expr;
            }

            if expr_might_be_indexed(p, src, &mut ai_cur_col, p_left, op) {
                let t = &mut w.a_wc[wc].a[idx_term];
                t.left_cursor = ai_cur_col[0];
                t.left_column = ai_cur_col[1];
                t.e_operator = operator_mask(op) & op_mask;
            }
            if op == TK_IS {
                w.a_wc[wc].a[idx_term].wt_flags |= TERM_IS;
            }
            if p_right.nn() && expr_might_be_indexed(p, src, &mut ai_cur_col, p_right, op) && !p.e(p_right).has(EP_FixedCol) {
                let (new_idx, dup);
                let mut e_extra_op: u16 = 0; // Extra bits for pNew->eOperator
                if w.a_wc[wc].a[idx_term].left_cursor >= 0 {
                    dup = sqlite3_expr_dup(p, e, 0);
                    let idx_new = where_clause_insert(p, w, wc, dup, TERM_VIRTUAL | TERM_DYNAMIC);
                    if idx_new == 0 {
                        return;
                    }
                    new_idx = idx_new;
                    mark_term_as_child(w, wc, idx_new, idx_term);
                    if op == TK_IS {
                        w.a_wc[wc].a[idx_new].wt_flags |= TERM_IS;
                    }
                    w.a_wc[wc].a[idx_term].wt_flags |= TERM_COPIED;

                    if term_is_equivalence(p, dup, w.p_tab_list) {
                        w.a_wc[wc].a[idx_term].e_operator |= WO_EQUIV;
                        e_extra_op = WO_EQUIV;
                    }
                } else {
                    dup = e;
                    new_idx = idx_term;
                }
                let f = expr_commute(p, dup);
                let dop = p.e(dup).op;
                let t = &mut w.a_wc[wc].a[new_idx];
                t.wt_flags |= f;
                t.left_cursor = ai_cur_col[0];
                t.left_column = ai_cur_col[1];
                t.prereq_right = prereq_left | extra_right;
                t.prereq_all = prereq_all;
                t.e_operator = (operator_mask(dop) + e_extra_op) & op_mask;
            } else if op == TK_ISNULL && !p.e(e).has(EP_OuterON) && !sqlite3_expr_can_be_null(p, p_left) {
                let x = p.em(e);
                x.op = TK_TRUEFALSE; // See tag-20230504-1
                x.z_token = Some("false".to_owned());
                x.flags |= EP_IsFalse;
                let t = &mut w.a_wc[wc].a[idx_term];
                t.prereq_all = 0;
                t.e_operator = 0;
            }
        }
        // If a term is the BETWEEN operator, create two new virtual terms that define the
        // range that the BETWEEN implements. For example:
        //
        //      a BETWEEN b AND c
        //
        // is converted into:
        //
        //      (a BETWEEN b AND c) AND (a>=b) AND (a<=c)
        //
        // The two new terms are added onto the end of the WhereClause object. The new terms
        // are "dynamic" and are children of the original BETWEEN term. That means that if
        // the BETWEEN term is coded, the children are skipped. Or, if the children are
        // satisfied by an index, the original BETWEEN term is skipped.
        else if p.e(e).op == TK_BETWEEN && w.a_wc[wc].op == TK_AND {
            const OPS: [u8; 2] = [TK_GE, TK_LE];
            let list = p.e(e).x_list();
            for i in 0..2 {
                let l = sqlite3_expr_dup(p, p.e(e).p_left, 0);
                let r = sqlite3_expr_dup(p, p.l(list).a[i].p_expr, 0);
                let new_expr = sqlite3_p_expr(p, OPS[i], l, r);
                transfer_join_markings(p, new_expr, e);
                let idx_new = where_clause_insert(p, w, wc, new_expr, TERM_VIRTUAL | TERM_DYNAMIC);
                expr_analyze(p, w, wc, idx_new);
                mark_term_as_child(w, wc, idx_new, idx_term);
            }
        }
        // Analyze a term that is composed of two or more subterms connected by an OR
        // operator.
        else if p.e(e).op == TK_OR && !p.e(e).has(EP_Collate) {
            let alt = sqlite3_expr_simplified_and_or(p, e);
            if alt != e {
                e = sqlite3_expr_skip_collate_and_likely(p, alt);
                w.a_wc[wc].a[idx_term].p_expr = e;
                continue;
            }
            expr_analyze_or_term(p, w, wc, idx_term);
        }
        // The form "x IS NOT NULL" can sometimes be evaluated more efficiently as "x>NULL" if
        // x is not an INTEGER PRIMARY KEY. So construct a virtual term of that form.
        //
        // The virtual term must be tagged with TERM_VNULL.
        else if p.e(e).op == TK_NOTNULL {
            let left = p.e(e).p_left;
            if p.e(left).op == TK_COLUMN && p.e(left).i_column >= 0 && !p.e(e).has(EP_OuterON) {
                let l = sqlite3_expr_dup(p, left, 0);
                let r = sqlite3_expr_alloc(p, TK_NULL, None, 0);
                let new_expr = sqlite3_p_expr(p, TK_GT, l, r);
                let idx_new = where_clause_insert(p, w, wc, new_expr, TERM_VIRTUAL | TERM_DYNAMIC | TERM_VNULL);
                if idx_new != 0 {
                    let (i_table, i_column) = (p.e(left).i_table, p.e(left).i_column as i32);
                    {
                        let t = &mut w.a_wc[wc].a[idx_new];
                        t.prereq_right = 0;
                        t.left_cursor = i_table;
                        t.left_column = i_column;
                        t.e_operator = WO_GT;
                    }
                    mark_term_as_child(w, wc, idx_new, idx_term);
                    w.a_wc[wc].a[idx_term].wt_flags |= TERM_COPIED;
                    let pa = w.a_wc[wc].a[idx_term].prereq_all;
                    w.a_wc[wc].a[idx_new].prereq_all = pa;
                }
            }
        }
        // Add constraints to reduce the search space on a LIKE or GLOB operator.
        //
        // A like pattern of the form "x LIKE 'aBc%'" is changed into constraints
        //
        //          x>='ABC' AND x<'abd' AND x LIKE 'aBc%'
        //
        // The last character of the prefix "abc" is incremented to form the termination
        // condition "abd". If case is not significant (the default for LIKE) then the
        // lower-bound is made all uppercase and the upper- bound is made all lowercase so
        // that the bounds also work when comparing BLOBs.
        else if p.e(e).op == TK_FUNCTION
            && w.a_wc[wc].op == TK_AND
            && is_like_or_glob(p, e, &mut p_str1, &mut is_complete, &mut no_case)
        {
            let wt_flags = TERM_LIKEOPT | TERM_VIRTUAL | TERM_DYNAMIC;
            let p_left = p.l(p.e(e).x_list()).a[1].p_expr;
            let p_str2 = sqlite3_expr_dup(p, p_str1, 0);

            // Convert the lower bound to upper-case and the upper bound to lower-case (upper-
            // case is less than lower-case in ASCII) so that the range constraints also work
            // for BLOBs
            let mut s1: Vec<u8> = p.e(p_str1).tok().as_bytes().to_vec();
            let mut s2: Vec<u8> = s1.clone();
            if no_case {
                w.a_wc[wc].a[idx_term].wt_flags |= TERM_LIKE;
                for i in 0..s1.len() {
                    let c = s1[i];
                    s1[i] = c.to_ascii_uppercase();
                    s2[i] = c.to_ascii_lowercase();
                }
            }
            {
                // Last character before the first wildcard
                let mut pc = s2.len() - 1;
                if no_case {
                    // The point is to increment the last character before the first wildcard.
                    // But if we increment '@', that will push it into the alphabetic range
                    // where case conversions will mess up the inequality. To avoid this,
                    // make sure to also run the full LIKE on all candidate expressions by
                    // clearing the isComplete flag
                    if s2[pc] == b'A' - 1 {
                        is_complete = false;
                    }
                    s2[pc] = s2[pc].to_ascii_lowercase();
                }
                // Increment the value of the last utf8 character in the prefix.
                while s2[pc] == 0xBF && pc > 0 {
                    s2[pc] = 0x80;
                    pc -= 1;
                }
                s2[pc] = s2[pc].wrapping_add(1);
            }
            // The bounds are C strings; a bound that is no longer valid UTF-8 after its last
            // byte was incremented cannot be held by the token text of this port.
            let (Ok(t1), Ok(t2)) = (String::from_utf8(s1), String::from_utf8(s2)) else {
                p.not_supported("LIKE optimization with a range bound that is not valid UTF-8");
                return;
            };
            p.em(p_str1).z_token = Some(t1);
            p.em(p_str2).z_token = Some(t2);

            let z_coll_seq_name = if no_case { "NOCASE" } else { "BINARY" };
            let mut new_expr1 = sqlite3_expr_dup(p, p_left, 0);
            new_expr1 = sqlite3_expr_add_collate_string(p, new_expr1, z_coll_seq_name);
            new_expr1 = sqlite3_p_expr(p, TK_GE, new_expr1, p_str1);
            transfer_join_markings(p, new_expr1, e);
            let idx_new1 = where_clause_insert(p, w, wc, new_expr1, wt_flags);
            let mut new_expr2 = sqlite3_expr_dup(p, p_left, 0);
            new_expr2 = sqlite3_expr_add_collate_string(p, new_expr2, z_coll_seq_name);
            new_expr2 = sqlite3_p_expr(p, TK_LT, new_expr2, p_str2);
            transfer_join_markings(p, new_expr2, e);
            let idx_new2 = where_clause_insert(p, w, wc, new_expr2, wt_flags);
            expr_analyze(p, w, wc, idx_new1);
            expr_analyze(p, w, wc, idx_new2);
            if is_complete {
                mark_term_as_child(w, wc, idx_new1, idx_term);
                mark_term_as_child(w, wc, idx_new2, idx_term);
            }
        }
        break;
    }

    // If there is a vector == or IS term - e.g. "(a, b) == (?, ?)" - create new terms for
    // each component comparison - "a = ?" and "b = ?". The new terms completely replace the
    // original vector comparison, which is no longer used.
    //
    // This is only required if at least one side of the comparison operation is not a
    // sub-select.
    //
    // tag-20220128a
    let eop = p.e(e).op;
    let n_left = if eop == TK_EQ || eop == TK_IS { sqlite3_expr_vector_size(p, p.e(e).p_left) } else { 0 };
    if (eop == TK_EQ || eop == TK_IS)
        && n_left > 1
        && sqlite3_expr_vector_size(p, p.e(e).p_right) == n_left
        && ((p.e(p.e(e).p_left).flags & EP_xIsSelect) == 0 || (p.e(p.e(e).p_right).flags & EP_xIsSelect) == 0)
        && w.a_wc[wc].op == TK_AND
    {
        for i in 0..n_left {
            let (l0, r0) = (p.e(e).p_left, p.e(e).p_right);
            let l = sqlite3_expr_for_vector_field(p, l0, i, n_left);
            let r = sqlite3_expr_for_vector_field(p, r0, i, n_left);
            let new = sqlite3_p_expr(p, eop, l, r);
            transfer_join_markings(p, new, e);
            let idx_new = where_clause_insert(p, w, wc, new, TERM_DYNAMIC | TERM_SLICE);
            expr_analyze(p, w, wc, idx_new);
        }
        let t = &mut w.a_wc[wc].a[idx_term];
        t.wt_flags |= TERM_CODED | TERM_VIRTUAL; // Disable the original
        t.e_operator = WO_ROWVAL;
    }
    // If there is a vector IN term - e.g. "(a, b) IN (SELECT ...)" - create a virtual term
    // for each vector component. The expression object used by each such virtual term is
    // pExpr (the full vector IN(...) expression). It is not a child of pExpr, as the vector
    // IN(...) is still needed to test for a match; the virtual terms only make it possible
    // to use an index.
    else if eop == TK_IN
        && w.a_wc[wc].a[idx_term].i_field == 0
        && p.e(p.e(e).p_left).op == TK_VECTOR
        && p.e(e).use_x_select()
        && {
            let s = p.e(e).x_select();
            (p.s(s).p_prior.is_null() || (p.s(s).sel_flags & SF_Values) != 0)
                && p.s(s).p_win.is_null()
                && (p.s(s).sel_flags & SF_MinMaxAgg) == 0
                && p.l(p.s(s).p_e_list).n_expr() <= 255 // UMXV(pTerm->nChild)
        }
        && w.a_wc[wc].op == TK_AND
    {
        let n = sqlite3_expr_vector_size(p, p.e(e).p_left);
        for i in 0..n {
            let idx_new = where_clause_insert(p, w, wc, e, TERM_VIRTUAL | TERM_SLICE);
            w.a_wc[wc].a[idx_new].i_field = i + 1;
            expr_analyze(p, w, wc, idx_new);
            mark_term_as_child(w, wc, idx_new, idx_term);
        }
    }
    // Add a WO_AUX auxiliary term to the constraint set if the current expression is of the
    // form "column OP expr" where OP is an operator that gets passed into virtual tables but
    // which is not normally optimized for ordinary tables. In other words, OP is one of
    // MATCH, LIKE, GLOB, REGEXP, !=, IS, IS NOT, or NOT NULL. This information is used by
    // the xBestIndex methods of virtual tables. The native query optimizer does not attempt
    // to do anything with MATCH functions.
    else if w.a_wc[wc].op == TK_AND {
        let (mut res, e_op2, mut p_left, mut p_right) = is_auxiliary_vtab_operator(p, e);
        while res > 0 {
            res -= 1;
            let prereq_expr = sqlite3_where_expr_usage(p, &mut w.s_mask_set, p_right);
            let prereq_column = sqlite3_where_expr_usage(p, &mut w.s_mask_set, p_left);
            if (prereq_expr & prereq_column) == 0 {
                let d = sqlite3_expr_dup(p, p_right, 0);
                let new_expr = sqlite3_p_expr(p, TK_MATCH, ExprId::NULL, d);
                if p.e(e).has(EP_OuterON) && new_expr.nn() {
                    let j = p.e(e).w;
                    let ne = p.em(new_expr);
                    ne.flags |= EP_OuterON;
                    ne.w = j;
                }
                let idx_new = where_clause_insert(p, w, wc, new_expr, TERM_VIRTUAL | TERM_DYNAMIC);
                let (lt, lc) = (p.e(p_left).i_table, p.e(p_left).i_column as i32);
                {
                    let nt = &mut w.a_wc[wc].a[idx_new];
                    nt.prereq_right = prereq_expr | extra_right;
                    nt.left_cursor = lt;
                    nt.left_column = lc;
                    nt.e_operator = WO_AUX;
                    nt.e_match_op = e_op2;
                }
                mark_term_as_child(w, wc, idx_new, idx_term);
                w.a_wc[wc].a[idx_term].wt_flags |= TERM_COPIED;
                let pa = w.a_wc[wc].a[idx_term].prereq_all;
                w.a_wc[wc].a[idx_new].prereq_all = pa;
            }
            std::mem::swap(&mut p_left, &mut p_right);
        }
    }

    // Prevent ON clause terms of a LEFT JOIN from being used to drive an index for tables to
    // the left of the join.
    w.a_wc[wc].a[idx_term].prereq_right |= extra_right;
}

/// `ExprIsVtab(X)`: X is a column of a virtual table
fn expr_is_vtab(p: &Parse, x: ExprId) -> bool {
    x.nn() && p.e(x).op == TK_COLUMN && p.e(x).y_tab.nn() && p.tab(p.e(x).y_tab).is_virtual()
}

/// `isAuxiliaryVtabOperator(db, pExpr, peOp2, ppLeft, ppRight)`: check to see if the given
/// expression is of the form
///
/// ```text
///         column OP expr
/// ```
///
/// where OP is one of MATCH, GLOB, LIKE, REGEXP, !=, IS NOT, or IS NOT NULL and "column" is a
/// column of a virtual table:
///
/// ```text
///          Expression                   Virtual Table Operator
///          -----------------------      ---------------------------------
///      1.  column MATCH expr            SQLITE_INDEX_CONSTRAINT_MATCH
///      2.  column GLOB expr             SQLITE_INDEX_CONSTRAINT_GLOB
///      3.  column LIKE expr             SQLITE_INDEX_CONSTRAINT_LIKE
///      4.  column REGEXP expr           SQLITE_INDEX_CONSTRAINT_REGEXP
///      5.  column != expr               SQLITE_INDEX_CONSTRAINT_NE
///      6.  expr != column               SQLITE_INDEX_CONSTRAINT_NE
///      7.  column IS NOT expr           SQLITE_INDEX_CONSTRAINT_ISNOT
///      8.  expr IS NOT column           SQLITE_INDEX_CONSTRAINT_ISNOT
///      9.  column IS NOT NULL           SQLITE_INDEX_CONSTRAINT_ISNOTNULL
/// ```
///
/// In every case, "column" must be a column of a virtual table. If there is a match, return
/// the "column" expression as the left one, the "expr" expression as the right one (even
/// though in forms (6) and (8) the column is on the right and the expression is on the left),
/// and the appropriate virtual table operator. The count is 1 or 2 if there is a match. The
/// usual count is 1, but if the RHS is also a column of virtual table in forms (5) or (7)
/// then it is 2. If the expression matches none of the patterns above, the count is 0.
fn is_auxiliary_vtab_operator(p: &mut Parse, e: ExprId) -> (i32, u8, ExprId, ExprId) {
    let op = p.e(e).op;
    if op == TK_FUNCTION {
        let list = p.e(e).x_list();
        if list.is_null() || p.l(list).a.len() != 2 {
            return (0, 0, ExprId::NULL, ExprId::NULL);
        }

        // Built-in operators MATCH, GLOB, LIKE, and REGEXP attach to a virtual table on their
        // second argument, which is the same as the left-hand side operand in their in-fix
        // form.
        //
        //       vtab_column MATCH expression
        //       MATCH(expression,vtab_column)
        let col = p.l(list).a[1].p_expr;
        if expr_is_vtab(p, col) {
            let i = crate::where_::sqlite3_expr_is_like_operator(p, e);
            if i != 0 {
                return (1, i as u8, col, p.l(list).a[0].p_expr);
            }
        }

        // We can also match against the first column of overloaded functions where
        // xFindFunction returns a value of at least SQLITE_INDEX_CONSTRAINT_FUNCTION.
        //
        //      OVERLOADED(vtab_column,expression)
        //
        // Historically, xFindFunction expected to see lower-case function names. But for
        // this use case, xFindFunction is expected to deal with function names in an
        // arbitrary case.
        let col = p.l(list).a[0].p_expr;
        if expr_is_vtab(p, col) {
            let vt = crate::vtab::sqlite3_get_vtable(p, p.e(col).y_tab).cloned();
            if let Some(vt) = vt
                && vt.has_find_function
            {
                let host = p.db.host;
                let z = p.e(e).tok().to_owned();
                let i = host.vtab_find_function(vt.id, 2, &z);
                if i >= crate::vtab::SQLITE_INDEX_CONSTRAINT_FUNCTION {
                    return (1, i as u8, col, p.l(list).a[1].p_expr);
                }
            }
        }
    } else if op >= TK_EQ {
        // Comparison operators are a common case. Save a few comparisons for that common case
        // by terminating early.
        return (0, 0, ExprId::NULL, ExprId::NULL);
    } else if op == TK_NE || op == TK_ISNOT || op == TK_NOTNULL {
        let mut res = 0;
        let mut p_left = p.e(e).p_left;
        let mut p_right = p.e(e).p_right;
        if expr_is_vtab(p, p_left) {
            res += 1;
        }
        if p_right.nn() && expr_is_vtab(p, p_right) {
            res += 1;
            std::mem::swap(&mut p_left, &mut p_right);
        }
        let e_op2 = match op {
            TK_NE => crate::vtab::SQLITE_INDEX_CONSTRAINT_NE,
            TK_ISNOT => crate::vtab::SQLITE_INDEX_CONSTRAINT_ISNOT,
            _ => crate::vtab::SQLITE_INDEX_CONSTRAINT_ISNOTNULL,
        };
        return (res, e_op2, p_left, p_right);
    }
    (0, 0, ExprId::NULL, ExprId::NULL)
}

/// `sqlite3WhereSplit(pWC, pExpr, op)`: this routine identifies subexpressions in the WHERE
/// clause where each subexpression is separated by the AND operator or some other operator
/// specified in the op parameter. The WhereClause structure is filled with pointers to
/// subexpressions. For example:
///
/// ```text
///    WHERE  a=='hello' AND coalesce(b,11)<10 AND (c+12!=d OR c==22)
///           \________/     \_______________/     \________________/
///            slot[0]            slot[1]               slot[2]
/// ```
///
/// The original WHERE clause in pExpr is unaltered. All this routine does is make slot[]
/// entries point to substructure within pExpr.
///
/// In the previous sentence and in the diagram, "slot[]" refers to the WhereClause.a[]
/// array. The slot[] array grows as needed to contain all terms of the WHERE clause.
pub fn sqlite3_where_split(p: &Parse, w: &mut WhereInfo, wc: WcId, e: ExprId, op: u8) {
    let e2 = sqlite3_expr_skip_collate_and_likely(p, e);
    w.a_wc[wc].op = op;
    if e2.is_null() {
        return;
    }
    if p.e(e2).op != op {
        where_clause_insert(p, w, wc, e, 0);
    } else {
        sqlite3_where_split(p, w, wc, p.e(e2).p_left, op);
        sqlite3_where_split(p, w, wc, p.e(e2).p_right, op);
    }
}

/// `sqlite3WhereExprUsageFull(pMaskSet, p)` / `sqlite3WhereExprUsageNN(pMaskSet, p)`: these
/// routines walk (recursively) an expression tree and generate a bitmask indicating which
/// tables are used in that expression tree.
pub fn sqlite3_where_expr_usage_nn(p: &Parse, m: &mut WhereMaskSet, e: ExprId) -> Bitmask {
    let x = p.e(e);
    if x.op == TK_COLUMN && !x.has(EP_FixedCol) {
        return sqlite3_where_get_mask(m, x.i_table);
    } else if x.has(EP_TokenOnly | EP_Leaf) {
        return 0;
    }
    // sqlite3WhereExprUsageFull()
    let mut mask = if x.op == TK_IF_NULL_ROW { sqlite3_where_get_mask(m, x.i_table) } else { 0 };
    if x.p_left.nn() {
        mask |= sqlite3_where_expr_usage_nn(p, m, x.p_left);
    }
    if x.p_right.nn() {
        mask |= sqlite3_where_expr_usage_nn(p, m, x.p_right);
    } else if x.use_x_select() {
        if x.has(EP_VarSelect) {
            m.b_var_select = true;
        }
        mask |= expr_select_usage(p, m, x.x_select());
    } else if x.x_list().nn() {
        mask |= sqlite3_where_expr_list_usage(p, m, x.x_list());
    }
    if (x.op == TK_FUNCTION || x.op == TK_AGG_FUNCTION) && x.has(EP_WinFunc) && x.y_win.nn() {
        let win = p.a_win[x.y_win.ix()].clone();
        mask |= sqlite3_where_expr_list_usage(p, m, win.p_partition);
        mask |= sqlite3_where_expr_list_usage(p, m, win.p_order_by);
        mask |= sqlite3_where_expr_usage(p, m, win.p_filter);
    }
    mask
}

/// `sqlite3WhereExprUsage(pMaskSet, p)`
pub fn sqlite3_where_expr_usage(p: &Parse, m: &mut WhereMaskSet, e: ExprId) -> Bitmask {
    if e.nn() { sqlite3_where_expr_usage_nn(p, m, e) } else { 0 }
}

/// `sqlite3WhereExprListUsage(pMaskSet, pList)`
pub fn sqlite3_where_expr_list_usage(p: &Parse, m: &mut WhereMaskSet, list: ExprListId) -> Bitmask {
    let mut mask = 0;
    if list.nn() {
        for i in 0..p.l(list).a.len() {
            mask |= sqlite3_where_expr_usage(p, m, p.l(list).a[i].p_expr);
        }
    }
    mask
}

/// `sqlite3WhereExprAnalyze(pTabList, pWC)`: call exprAnalyze on all terms in a WHERE clause.
///
/// Note that exprAnalyze() might add new virtual terms onto the end of the WHERE clause. We
/// do not want to analyze these new virtual terms, so start analyzing at the end and work
/// forward so that the added virtual terms are never processed.
pub fn sqlite3_where_expr_analyze(p: &mut Parse, w: &mut WhereInfo, wc: WcId) {
    let n = w.a_wc[wc].a.len();
    for i in (0..n).rev() {
        expr_analyze(p, w, wc, i);
    }
}

/// `whereAddLimitExpr(pWC, iReg, pExpr, iCsr, eMatchOp)`: add either a LIMIT (if
/// eMatchOp==SQLITE_INDEX_CONSTRAINT_LIMIT) or OFFSET (if
/// eMatchOp==SQLITE_INDEX_CONSTRAINT_OFFSET) term to the where-clause passed as the first
/// argument. The value for the term is found in register iReg.
///
/// In the common case where the value is a simple integer (example: "LIMIT 5 OFFSET 10")
/// then the expression codes as a TK_INTEGER so that it will be available to
/// sqlite3_vtab_rhs_value(). If not, then it codes as a TK_REGISTER expression.
fn where_add_limit_expr(p: &mut Parse, w: &mut WhereInfo, wc: WcId, i_reg: i32, e: ExprId, i_csr: i32, e_match_op: u8) {
    let new;
    match sqlite3_expr_is_integer(p, e, true, false) {
        Some(i_val) if i_val >= 0 => {
            let val = sqlite3_expr_int32(p, i_val);
            if val.is_null() {
                return;
            }
            new = sqlite3_p_expr(p, TK_MATCH, ExprId::NULL, val);
        }
        _ => {
            let val = sqlite3_expr_alloc(p, TK_REGISTER, None, 0);
            if val.is_null() {
                return;
            }
            p.em(val).i_table = i_reg;
            new = sqlite3_p_expr(p, TK_MATCH, ExprId::NULL, val);
        }
    }
    if new.nn() {
        let idx = where_clause_insert(p, w, wc, new, TERM_DYNAMIC | TERM_VIRTUAL);
        let t = &mut w.a_wc[wc].a[idx];
        t.left_cursor = i_csr;
        t.e_operator = WO_AUX;
        t.e_match_op = e_match_op;
    }
}

/// `sqlite3WhereAddLimit(pWC, p)`: possibly add terms corresponding to the LIMIT and OFFSET
/// clauses of the SELECT statement passed as the second argument. These terms are only added
/// if:
///
///   1. The SELECT statement has a LIMIT clause, and
///   2. The SELECT statement is not an aggregate or DISTINCT query, and
///   3. The SELECT statement has exactly one object in its FROM clause, and that object is a
///      virtual table, and
///   4. There are no terms in the WHERE clause that will not be passed to the virtual table
///      xBestIndex method.
///   5. The ORDER BY clause, if any, will be made available to the xBestIndex method.
///
/// LIMIT and OFFSET terms are ignored by most of the planner code. They exist only so that
/// they may be passed to the xBestIndex method of the single virtual table in the FROM clause
/// of the SELECT.
pub fn sqlite3_where_add_limit(p: &mut Parse, w: &mut WhereInfo, wc: WcId, sel: SelectId) {
    let src = p.s(sel).p_src;
    if p.s(sel).p_group_by.is_null()
        && (p.s(sel).sel_flags & (SF_Distinct | SF_Aggregate)) == 0 // 2
        && (p.src(src).a.len() == 1 && p.tab(p.src(src).a[0].p_s_tab).is_virtual())
    // 3
    {
        let order_by = p.s(sel).p_order_by;
        let i_csr = p.src(src).a[0].i_cursor;

        // Check condition (4). Return early if it is not met.
        for ii in 0..w.a_wc[wc].a.len() {
            let t = &w.a_wc[wc].a[ii];
            if t.wt_flags & TERM_CODED != 0 {
                // This term is a vector operation that has been decomposed into other,
                // subsequent terms. It can be ignored. See tag-20220128a
                continue;
            }
            if t.n_child != 0 {
                // If this term has child terms, then they are also part of the pWC->a[]
                // array. So this term can be ignored, as a LIMIT clause will only be added if
                // each of the child terms passes the (leftCursor==iCsr) test below.
                continue;
            }
            if t.left_cursor == i_csr && t.prereq_right == 0 {
                continue;
            }

            // If this term has a parent with exactly one child, and the parent will be passed
            // through to xBestIndex, then this term can be ignored.
            if t.i_parent >= 0 {
                let pp = &w.a_wc[wc].a[t.i_parent as usize];
                if pp.left_cursor == i_csr && pp.prereq_right == 0 && pp.n_child == 1 {
                    continue;
                }
            }

            // This term will not be passed through. Do not add a LIMIT clause.
            return;
        }

        // Check condition (5). Return early if it is not met.
        if order_by.nn() {
            for ii in 0..p.l(order_by).a.len() {
                let e = p.l(order_by).a[ii].p_expr;
                if p.e(e).op != TK_COLUMN {
                    return;
                }
                if p.e(e).i_table != i_csr {
                    return;
                }
                if p.l(order_by).a[ii].sort_flags & sqliters_vdbe::vdbeaux::KEYINFO_ORDER_BIGNULL != 0 {
                    return;
                }
            }
        }

        // All conditions are met. Add the terms to the where-clause object.
        let (i_offset, i_limit, sel_flags, lim) = {
            let s = p.s(sel);
            (s.i_offset, s.i_limit, s.sel_flags, s.p_limit)
        };
        if i_offset != 0 && (sel_flags & SF_Compound) == 0 {
            let r = p.e(lim).p_right;
            where_add_limit_expr(p, w, wc, i_offset, r, i_csr, crate::vtab::SQLITE_INDEX_CONSTRAINT_OFFSET);
        }
        if i_offset == 0 || (sel_flags & SF_Compound) == 0 {
            let l = p.e(lim).p_left;
            where_add_limit_expr(p, w, wc, i_limit, l, i_csr, crate::vtab::SQLITE_INDEX_CONSTRAINT_LIMIT);
        }
    }
}

/// `sqlite3WhereTabFuncArgs(pParse, pItem, pWC)`: for table-valued-functions, transform the
/// function arguments into new WHERE clause terms.
///
/// Each function argument translates into an equality constraint against a HIDDEN column in
/// the table.
pub fn sqlite3_where_tab_func_args(p: &mut Parse, w: &mut WhereInfo, wc: WcId, src: SrcListId, i: usize) {
    let (is_tab_func, tab, name, args, i_cursor, jointype) = {
        let it = &p.src(src).a[i];
        (it.is_tab_func, it.p_s_tab, it.z_name.clone().unwrap_or_default(), it.p_func_arg, it.i_cursor, it.jointype as i32)
    };
    if !is_tab_func {
        return;
    }
    if !p.tab(tab).is_virtual() {
        p.error_msg(format!("'{name}' is not a function"));
        return;
    }
    if args.is_null() {
        return;
    }
    let n_col = p.tab(tab).a_col.len();
    let mut k = 0usize;
    for j in 0..p.l(args).a.len() {
        while k < n_col && (p.tab(tab).a_col[k].col_flags & COLFLAG_HIDDEN as u16) == 0 {
            k += 1;
        }
        if k >= n_col {
            let tn = p.tab(tab).z_name.clone();
            p.error_msg(format!("too many arguments on {tn}() - max {j}"));
            return;
        }
        let col_ref = sqlite3_expr_alloc(p, TK_COLUMN, None, 0);
        if col_ref.is_null() {
            return;
        }
        {
            let c = p.em(col_ref);
            c.i_table = i_cursor;
            c.i_column = k as i16;
            c.y_tab = tab;
        }
        k += 1;
        let used = crate::resolve::sqlite3_expr_col_used(p, col_ref);
        p.srcm(src).a[i].col_used |= used;
        let a = p.l(args).a[j].p_expr;
        let d = sqlite3_expr_dup(p, a, 0);
        let rhs = sqlite3_p_expr(p, TK_UPLUS, d, ExprId::NULL);
        let term = sqlite3_p_expr(p, TK_EQ, col_ref, rhs);
        let join_type = if jointype & (JT_LEFT | JT_RIGHT) != 0 { EP_OuterON } else { EP_InnerON };
        crate::select::sqlite3_set_join_expr(p, term, i_cursor, join_type);
        where_clause_insert(p, w, wc, term, TERM_DYNAMIC);
    }
}

/// `sqlite3WhereLoopBloomable(pLoop)`: return true if pLoop is a candidate for a Bloom
/// filter: an IPK lookup, or an index whose first nEq columns all use the BINARY collation.
pub fn sqlite3_where_loop_bloomable(p: &Parse, l: &WhereLoop) -> bool {
    if l.ws_flags & WHERE_IPK as u32 != 0 {
        return true;
    }
    if (l.ws_flags & WHERE_INDEXED as u32) == 0 {
        return false;
    }
    crate::where_::sqlite3_index_bloomable(p, l.p_index, l.n_eq as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atof_validity() {
        assert!(atof_is_num(b"12"));
        assert!(atof_is_num(b" -1.5e3 "));
        assert!(atof_is_num(b".5"));
        assert!(atof_is_num(b"5."));
        assert!(!atof_is_num(b"abc"));
        assert!(!atof_is_num(b"1e"));
        assert!(!atof_is_num(b"1x"));
        assert!(!atof_is_num(b""));
        assert!(!atof_is_num(b"-"));
        assert!(!atof_is_num(b"."));
    }
}
