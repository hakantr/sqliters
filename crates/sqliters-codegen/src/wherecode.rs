//! `wherecode.c`: this module contains C code that generates VDBE code used to process the
//! WHERE clause of SQL statements. This file was split off from where.c on 2015-06-06 in
//! order to reduce the size of where.c and make it easier to edit. This file contains the
//! routines that actually generate the bulk of the WHERE loop code. The original where.c file
//! retains the code that does query planning and analysis.
//!
//! The virtual-table case of `sqlite3WhereCodeOneLoopStart()` is not part of this port (a
//! virtual table is reported as unsupported before the planner runs).

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::P4;

use crate::build::*;
use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::schema::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::where_::*;
use crate::whereint::*;

/// `%S` of `sqlite3_str_appendf()` (printf.c, `etSRCITEM`): the name of a FROM-clause term
/// for EXPLAIN QUERY PLAN and error messages. `alt2` is the `!` flag (`%!S`): ignore the
/// alias.
pub fn src_item_name(p: &Parse, src: SrcListId, i: usize, alt2: bool) -> String {
    let it = &p.src(src).a[i];
    if let Some(a) = &it.z_alias
        && !alt2
    {
        return a.clone();
    }
    if let Some(n) = &it.z_name {
        let mut s = String::new();
        if !it.fixed_schema
            && !it.is_subquery
            && let Some(d) = &it.z_database
        {
            s.push_str(d);
            s.push('.');
        }
        s.push_str(n);
        return s;
    }
    if let Some(a) = &it.z_alias {
        return a.clone();
    }
    if it.is_subquery {
        let sel = it.p_subq.as_ref().expect("subquery").p_select;
        let s = p.s(sel);
        if s.sel_flags & SF_NestedFrom != 0 {
            return format!("(join-{})", s.sel_id);
        } else if s.sel_flags & SF_MultiValue != 0 {
            return format!("{}-ROW VALUES CLAUSE", it.n_row);
        } else {
            return format!("(subquery-{})", s.sel_id);
        }
    }
    String::new()
}

/// `explainIndexColumnName(pIdx, i)`: return the name of the i-th column of the pIdx index.
fn explain_index_column_name(p: &Parse, idx: IndexId, i: usize) -> String {
    let c = p.idx(idx).ai_column[i] as i32;
    if c == XN_EXPR {
        return "<expr>".to_owned();
    }
    if c == XN_ROWID {
        return "rowid".to_owned();
    }
    p.tab(p.idx(idx).p_table).a_col[c as usize].z_cn_name.clone()
}

/// `explainAppendTerm(pStr, pIdx, nTerm, iTerm, bAnd, zOp)`: this routine is a helper for
/// explainIndexRange() below
///
/// pStr holds the text of an expression that we are building up one term at a time. This
/// routine adds a new term to the end of the expression. Terms are separated by AND so add
/// the "AND" text for second and subsequent terms only.
fn explain_append_term(p: &Parse, s: &mut String, idx: IndexId, n_term: usize, i_term: usize, b_and: bool, z_op: &str) {
    if b_and {
        s.push_str(" AND ");
    }
    if n_term > 1 {
        s.push('(');
    }
    for i in 0..n_term {
        if i != 0 {
            s.push(',');
        }
        s.push_str(&explain_index_column_name(p, idx, i_term + i));
    }
    if n_term > 1 {
        s.push(')');
    }
    s.push_str(z_op);
    if n_term > 1 {
        s.push('(');
    }
    for i in 0..n_term {
        if i != 0 {
            s.push(',');
        }
        s.push('?');
    }
    if n_term > 1 {
        s.push(')');
    }
}

/// `explainIndexRange(pStr, pLoop)`: argument pLevel describes a strategy for scanning table
/// pTab. This function appends text to pStr that describes the subset of table rows scanned
/// by the strategy in the form of an SQL expression.
///
/// For example, if the query:
///
///   SELECT * FROM t1 WHERE a=1 AND b>2;
///
/// is run and there is an index on (a, b), then this function returns a string similar to:
///
///   "a=? AND b>?"
fn explain_index_range(p: &Parse, s: &mut String, l: &WhereLoop) {
    let index = l.p_index;
    let n_eq = l.n_eq as usize;
    let n_skip = l.n_skip as usize;
    if n_eq == 0 && (l.ws_flags & (WHERE_BTM_LIMIT | WHERE_TOP_LIMIT) as u32) == 0 {
        return;
    }
    s.push_str(" (");
    let mut i = 0;
    while i < n_eq {
        let z = explain_index_column_name(p, index, i);
        if i != 0 {
            s.push_str(" AND ");
        }
        if i >= n_skip {
            s.push_str(&format!("{z}=?"));
        } else {
            s.push_str(&format!("ANY({z})"));
        }
        i += 1;
    }
    let j = i;
    let mut b_and = i != 0;
    if l.ws_flags & WHERE_BTM_LIMIT as u32 != 0 {
        explain_append_term(p, s, index, l.n_btm as usize, j, b_and, ">");
        b_and = true;
    }
    if l.ws_flags & WHERE_TOP_LIMIT as u32 != 0 {
        explain_append_term(p, s, index, l.n_top as usize, j, b_and, "<");
    }
    s.push(')');
}

/// `sqlite3WhereAddExplainText(pParse, addr, pTabList, pLevel, wctrlFlags)`: this function
/// sets the P4 value of an existing OP_Explain opcode to text describing the loop in pLevel.
/// If the OP_Explain opcode already has a P4 value, it is freed before it is overwritten.
pub fn sqlite3_where_add_explain_text(p: &mut Parse, w: &WhereInfo, addr: i32, tab_list: SrcListId, i_level: usize, wctrl_flags: u16) {
    if p.explain != 2 {
        return;
    }
    let level = &w.a[i_level];
    let i_from = level.i_from as usize;
    let l = &w.loops[level.p_w_loop];
    let flags = l.ws_flags;
    let is_search = (flags & (WHERE_BTM_LIMIT | WHERE_TOP_LIMIT) as u32) != 0
        || ((flags & WHERE_VIRTUALTABLE as u32) == 0 && l.n_eq > 0)
        || (wctrl_flags as i32 & (WHERE_ORDERBY_MIN | WHERE_ORDERBY_MAX)) != 0;
    let (from_exists, tab, jointype) = {
        let it = &p.src(tab_list).a[i_from];
        (it.from_exists, it.p_s_tab, it.jointype as i32)
    };
    let mut s = format!(
        "{} {}{}",
        if is_search { "SEARCH" } else { "SCAN" },
        src_item_name(p, tab_list, i_from, false),
        if from_exists { " EXISTS" } else { "" }
    );
    if (flags & (WHERE_IPK | WHERE_VIRTUALTABLE) as u32) == 0 {
        let idx = l.p_index;
        let z_fmt: Option<String>;
        if !p.tab(tab).has_rowid() && p.idx(idx).is_primary_key() {
            z_fmt = if is_search { Some("PRIMARY KEY".to_owned()) } else { None };
        } else if flags & WHERE_PARTIALIDX as u32 != 0 {
            z_fmt = Some("AUTOMATIC PARTIAL COVERING INDEX".to_owned());
        } else if flags & WHERE_AUTO_INDEX as u32 != 0 {
            z_fmt = Some("AUTOMATIC COVERING INDEX".to_owned());
        } else if flags & (WHERE_IDX_ONLY | WHERE_EXPRIDX) as u32 != 0 {
            z_fmt = Some(format!("COVERING INDEX {}", p.idx(idx).z_name));
        } else {
            z_fmt = Some(format!("INDEX {}", p.idx(idx).z_name));
        }
        if let Some(f) = z_fmt {
            s.push_str(" USING ");
            s.push_str(&f);
            explain_index_range(p, &mut s, l);
        }
    } else if (flags & WHERE_IPK as u32) != 0 && (flags & WHERE_CONSTRAINT as u32) != 0 {
        let z_rowid = "rowid";
        s.push_str(&format!(" USING INTEGER PRIMARY KEY ({z_rowid}"));
        let c_range_op;
        if flags & (WHERE_COLUMN_EQ | WHERE_COLUMN_IN) as u32 != 0 {
            c_range_op = '=';
        } else if (flags & WHERE_BOTH_LIMIT as u32) == WHERE_BOTH_LIMIT as u32 {
            s.push_str(&format!(">? AND {z_rowid}"));
            c_range_op = '<';
        } else if flags & WHERE_BTM_LIMIT as u32 != 0 {
            c_range_op = '>';
        } else {
            c_range_op = '<';
        }
        s.push_str(&format!("{c_range_op}?)"));
    }
    if jointype & JT_LEFT != 0 {
        s.push_str(" LEFT-JOIN");
    }
    p.v.change_p4(addr, P4::dynamic(&s));
}

/// `sqlite3WhereExplainOneScan(pParse, pTabList, pLevel, wctrlFlags)`: this function is a
/// no-op unless currently processing an EXPLAIN QUERY PLAN command, or if stmt_scanstatus
/// has been enabled. If the query being compiled is an EXPLAIN QUERY PLAN, a single record
/// is added to the output to describe the table scan strategy in pLevel.
///
/// If an OP_Explain opcode is added to the VM, its address is returned. Otherwise, if no
/// OP_Explain is coded, zero is returned.
pub fn sqlite3_where_explain_one_scan(p: &mut Parse, w: &WhereInfo, tab_list: SrcListId, i_level: usize, wctrl_flags: u16) -> i32 {
    let mut ret = 0;
    if p.explain == 2 {
        let l = &w.loops[w.a[i_level].p_w_loop];
        if (l.ws_flags & WHERE_MULTI_OR as u32) == 0 && (wctrl_flags as i32 & WHERE_OR_SUBCLAUSE) == 0 {
            let addr = p.v.current_addr();
            let r_run = l.r_run as i32;
            ret = p.v.add_op3(OP_Explain, addr, p.addr_explain, r_run);
            sqlite3_where_add_explain_text(p, w, addr, tab_list, i_level, wctrl_flags);
        }
    }
    ret
}

/// `sqlite3WhereExplainBloomFilter(pParse, pWInfo, pLevel)`: add a single OP_Explain opcode
/// that describes a Bloom filter.
///
/// Or if not processing EXPLAIN QUERY PLAN and not in a SQLITE_DEBUG and/or
/// SQLITE_ENABLE_STMT_SCANSTATUS build, then OP_Explain opcodes are not required and so this
/// routine is a no-op.
///
/// If an OP_Explain opcode is added to the VM, its address is returned. Otherwise, if no
/// OP_Explain is coded, zero is returned.
pub fn sqlite3_where_explain_bloom_filter(p: &mut Parse, w: &WhereInfo, i_level: usize) -> i32 {
    // (C adds this OP_Explain unconditionally: it is also part of the program of a statement
    // that is not an EXPLAIN QUERY PLAN)
    let level = &w.a[i_level];
    let i_from = level.i_from as usize;
    let l = &w.loops[level.p_w_loop];
    let mut s = format!("BLOOM FILTER ON {} (", src_item_name(p, w.p_tab_list, i_from, false));
    if l.ws_flags & WHERE_IPK as u32 != 0 {
        let tab = p.tab(p.src(w.p_tab_list).a[i_from].p_s_tab);
        if tab.i_p_key >= 0 {
            s.push_str(&format!("{}=?", tab.a_col[tab.i_p_key as usize].z_cn_name));
        } else {
            s.push_str("rowid=?");
        }
    } else {
        for i in l.n_skip as usize..l.n_eq as usize {
            let z = explain_index_column_name(p, l.p_index, i);
            if i > l.n_skip as usize {
                s.push_str(" AND ");
            }
            s.push_str(&format!("{z}=?"));
        }
    }
    s.push(')');
    let addr = p.v.current_addr();
    p.v.add_op4(OP_Explain, addr, p.addr_explain, 0, P4::dynamic(&s))
}

/// `disableTerm(pLevel, pTerm)`: disable a term in the WHERE clause. Except, do not disable
/// the term if it controls a LEFT OUTER JOIN and it did not originate in the ON or USING
/// clause of that join.
///
/// Consider the term t2.z='ok' in the following queries:
///
///   (1)  SELECT * FROM t1 LEFT JOIN t2 ON t1.a=t2.x WHERE t2.z='ok'
///   (2)  SELECT * FROM t1 LEFT JOIN t2 ON t1.a=t2.x AND t2.z='ok'
///   (3)  SELECT * FROM t1, t2 WHERE t1.a=t2.x AND t2.z='ok'
///
/// The t2.z='ok' is disabled in the in (2) because it originates in the ON clause. The term
/// is disabled in (3) because it is not part of a LEFT OUTER JOIN. In (1), the term is not
/// disabled.
///
/// Disabling a term causes that term to not be tested in the inner loop of the join.
/// Disabling is an optimization. When terms are satisfied by indices, we disable them to
/// prevent redundant tests in the inner loop. We would get the correct results if nothing
/// were ever disabled, but joins might run a little slower. The trick is to disable as much
/// as we can without disabling too much. If we disabled in (1), we'd get the wrong answer.
/// See ticket #813.
///
/// If all the children of a term are disabled, then that term is also automatically
/// disabled. In this way, terms get disabled if derived virtual terms are tested first. For
/// example:
///
///      x GLOB 'abc*' AND x>='abc' AND x<'acd'
///      \___________/     \______/     \_____/
///         parent          child1       child2
///
/// Only the parent term was in the original WHERE clause. The child1 and child2 terms were
/// added by the LIKE optimization. If both of the virtual child terms are valid, then
/// testing of the parent can be skipped.
///
/// Usually the parent term is marked as TERM_CODED. But if the parent term was originally
/// TERM_LIKE, then the parent gets TERM_LIKECOND instead. The TERM_LIKECOND marking
/// indicates that the term should be coded inside a conditional such that is only evaluated
/// on the second pass of a LIKE-optimization loop, when scanning BLOBs.
fn disable_term(p: &Parse, w: &mut WhereInfo, i_level: usize, mut t: TermRef) {
    let mut n_loop = 0;
    loop {
        let (wt_flags, e, prereq_all, i_parent) = {
            let term = w.term(t);
            (term.wt_flags, term.p_expr, term.prereq_all, term.i_parent)
        };
        if !((wt_flags & TERM_CODED) == 0
            && (w.a[i_level].i_left_join == 0 || p.e(e).has(EP_OuterON))
            && (w.a[i_level].not_ready & prereq_all) == 0)
        {
            break;
        }
        if n_loop != 0 && (wt_flags & TERM_LIKE) != 0 {
            w.term_mut(t).wt_flags |= TERM_LIKECOND;
        } else {
            w.term_mut(t).wt_flags |= TERM_CODED;
        }
        if i_parent < 0 {
            break;
        }
        t = TermRef { wc: t.wc, i: i_parent as u32 };
        let parent = w.term_mut(t);
        parent.n_child = parent.n_child.wrapping_sub(1);
        if parent.n_child != 0 {
            break;
        }
        n_loop += 1;
    }
}

/// `codeApplyAffinity(pParse, base, n, zAff)`: code an OP_Affinity opcode to apply the
/// column affinity string zAff to the n registers starting at base.
///
/// As an optimization, SQLITE_AFF_BLOB and SQLITE_AFF_NONE entries (which are no-ops) at the
/// beginning and end of zAff are ignored. If all entries in zAff are SQLITE_AFF_BLOB or
/// SQLITE_AFF_NONE, then no code gets generated.
///
/// This routine makes its own copy of zAff so that the caller is free to modify zAff after
/// this routine returns.
fn code_apply_affinity(p: &mut Parse, mut base: i32, mut n: i32, z_aff: &[u8]) {
    let mut z = z_aff;
    // Adjust base and n to skip over SQLITE_AFF_BLOB and SQLITE_AFF_NONE entries at the
    // beginning and end of the affinity string.
    while n > 0 && !z.is_empty() && z[0] <= SQLITE_AFF_BLOB {
        n -= 1;
        base += 1;
        z = &z[1..];
    }
    while n > 1 && z[n as usize - 1] <= SQLITE_AFF_BLOB {
        n -= 1;
    }

    // Code the OP_Affinity opcode if there is anything left to do.
    if n > 0 {
        p.v.add_op4(OP_Affinity, base, n, 0, P4::Dynamic(z[..n as usize].to_vec()));
    }
}

/// `updateRangeAffinityStr(pRight, n, zAff)`: expression pRight, which is the RHS of a
/// comparison operation, is either a vector of n elements or, if n==1, a scalar expression.
/// Before the comparison operation, affinity zAff is to be applied to the pRight values.
/// This function modifies characters within the affinity string to SQLITE_AFF_BLOB if either:
///
///   * the comparison will be performed with no affinity, or
///   * the affinity change in zAff is guaranteed not to change the value.
fn update_range_affinity_str(p: &Parse, right: ExprId, n: i32, z_aff: &mut [u8]) {
    for i in 0..n as usize {
        let e = sqlite3_vector_field_subexpr(p, right, i as i32);
        if sqlite3_compare_affinity(p, e, z_aff[i]) == SQLITE_AFF_BLOB || sqlite3_expr_needs_no_affinity_change(p, e, z_aff[i]) {
            z_aff[i] = SQLITE_AFF_BLOB;
        }
    }
}

/// `adjustOrderByCol(pOrderBy, pEList)`: the pOrderBy->a[].u.x.iOrderByCol values might be
/// incorrect because columns might have been rearranged in the result set. This routine
/// fixes them up.
///
/// pEList is the new result set. The pEList->a[].u.x.iOrderByCol values contain the original
/// (1-based) column number.
fn adjust_order_by_col(p: &mut Parse, order_by: ExprListId, e_list: ExprListId) {
    if order_by.is_null() {
        return;
    }
    for i in 0..p.l(order_by).a.len() {
        let t = p.l(order_by).a[i].i_order_by_col;
        if t == 0 {
            continue;
        }
        let n = p.l(e_list).a.len();
        let mut j = 0;
        while j < n {
            if p.l(e_list).a[j].i_order_by_col == t {
                p.lm(order_by).a[i].i_order_by_col = j as u16 + 1;
                break;
            }
            j += 1;
        }
        if j >= n {
            p.lm(order_by).a[i].i_order_by_col = 0;
        }
    }
}

/// `removeUnindexableInClauseTerms(pParse, iEq, pLoop, pX)`: pX is an expression of the form:
/// (vector) IN (SELECT ...). In other words, it is a vector IN operator with a SELECT clause
/// on the LHS. But not all terms in the vector are indexable and the terms might not be in
/// the correct order for indexing.
///
/// This routine makes a copy of the input pX expression and then adjusts the vector in the
/// copy so that it contains only index terms and those terms are in the correct order. The
/// modified vector IN expression is returned. The caller is responsible for deleting the
/// returned expression.
///
/// Example:
///
///    CREATE TABLE t1(a,b,c,d,e,f);
///    CREATE INDEX t1x1 ON t1(e,c);
///    SELECT * FROM t1 WHERE (a,b,c,d,e) IN (SELECT v,w,x,y,z FROM t2)
///                           \_______________________________________/
///                                     The pX expression
///
/// Since only columns e and c can be used with the index, in that order, the modified IN
/// expression that is returned will be:
///
///        (e,c) IN (SELECT z,x FROM t2)
///
/// The reduced pX is different from the original (obviously) and thus is only used for
/// indexing, to improve performance. The original unaltered IN expression must also be run
/// on each output row for correctness.
fn remove_unindexable_in_clause_terms(p: &mut Parse, w: &WhereInfo, i_eq: usize, i_loop: usize, x: ExprId) -> ExprId {
    let new = sqlite3_expr_dup(p, x, 0);
    let mut sel = p.e(new).x_select();
    let first = sel;
    while sel.nn() {
        let orig_rhs = p.s(sel).p_e_list; // Original unmodified RHS
        let orig_lhs = if sel == first { p.e(p.e(new).p_left).x_list() } else { ExprListId::NULL }; // Original unmodified LHS
        let mut rhs = ExprListId::NULL; // New RHS after modifications
        let mut lhs = ExprListId::NULL; // New LHS after mods
        let mut n_rhs = 0; // Number of RHS terms added so far
        let l = &w.loops[i_loop];
        for i in i_eq..l.n_l_term() {
            let t = l.a_l_term[i].expect("term");
            if w.term(t).p_expr == x {
                let i_field = (w.term(t).i_field - 1) as usize;
                if i_field as i32 != n_rhs {
                    p.em(new).flags &= !EP_Subrtn;
                }
                let re = p.l(orig_rhs).a[i_field].p_expr;
                if re.is_null() {
                    continue; // Duplicate PK column
                }
                rhs = sqlite3_expr_list_append(p, rhs, re);
                p.lm(orig_rhs).a[i_field].p_expr = ExprId::NULL;
                if rhs.nn() {
                    p.lm(rhs).a.last_mut().expect("item").i_order_by_col = i_field as u16 + 1;
                }
                if orig_lhs.nn() {
                    let le = p.l(orig_lhs).a[i_field].p_expr;
                    lhs = sqlite3_expr_list_append(p, lhs, le);
                    p.lm(orig_lhs).a[i_field].p_expr = ExprId::NULL;
                }
                n_rhs += 1;
            }
        }
        if orig_lhs.nn() {
            let left = p.e(new).p_left;
            p.em(left).x = lhs.0;
        }
        p.sm(sel).p_e_list = rhs;
        p.n_select += 1;
        p.sm(sel).sel_id = p.n_select as u32; // Req'd for SubrtnSig validity
        if lhs.nn() && p.l(lhs).n_expr() == 1 {
            // Take care here not to generate a TK_VECTOR containing only a single value.
            // This is not permitted by the parser. If any other code is subsequently added
            // to this function, where it might be run on a vector, it should check for this
            // case: a vector of size one.
            let e0 = p.l(lhs).a[0].p_expr;
            p.lm(lhs).a[0].p_expr = ExprId::NULL;
            p.em(new).p_left = e0;
        }

        // If either the ORDER BY clause or the GROUP BY clause contains references to result
        // set columns, those references might now be obsolete. So fix them up.
        if rhs.nn() {
            let (ob, gb) = (p.s(sel).p_order_by, p.s(sel).p_group_by);
            adjust_order_by_col(p, ob, rhs);
            adjust_order_by_col(p, gb, rhs);
            for it in p.lm(rhs).a.iter_mut() {
                it.i_order_by_col = 0;
            }
        }
        sel = p.s(sel).p_prior;
    }
    new
}

/// `codeINTerm(pParse, pTerm, pLevel, iEq, bRev, iTarget)`: generate code for a single
/// X IN (....) term of the WHERE clause.
///
/// This is a special-case of codeEqualityTerm() that works for IN operators only. It is
/// broken out into a subroutine because this case is uncommon and by splitting it off into a
/// subroutine, the common case runs faster.
///
/// The current value for the constraint is left in register iTarget. This routine sets up a
/// loop that will iterate over all values of X.
#[allow(unused_assignments)] // (eType is initialized as in C)
fn code_in_term(p: &mut Parse, w: &mut WhereInfo, t: TermRef, i_level: usize, i_eq: usize, mut b_rev: bool, i_target: i32) {
    let x = w.term(t).p_expr;
    let mut e_type = IN_INDEX_NOOP;
    let mut i_tab = 0;
    let i_loop = w.a[i_level].p_w_loop;
    let mut n_eq = 0;
    let mut ai_map: Option<Vec<i32>> = None;

    {
        let l = &w.loops[i_loop];
        if (l.ws_flags & WHERE_VIRTUALTABLE as u32) == 0 && l.p_index.nn() && p.idx(l.p_index).a_sort_order[i_eq] != 0 {
            b_rev = !b_rev;
        }
    }
    for i in 0..i_eq {
        if let Some(ti) = w.loops[i_loop].a_l_term[i]
            && w.term(ti).p_expr == x
        {
            disable_term(p, w, i_level, t);
            return;
        }
    }
    for i in i_eq..w.loops[i_loop].n_l_term() {
        let ti = w.loops[i_loop].a_l_term[i].expect("term");
        if w.term(ti).p_expr == x {
            n_eq += 1;
        }
    }

    if !p.e(x).use_x_select() || p.l(p.s(p.e(x).x_select()).p_e_list).n_expr() == 1 {
        e_type = sqlite3_find_in_index(p, x, IN_INDEX_LOOP as u32, None, None, &mut i_tab);
    } else {
        let x_mod = remove_unindexable_in_clause_terms(p, w, i_eq, i_loop, x);
        if n_eq > 1 {
            // If this IN(SELECT ...) expression drives more than one column of the index,
            // then OP_SeekScan is not usable: the IN-loop must visit every combination.
            w.loops[i_loop].ws_flags &= !(WHERE_IN_SEEKSCAN as u32);
        }
        let n_col = p.l(p.s(p.e(x).x_select()).p_e_list).n_expr();
        let mut map = vec![0i32; n_col as usize];
        e_type = sqlite3_find_in_index(p, x_mod, IN_INDEX_LOOP as u32, None, Some(&mut map), &mut i_tab);
        ai_map = Some(map);
    }

    if e_type == IN_INDEX_INDEX_DESC {
        b_rev = !b_rev;
    }
    p.v.add_op2(if b_rev { OP_Last } else { OP_Rewind }, i_tab, 0);

    w.loops[i_loop].ws_flags |= WHERE_IN_ABLE as u32;
    if w.a[i_level].a_in_loop.is_empty() {
        w.a[i_level].addr_nxt = p.v.make_label();
    }
    if i_eq > 0 && (w.loops[i_loop].ws_flags & WHERE_IN_SEEKSCAN as u32) == 0 {
        w.loops[i_loop].ws_flags |= WHERE_IN_EARLYOUT as u32;
    }

    let mut i_map = 0usize; // Index in aiMap[]
    for i in i_eq..w.loops[i_loop].n_l_term() {
        let ti = w.loops[i_loop].a_l_term[i].expect("term");
        if w.term(ti).p_expr == x {
            let i_out = i_target + (i - i_eq) as i32;
            let mut p_in = InLoop::default();
            if e_type == IN_INDEX_ROWID {
                p_in.addr_in_top = p.v.add_op2(OP_Rowid, i_tab, i_out);
            } else {
                let i_col = match &ai_map {
                    Some(m) => {
                        let c = m[i_map];
                        i_map += 1;
                        c
                    }
                    None => 0,
                };
                p_in.addr_in_top = p.v.add_op3(OP_Column, i_tab, i_col, i_out);
            }
            p.v.add_op1(OP_IsNull, i_out);
            if i == i_eq {
                p_in.i_cur = i_tab;
                p_in.e_end_loop_op = if b_rev { OP_Prev } else { OP_Next };
                if i_eq > 0 {
                    p_in.i_base = i_target - i as i32;
                    p_in.n_prefix = i as i32;
                } else {
                    p_in.n_prefix = 0;
                }
            } else {
                p_in.e_end_loop_op = OP_Noop;
            }
            w.a[i_level].a_in_loop.push(p_in);
        }
    }
    if i_eq > 0 && (w.loops[i_loop].ws_flags & (WHERE_IN_SEEKSCAN | WHERE_VIRTUALTABLE) as u32) == 0 {
        p.v.add_op3(OP_SeekHit, w.a[i_level].i_idx_cur, 0, i_eq as i32);
    }
}

/// `codeEqualityTerm(pParse, pTerm, pLevel, iEq, bRev, iTarget)`: generate code for a single
/// equality term of the WHERE clause. An equality term can be either X=expr or X IN (...).
/// pTerm is the term to be coded.
///
/// The current value for the constraint is left in a register, the index of which is
/// returned. An attempt is made store the result in iTarget but this is only guaranteed for
/// TK_ISNULL and TK_IN constraints. If the constraint is a TK_EQ or TK_IS, then the current
/// value might be left in some other register and it is the caller's responsibility to
/// compensate.
///
/// For a constraint of the form X=expr, the expression is evaluated in straight-line code.
/// For constraints of the form X IN (...) this routine sets up a loop that will iterate over
/// all values of X.
fn code_equality_term(p: &mut Parse, w: &mut WhereInfo, t: TermRef, i_level: usize, i_eq: usize, b_rev: bool, i_target: i32) -> i32 {
    let x = w.term(t).p_expr;
    let i_reg; // Register holding results
    if p.e(x).op == TK_EQ || p.e(x).op == TK_IS {
        let r = p.e(x).p_right;
        i_reg = sqlite3_expr_code_target(p, r, i_target);
    } else if p.e(x).op == TK_ISNULL {
        i_reg = i_target;
        p.v.add_op2(OP_Null, 0, i_reg);
    } else {
        i_reg = i_target;
        code_in_term(p, w, t, i_level, i_eq, b_rev, i_target);
    }

    // As an optimization, try to disable the WHERE clause term that is driving the index as
    // it will always be true. The correct answer is obtained regardless, but we might get
    // the answer with fewer CPU cycles by omitting the unnecessary comparison operation
    // encoded by pTerm->pExpr.
    //
    // But do not disable the term unless we are certain that the term is not a transitive
    // constraint. For an example of where that does not work, see
    // https://sqlite.org/forum/forumpost/eb8613976a (2021-05-04)
    let l_flags = w.loops[w.a[i_level].p_w_loop].ws_flags;
    if (l_flags & WHERE_TRANSCONS as u32) == 0 || (w.term(t).e_operator & WO_EQUIV) == 0 {
        disable_term(p, w, i_level, t);
    }
    i_reg
}

/// `codeAllEqualityTerms(pParse, pLevel, bRev, nExtraReg, pzAff)`: generate code that will
/// evaluate all == and IN constraints for an index scan.
///
/// For example, consider table t1(a,b,c,d,e,f) with index i1(a,b,c). Suppose the WHERE
/// clause is this:  a==5 AND b IN (1,2,3) AND c>5 AND c<10 The index has as many as three
/// equality constraints, but in this example, the third "c" value is an inequality. So only
/// two constraints are coded. This routine will generate code to evaluate a==5 and b IN
/// (1,2,3). The current values for a and b will be stored in consecutive registers and the
/// index of the first register is returned.
///
/// In the example above nEq==2. But this subroutine works for any value of nEq including 0.
/// If nEq==0, this routine is nearly a no-op. The only thing it does is allocate the
/// pLevel->iMem memory cell and compute the affinity string.
///
/// The nExtraReg parameter is 0 or 1. It is 0 if all WHERE clause constraints are coded.
/// This routine will allocate and initialize nEq+nExtraReg registers ...
///
/// Before returning, the affinity string is returned: a copy of the affinity string of the
/// index with entries modified to SQLITE_AFF_BLOB where no conversion is needed.
fn code_all_equality_terms(p: &mut Parse, w: &mut WhereInfo, i_level: usize, b_rev: bool, n_extra_reg: i32) -> (i32, Vec<u8>) {
    // This module is only called on query plans that use an index.
    let i_loop = w.a[i_level].p_w_loop;
    let n_eq = w.loops[i_loop].n_eq as usize;
    let n_skip = w.loops[i_loop].n_skip as usize;
    let idx = w.loops[i_loop].p_index;

    // Figure out how many memory cells we will need then allocate them.
    let reg_base = p.n_mem + 1;
    let n_reg = n_eq as i32 + n_extra_reg;
    p.n_mem += n_reg;

    let mut z_aff = sqlite3_index_affinity_str(p, idx);

    if n_skip != 0 {
        let i_idx_cur = w.a[i_level].i_idx_cur;
        p.v.add_op3(OP_Null, 0, reg_base, reg_base + n_skip as i32 - 1);
        p.v.add_op1(if b_rev { OP_Last } else { OP_Rewind }, i_idx_cur);
        let j = p.v.add_op0(OP_Goto);
        w.a[i_level].addr_skip = p.v.add_op4_int(if b_rev { OP_SeekLT } else { OP_SeekGT }, i_idx_cur, 0, reg_base, n_skip as i32);
        p.v.jump_here(j);
        for j in 0..n_skip as i32 {
            p.v.add_op3(OP_Column, i_idx_cur, j, reg_base + j);
        }
    }

    // Evaluate the equality constraints
    for j in n_skip..n_eq {
        let t = w.loops[i_loop].a_l_term[j].expect("term");
        // The following testcase is true for indices with redundant columns. Ex: CREATE
        // INDEX i1 ON t1(a,b,a); SELECT * FROM t1 WHERE a=0 AND b=0;
        let r1 = code_equality_term(p, w, t, i_level, j, b_rev, reg_base + j as i32);
        if r1 != reg_base + j as i32 {
            p.v.add_op2(OP_Copy, r1, reg_base + j as i32);
        }
        let (e_operator, wt_flags, te) = {
            let term = w.term(t);
            (term.e_operator, term.wt_flags, term.p_expr)
        };
        if e_operator & WO_IN != 0 {
            if p.e(te).flags & EP_xIsSelect != 0 {
                // No affinity ever needs to be (or should be) applied to a value from the
                // RHS of an "? IN (SELECT ...)" expression. The sub-select itself was coded
                // to apply the required affinity.
                z_aff[j] = SQLITE_AFF_BLOB;
            }
        } else if (e_operator & WO_ISNULL) == 0 {
            let right = p.e(te).p_right;
            if (wt_flags & TERM_IS) == 0 && sqlite3_expr_can_be_null(p, right) {
                p.v.add_op2(OP_IsNull, reg_base + j as i32, w.a[i_level].addr_brk);
            }
            if p.n_err == 0 {
                if sqlite3_compare_affinity(p, right, z_aff[j]) == SQLITE_AFF_BLOB {
                    z_aff[j] = SQLITE_AFF_BLOB;
                }
                if sqlite3_expr_needs_no_affinity_change(p, right, z_aff[j]) {
                    z_aff[j] = SQLITE_AFF_BLOB;
                }
            }
        }
    }
    (reg_base, z_aff)
}

/// `whereLikeOptimizationStringFixup(v, pLevel, pTerm)`: if the most recently coded
/// instruction is a constant range constraint (a string literal) that originated from the
/// LIKE optimization, then set P3 and P5 on the OP_String8 instruction so that the string
/// will be cast into a BLOB if and only if the LIKE counter register (P3) is odd. See the
/// LIKE counter discussion in sqlite3WhereCodeOneLoopStart().
fn where_like_optimization_string_fixup(p: &mut Parse, w: &WhereInfo, i_level: usize, t: TermRef) {
    if w.term(t).wt_flags & TERM_LIKEOPT != 0 {
        let cntr = w.a[i_level].i_like_rep_cntr;
        let op = p.v.get_last_op_mut();
        op.p3 = (cntr >> 1) as i32; // Register holding counter
        op.p5 = (cntr & 1) as u16; // ASC or DESC
    }
}

/// `codeDeferredSeek(pWInfo, pIdx, iCur, iIdxCur)`: cursor iCur is open on an intkey b-tree
/// (a table). Register iRowid contains a rowid value just read from cursor iIdxCur, open on
/// index pIdx. This function generates code to do a deferred seek of cursor iCur to iRowid
/// (using OP_DeferredSeek).
///
/// Normally, this is just:
///
///   OP_DeferredSeek $iCur $iRowid
///
/// Which causes a seek on $iCur to the row with rowid $iRowid.
///
/// However, if the scan currently being coded is a branch of an OR-loop and the statement
/// currently being coded is a SELECT, then additional information is added that might allow
/// OP_Column to omit the seek and instead do its lookup on the index, thus avoiding an
/// expensive seek operation. If P3 is non-zero, it means that the index is using an
/// expression index... P4 is an array of integers, where the first is the number of columns
/// in the table, and the K-th entry is the index column that holds table column K (or zero).
fn code_deferred_seek(p: &mut Parse, w: &mut WhereInfo, idx: IndexId, i_cur: i32, i_idx_cur: i32) {
    w.b_deferred_seek = true;
    p.v.add_op3(OP_DeferredSeek, i_idx_cur, 0, i_cur);
    if (w.wctrl_flags as i32 & (WHERE_OR_SUBCLAUSE | WHERE_RIGHT_JOIN)) != 0 && p.write_mask == 0 {
        let tab = p.idx(idx).p_table;
        let n_col = p.tab(tab).a_col.len();
        // ai[0] = pTab->nCol; the elements follow
        let mut ai = vec![0u32; n_col];
        for i in 0..p.idx(idx).n_column as usize - 1 {
            let x1 = p.idx(idx).ai_column[i] as i32;
            let x2 = sqlite3_table_column_to_storage(p.tab(tab), x1);
            if x1 >= 0 {
                ai[x2 as usize] = i as u32 + 1;
            }
        }
        p.v.change_p4(-1, P4::IntArray(ai));
    }
}

/// `codeExprOrVector(pParse, p, iReg, nReg)`: if the expression passed as the second argument
/// is a vector, generate code to write the first nReg elements of the vector into an array
/// of registers starting with iReg.
///
/// If the expression is not a vector, then nReg must be passed 1. In this case, generate
/// code to evaluate the expression and leave the result in register iReg.
fn code_expr_or_vector(p: &mut Parse, e: ExprId, i_reg: i32, n_reg: i32) {
    if e.nn() && sqlite3_expr_is_vector(p, e) {
        if p.e(e).use_x_select() {
            let i_select = sqlite3_code_subselect(p, e);
            p.v.add_op3(OP_Copy, i_select, i_reg, n_reg - 1);
        } else {
            let list = p.e(e).x_list();
            for i in 0..n_reg {
                let x = p.l(list).a[i as usize].p_expr;
                sqlite3_expr_code(p, x, i_reg + i);
            }
        }
    } else {
        sqlite3_expr_code(p, e, i_reg);
    }
}

/// `whereApplyPartialIndexConstraints(pTruth, iTabCur, pWC)`: the pTruth expression is always
/// true because it is the WHERE clause a partial index that is driving a query loop. Look
/// through all terms of the WHERE clause for the query to see if any can be proven true and
/// thus do not need to be evaluated.
fn where_apply_partial_index_constraints(p: &Parse, w: &mut WhereInfo, mut truth: ExprId, i_tab_cur: i32, wc: WcId) {
    while p.e(truth).op == TK_AND {
        where_apply_partial_index_constraints(p, w, p.e(truth).p_left, i_tab_cur, wc);
        truth = p.e(truth).p_right;
    }
    for term in w.a_wc[wc].a.iter_mut() {
        if term.wt_flags & TERM_CODED != 0 {
            continue;
        }
        if sqlite3_expr_compare(p, false, term.p_expr, truth, i_tab_cur) == 0 {
            term.wt_flags |= TERM_CODED;
        }
    }
}

/// `filterPullDown(pParse, pWInfo, iLevel, addrNxt, notReady)`: this routine is called right
/// after An OP_Filter has been generated and before the corresponding index search has been
/// performed. This routine checks to see if there are additional Bloom filters in inner loops
/// that can be checked prior to doing the index lookup. If there are available inner-loop
/// Bloom filters, then evaluate those filters now, before the index lookup. The idea is that
/// a Bloom filter check is way faster than an index lookup, and the Bloom filter might return
/// false, meaning that the index lookup can be skipped.
///
/// We know that an inner loop uses a Bloom filter because it has the WhereLevel.regFilter
/// set. If an inner-loop Bloom filter is checked, then clear WhereLevel.regFilter to 0 to
/// prevent it from being checked a second time when the inner loop is evaluated.
fn filter_pull_down(p: &mut Parse, w: &mut WhereInfo, mut i_level: usize, addr_nxt: i32, not_ready: Bitmask) {
    loop {
        i_level += 1;
        if i_level >= w.n_level as usize {
            break;
        }
        let i_loop = w.a[i_level].p_w_loop;
        if w.a[i_level].reg_filter == 0 {
            continue;
        }
        if w.loops[i_loop].n_skip != 0 {
            continue;
        }
        //         ,--- Because sqlite3ConstructBloomFilter() has will not have set
        //  vvvvv--'    pLevel->regFilter if this were true.
        if w.loops[i_loop].prereq & not_ready != 0 {
            continue;
        }
        let saved_addr_brk = w.a[i_level].addr_brk;
        w.a[i_level].addr_brk = addr_nxt;
        if w.loops[i_loop].ws_flags & WHERE_IPK as u32 != 0 {
            let t = w.loops[i_loop].a_l_term[0].expect("term");
            let mut reg_rowid = sqlite3_get_temp_reg(p);
            reg_rowid = code_equality_term(p, w, t, i_level, 0, false, reg_rowid);
            p.v.add_op2(OP_MustBeInt, reg_rowid, addr_nxt);
            p.v.add_op4_int(OP_Filter, w.a[i_level].reg_filter, addr_nxt, reg_rowid, 1);
        } else {
            let n_eq = w.loops[i_loop].n_eq as i32;
            let (r1, z_start_aff) = code_all_equality_terms(p, w, i_level, false, 0);
            code_apply_affinity(p, r1, n_eq, &z_start_aff);
            p.v.add_op4_int(OP_Filter, w.a[i_level].reg_filter, addr_nxt, r1, n_eq);
        }
        w.a[i_level].reg_filter = 0;
        w.a[i_level].addr_brk = saved_addr_brk;
    }
}

/// `whereLoopIsOneRow(pLoop)`: loop pLoop is a WHERE_INDEXED level that uses at least one IN
/// operator. Return true if the terms in pLoop->aLTerm[] are guaranteed to restrict the
/// search space to at most one row for each combination of values: a unique index with all
/// key columns constrained by something other than IS / IS NULL.
fn where_loop_is_one_row(p: &Parse, w: &WhereInfo, l: &WhereLoop) -> bool {
    let ix = p.idx(l.p_index);
    if ix.on_error != 0 && l.n_skip == 0 && l.n_eq == ix.n_key_col {
        for ii in 0..l.n_eq as usize {
            let t = l.a_l_term[ii].expect("term");
            if w.term(t).e_operator & (WO_IS | WO_ISNULL) != 0 {
                return false;
            }
        }
        return true;
    }
    false
}

/// `whereExprIsReady(pWInfo, pLevel, pTabItem, pE)`: return true if the WHERE-clause
/// expression pE can be evaluated at the current loop level: constraints of an outer join
/// must come from the right ON clause and that clause must be "ripe".
fn where_expr_is_ready(p: &Parse, w: &WhereInfo, i_level: usize, jtype: i32, e: ExprId) -> bool {
    if jtype & (JT_LEFT | JT_LTORJ | JT_RIGHT) != 0 {
        if !p.e(e).has(EP_OuterON | EP_InnerON) {
            // Defer processing WHERE clause constraints until after outer join processing.
            // tag-20220513a
            return false;
        } else if (jtype & JT_LEFT) != 0 && !p.e(e).has(EP_OuterON) {
            return false;
        } else {
            let m = sqlite3_where_get_mask(&w.s_mask_set, p.e(e).w);
            if m & w.a[i_level].not_ready != 0 {
                // An ON clause that is not ripe
                return false;
            }
        }
    }
    true
}

/// `SQLITE_STMTSTATUS_FULLSCAN_STEP`
const SQLITE_STMTSTATUS_FULLSCAN_STEP: u8 = 1;

/// `sqlite3WhereCodeOneLoopStart(pParse, v, pWInfo, iLevel, pLevel, notReady)`: generate code
/// for the start of the iLevel-th loop in the WHERE clause implementation described by
/// pWInfo.
pub fn sqlite3_where_code_one_loop_start(p: &mut Parse, w: &mut WhereInfo, i_level: usize, not_ready: Bitmask) -> Bitmask {
    let mut i_rowid_reg = 0; // Rowid is stored in this register, if not zero
    let mut i_release_reg = 0; // Temp register to free before returning
    let mut idx = IndexId::NULL; // Index used by loop (if any)
    let wc: WcId = 0;
    let i_loop = w.a[i_level].p_w_loop;
    let i_from = w.a[i_level].i_from as usize;
    let tab_list = w.p_tab_list;
    let (i_cur, jointype, via_coroutine, is_recursive) = {
        let it = &p.src(tab_list).a[i_from];
        (it.i_cursor, it.jointype as i32, it.via_coroutine, it.is_recursive)
    };
    w.a[i_level].not_ready = not_ready & !sqlite3_where_get_mask(&w.s_mask_set, i_cur);
    let b_rev = ((w.rev_mask >> i_level) & 1) != 0;

    // Create labels for the "break" and "continue" instructions for the current loop. Jump
    // to addrBrk to break out of a loop. Jump to cont to go immediately to the next iteration
    // of the loop.
    //
    // When there is an IN operator, we also have a "addrNxt" label that means to continue
    // with the next IN value combination. When there are no IN operators in the constraints,
    // the "addrNxt" label is the same as "addrBrk".
    let addr_brk = w.a[i_level].addr_brk;
    w.a[i_level].addr_nxt = addr_brk;
    let addr_cont = p.v.make_label();
    w.a[i_level].addr_cont = addr_cont;

    // If this is the right table of a LEFT OUTER JOIN, allocate and initialize a memory cell
    // that records if this table matches any row of the left table of the join.
    if i_from > 0 && (jointype & JT_LEFT) != 0 {
        p.n_mem += 1;
        w.a[i_level].i_left_join = p.n_mem;
        p.v.add_op2(OP_Integer, 0, w.a[i_level].i_left_join);
        if via_coroutine {
            p.v.add_op2(OP_NullRow, i_cur, 1);
        }
    }

    let l_ws_flags = w.loops[i_loop].ws_flags;
    // Special case of a FROM clause subquery implemented as a co-routine
    if via_coroutine {
        let (reg_yield, addr_fill_sub) = {
            let q = p.src(tab_list).a[i_from].p_subq.as_ref().expect("subquery");
            (q.reg_return, q.addr_fill_sub)
        };
        p.v.add_op3(OP_InitCoroutine, reg_yield, 0, addr_fill_sub);
        w.a[i_level].p2 = p.v.add_op2(OP_Yield, reg_yield, addr_brk);
        w.a[i_level].op = OP_Goto;
    } else if (l_ws_flags & WHERE_IPK as u32) != 0 && (l_ws_flags & (WHERE_COLUMN_IN | WHERE_COLUMN_EQ) as u32) != 0 {
        // Case 2: We can directly reference a single row using an equality comparison
        // against the ROWID field. Or we reference multiple rows using a "rowid IN (...)"
        // construct.
        let t = w.loops[i_loop].a_l_term[0].expect("term");
        p.n_mem += 1;
        i_release_reg = p.n_mem;
        i_rowid_reg = code_equality_term(p, w, t, i_level, 0, b_rev, i_release_reg);
        if i_rowid_reg != i_release_reg {
            sqlite3_release_temp_reg(p, i_release_reg);
        }
        let addr_nxt = w.a[i_level].addr_nxt;
        if w.a[i_level].reg_filter != 0 {
            p.v.add_op2(OP_MustBeInt, i_rowid_reg, addr_nxt);
            p.v.add_op4_int(OP_Filter, w.a[i_level].reg_filter, addr_nxt, i_rowid_reg, 1);
            filter_pull_down(p, w, i_level, addr_nxt, not_ready);
        }
        p.v.add_op3(OP_SeekRowid, i_cur, addr_nxt, i_rowid_reg);
        w.a[i_level].op = OP_Noop;
    } else if (l_ws_flags & WHERE_IPK as u32) != 0 && (l_ws_flags & WHERE_COLUMN_RANGE as u32) != 0 {
        // Case 3: We have an inequality comparison against the ROWID field.
        let mut test_op = OP_Noop;
        let mut mem_end_value = 0;
        let mut j = 0;
        let mut p_start: Option<TermRef> = None;
        let mut p_end: Option<TermRef> = None;
        if l_ws_flags & WHERE_BTM_LIMIT as u32 != 0 {
            p_start = w.loops[i_loop].a_l_term[j];
            j += 1;
        }
        if l_ws_flags & WHERE_TOP_LIMIT as u32 != 0 {
            p_end = w.loops[i_loop].a_l_term[j];
        }
        if b_rev {
            std::mem::swap(&mut p_start, &mut p_end);
        }
        if let Some(ts) = p_start {
            // The following constant maps TK_xx codes into corresponding seek opcodes. It
            // depends on a particular ordering of TK_xx
            const A_MOVE_OP: [u8; 4] = [
                /* TK_GT */ OP_SeekGT,
                /* TK_LE */ OP_SeekLE,
                /* TK_LT */ OP_SeekLT,
                /* TK_GE */ OP_SeekGE,
            ];
            let x = w.term(ts).p_expr; // The expression that defines the start bound
            let right = p.e(x).p_right;
            let (r1, mut r_temp, op);
            if sqlite3_expr_is_vector(p, right) {
                r_temp = sqlite3_get_temp_reg(p);
                r1 = r_temp;
                code_expr_or_vector(p, right, r1, 1);
                op = A_MOVE_OP[(((p.e(x).op as i32 - TK_GT as i32 - 1) & 0x3) | 0x1) as usize];
            } else {
                r_temp = 0;
                r1 = sqlite3_expr_code_temp(p, right, &mut r_temp);
                disable_term(p, w, i_level, ts);
                op = A_MOVE_OP[(p.e(x).op - TK_GT) as usize];
            }
            p.v.add_op3(op, i_cur, addr_brk, r1);
            sqlite3_release_temp_reg(p, r_temp);
        } else {
            p.v.add_op2(if b_rev { OP_Last } else { OP_Rewind }, i_cur, w.a[i_level].addr_halt);
        }
        if let Some(te) = p_end {
            let x = w.term(te).p_expr;
            let right = p.e(x).p_right;
            p.n_mem += 1;
            mem_end_value = p.n_mem;
            code_expr_or_vector(p, right, mem_end_value, 1);
            if !sqlite3_expr_is_vector(p, right) && (p.e(x).op == TK_LT || p.e(x).op == TK_GT) {
                test_op = if b_rev { OP_Le } else { OP_Ge };
            } else {
                test_op = if b_rev { OP_Lt } else { OP_Gt };
            }
            if !sqlite3_expr_is_vector(p, right) {
                disable_term(p, w, i_level, te);
            }
        }
        let start = p.v.current_addr();
        w.a[i_level].op = if b_rev { OP_Prev } else { OP_Next };
        w.a[i_level].p1 = i_cur;
        w.a[i_level].p2 = start;
        if test_op != OP_Noop {
            p.n_mem += 1;
            i_rowid_reg = p.n_mem;
            p.v.add_op2(OP_Rowid, i_cur, i_rowid_reg);
            p.v.add_op3(test_op, mem_end_value, addr_brk, i_rowid_reg);
            p.v.change_p5((SQLITE_AFF_NUMERIC as i32 | SQLITE_JUMPIFNULL) as u16);
        }
    } else if l_ws_flags & WHERE_INDEXED as u32 != 0 {
        // Case 4: Search using an index.
        //
        // The WHERE clause may contain zero or more equality terms ("==" or "IN" or "IS"
        // operators) that refer to the N left-most columns of the index. It may also contain
        // inequality constraints (>, <, >= or <=) on the indexed column that immediately
        // follows the N equalities. Only the right-most column can be an inequality - the
        // rest must use the "==", "IN", or "IS" operators.
        const A_START_OP: [u8; 8] = [
            0,
            0,
            OP_Rewind, // 2: (!start_constraints && startEq &&  !bRev)
            OP_Last,   // 3: (!start_constraints && startEq &&   bRev)
            OP_SeekGT, // 4: (start_constraints  && !startEq && !bRev)
            OP_SeekLT, // 5: (start_constraints  && !startEq &&  bRev)
            OP_SeekGE, // 6: (start_constraints  &&  startEq && !bRev)
            OP_SeekLE, // 7: (start_constraints  &&  startEq &&  bRev)
        ];
        const A_END_OP: [u8; 4] = [
            OP_IdxGE, // 0: (end_constraints && !bRev && !endEq)
            OP_IdxGT, // 1: (end_constraints && !bRev &&  endEq)
            OP_IdxLE, // 2: (end_constraints &&  bRev && !endEq)
            OP_IdxLT, // 3: (end_constraints &&  bRev &&  endEq)
        ];
        let n_eq = w.loops[i_loop].n_eq as i32; // Number of == or IN terms
        let mut n_btm = w.loops[i_loop].n_btm as i32; // Length of BTM vector
        let mut n_top = w.loops[i_loop].n_top as i32; // Length of TOP vector
        let mut p_range_start: Option<TermRef> = None; // Inequality constraint at range start
        let mut p_range_end: Option<TermRef> = None; // Inequality constraint at range end
        let mut n_extra_reg = 0; // Number of extra registers needed
        let mut b_seek_past_null = false; // True to seek past initial nulls
        let mut b_stop_at_null = false; // Add condition to terminate at NULLs
        let mut reg_bignull = 0; // big-null flag register
        let mut addr_seek_scan = 0; // Opcode of the OP_SeekScan, if any

        idx = w.loops[i_loop].p_index;
        let i_idx_cur = w.a[i_level].i_idx_cur;

        // Find any inequality constraint terms for the start and end of the range.
        let mut j = n_eq as usize;
        if l_ws_flags & WHERE_BTM_LIMIT as u32 != 0 {
            p_range_start = w.loops[i_loop].a_l_term[j];
            j += 1;
            n_extra_reg = n_extra_reg.max(w.loops[i_loop].n_btm as i32);
        }
        if l_ws_flags & WHERE_TOP_LIMIT as u32 != 0 {
            p_range_end = w.loops[i_loop].a_l_term[j];
            n_extra_reg = n_extra_reg.max(w.loops[i_loop].n_top as i32);
            if (w.term(p_range_end.expect("term")).wt_flags & TERM_LIKEOPT) != 0 {
                p.n_mem += 1;
                w.a[i_level].i_like_rep_cntr = p.n_mem as u32;
                p.v.add_op2(OP_Integer, 1, w.a[i_level].i_like_rep_cntr as i32);
                w.a[i_level].addr_like_rep = p.v.current_addr();
                // iLikeRepCntr actually stores 2x the counter register number. The bottom
                // bit is the DESC flag (1 if the scan is DESC).
                w.a[i_level].i_like_rep_cntr <<= 1;
                let desc = p.idx(idx).a_sort_order[n_eq as usize] as i32 == SQLITE_SO_DESC;
                w.a[i_level].i_like_rep_cntr |= (b_rev ^ desc) as u32;
            }
            if p_range_start.is_none() {
                let jc = p.idx(idx).ai_column[n_eq as usize] as i32;
                if (jc >= 0 && p.tab(p.idx(idx).p_table).a_col[jc as usize].not_null == 0) || jc == XN_EXPR {
                    b_seek_past_null = true;
                }
            }
        }

        // If the WHERE_BIGNULL_SORT flag is set, then index column nEq uses a non-default
        // "big-null" sort (either ASC NULLS LAST or DESC NULLS FIRST). For cases where the
        // cursor would iterate from the beginning of the index (no start constraints) the
        // range that includes the NULLs is scanned after the rest.
        if (l_ws_flags & (WHERE_TOP_LIMIT | WHERE_BTM_LIMIT) as u32) == 0 && (l_ws_flags & WHERE_BIGNULL_SORT as u32) != 0 {
            n_extra_reg = 1;
            b_seek_past_null = true;
            p.n_mem += 1;
            reg_bignull = p.n_mem;
            w.a[i_level].reg_bignull = reg_bignull;
            if w.a[i_level].i_left_join != 0 {
                p.v.add_op2(OP_Integer, 0, reg_bignull);
            }
            w.a[i_level].addr_bignull = p.v.make_label();
        }

        // If we are doing a reverse order scan on an ascending index, or a forward order scan
        // on a descending index, interchange the start and end terms (pRangeStart and
        // pRangeEnd).
        if n_eq < p.idx(idx).n_column as i32 && b_rev == (p.idx(idx).a_sort_order[n_eq as usize] as i32 == SQLITE_SO_ASC) {
            std::mem::swap(&mut p_range_end, &mut p_range_start);
            std::mem::swap(&mut b_seek_past_null, &mut b_stop_at_null);
            std::mem::swap(&mut n_btm, &mut n_top);
        }

        if i_level > 0 && (l_ws_flags & WHERE_IN_SEEKSCAN as u32) != 0 {
            // In case OP_SeekScan is used, ensure that the index cursor does not point to a
            // valid row for the first iteration of this loop.
            p.v.add_op1(OP_NullRow, i_idx_cur);
        }

        // Generate code to evaluate all constraint terms using == or IN and store the values
        // of those terms in an array of registers starting at regBase.
        let (reg_base, mut z_start_aff) = code_all_equality_terms(p, w, i_level, b_rev, n_extra_reg);
        let mut z_end_aff: Option<Vec<u8>> = None;
        if n_top != 0 {
            z_end_aff = Some(z_start_aff[n_eq as usize..].to_vec());
        }
        let addr_nxt = if reg_bignull != 0 { w.a[i_level].addr_bignull } else { w.a[i_level].addr_nxt };

        let mut start_eq = match p_range_start {
            None => true,
            Some(t) => w.term(t).e_operator & (WO_LE | WO_GE) != 0,
        };
        let mut end_eq = match p_range_end {
            None => true,
            Some(t) => w.term(t).e_operator & (WO_LE | WO_GE) != 0,
        };
        let mut start_constraints = p_range_start.is_some() || n_eq > 0;

        // Seek the index cursor to the start of the range.
        let mut n_constraint = n_eq;
        if let Some(ts) = p_range_start {
            let right = p.e(w.term(ts).p_expr).p_right;
            code_expr_or_vector(p, right, reg_base + n_eq, n_btm);
            where_like_optimization_string_fixup(p, w, i_level, ts);
            if (w.term(ts).wt_flags & TERM_VNULL) == 0 && sqlite3_expr_can_be_null(p, right) {
                p.v.add_op2(OP_IsNull, reg_base + n_eq, addr_nxt);
            }
            update_range_affinity_str(p, right, n_btm, &mut z_start_aff[n_eq as usize..]);
            n_constraint += n_btm;
            if !sqlite3_expr_is_vector(p, right) {
                disable_term(p, w, i_level, ts);
            } else {
                start_eq = true;
            }
            b_seek_past_null = false;
        } else if b_seek_past_null {
            start_eq = false;
            p.v.add_op2(OP_Null, 0, reg_base + n_eq);
            start_constraints = true;
            n_constraint += 1;
        } else if reg_bignull != 0 {
            p.v.add_op2(OP_Null, 0, reg_base + n_eq);
            start_constraints = true;
            n_constraint += 1;
        }
        code_apply_affinity(p, reg_base, n_constraint - b_seek_past_null as i32, &z_start_aff);
        let n_skip = w.loops[i_loop].n_skip as i32;
        if n_skip > 0 && n_constraint == n_skip {
            // The skip-scan logic inside the call to codeAllEqualityConstraints() above has
            // already left the cursor sitting on the correct row, so no further seeking is
            // needed
        } else {
            if reg_bignull != 0 {
                p.v.add_op2(OP_Integer, 1, reg_bignull);
            }
            if w.a[i_level].reg_filter != 0 {
                p.v.add_op4_int(OP_Filter, w.a[i_level].reg_filter, addr_nxt, reg_base, n_eq);
                filter_pull_down(p, w, i_level, addr_nxt, not_ready);
            }

            let mut op = A_START_OP[((start_constraints as usize) << 2) + ((start_eq as usize) << 1) + b_rev as usize];
            if (w.loops[i_loop].ws_flags & WHERE_IN_SEEKSCAN as u32) != 0 && op == OP_SeekGE {
                // TUNING: The OP_SeekScan opcode seeks to reduce the number of expensive seek
                // operations by replacing a single seek with 1 or more step operations. The
                // question is, how many steps should we try before giving up and going with
                // a seek. The cost of a seek is proportional to the logarithm of the of the
                // number of entries in the tree, so set the number of steps to try to the
                // estimated log of the size of the tree
                addr_seek_scan = p.v.add_op1(OP_SeekScan, (p.idx(idx).ai_row_log_est[0] as i32 + 9) / 10);
                if p_range_start.is_some() || p_range_end.is_some() {
                    p.v.change_p5(1);
                    let a = p.v.current_addr() + 1;
                    p.v.change_p2(addr_seek_scan, a);
                    addr_seek_scan = 0;
                }
            }
            p.v.add_op4_int(op, i_idx_cur, addr_nxt, reg_base, n_constraint);
            if reg_bignull != 0 {
                let a = p.v.current_addr() + 2;
                p.v.add_op2(OP_Goto, 0, a);
                op = A_START_OP[((n_constraint > 1) as usize) * 4 + 2 + b_rev as usize];
                p.v.add_op4_int(op, i_idx_cur, addr_nxt, reg_base, n_constraint - start_eq as i32);
            }
        }

        // Load the value for the inequality constraint at the end of the range (if any).
        n_constraint = n_eq;
        if let Some(te) = p_range_end {
            let right = p.e(w.term(te).p_expr).p_right;
            code_expr_or_vector(p, right, reg_base + n_eq, n_top);
            where_like_optimization_string_fixup(p, w, i_level, te);
            if (w.term(te).wt_flags & TERM_VNULL) == 0 && sqlite3_expr_can_be_null(p, right) {
                p.v.add_op2(OP_IsNull, reg_base + n_eq, addr_nxt);
            }
            if let Some(ze) = z_end_aff.as_mut() {
                update_range_affinity_str(p, right, n_top, ze);
                let z = ze.clone();
                code_apply_affinity(p, reg_base + n_eq, n_top, &z);
            }
            n_constraint += n_top;
            if !sqlite3_expr_is_vector(p, right) {
                disable_term(p, w, i_level, te);
            } else {
                end_eq = true;
            }
        } else if b_stop_at_null {
            if reg_bignull == 0 {
                p.v.add_op2(OP_Null, 0, reg_base + n_eq);
                end_eq = false;
            }
            n_constraint += 1;
        }

        // Top of the loop body
        w.a[i_level].p2 = p.v.current_addr();

        // Check if the index cursor is past the end of the range.
        if n_constraint != 0 {
            if reg_bignull != 0 {
                // Except, skip the end-of-range check while doing the NULL-scan
                let a = p.v.current_addr() + 3;
                p.v.add_op2(OP_IfNot, reg_bignull, a);
            }
            let op = A_END_OP[b_rev as usize * 2 + end_eq as usize];
            p.v.add_op4_int(op, i_idx_cur, addr_nxt, reg_base, n_constraint);
            if addr_seek_scan != 0 {
                p.v.jump_here(addr_seek_scan);
            }
        }
        if reg_bignull != 0 {
            // During a NULL-scan, check to see if we have reached the end of the NULLs
            let a = p.v.current_addr() + 2;
            p.v.add_op2(OP_If, reg_bignull, a);
            let op = A_END_OP[b_rev as usize * 2 + b_seek_past_null as usize];
            p.v.add_op4_int(op, i_idx_cur, addr_nxt, reg_base, n_constraint + b_seek_past_null as i32);
        }

        if (w.loops[i_loop].ws_flags & WHERE_IN_EARLYOUT as u32) != 0 {
            p.v.add_op3(OP_SeekHit, i_idx_cur, n_eq, n_eq);
        }

        // Seek the table cursor, if required
        let omit_table = (w.loops[i_loop].ws_flags & WHERE_IDX_ONLY as u32) != 0
            && (w.wctrl_flags as i32 & (WHERE_OR_SUBCLAUSE | WHERE_RIGHT_JOIN)) == 0;
        let idx_tab = p.idx(idx).p_table;
        if omit_table {
            // pIdx is a covering index. No need to access the main table.
        } else if p.tab(idx_tab).has_rowid() {
            code_deferred_seek(p, w, idx, i_cur, i_idx_cur);
        } else if i_cur != i_idx_cur {
            let pk = sqlite3_primary_key_index(p, idx_tab);
            let n_pk = p.idx(pk).n_key_col as i32;
            i_rowid_reg = sqlite3_get_temp_range(p, n_pk);
            for j in 0..n_pk {
                let k = sqlite3_table_column_to_index(p.idx(idx), p.idx(pk).ai_column[j as usize] as i32);
                p.v.add_op3(OP_Column, i_idx_cur, k, i_rowid_reg + j);
            }
            p.v.add_op4_int(OP_NotFound, i_cur, addr_cont, i_rowid_reg, n_pk);
        }

        if w.a[i_level].i_left_join == 0 {
            // If a partial index is driving the loop, try to eliminate WHERE clause terms
            // from the query that must be true due to the WHERE clause of the partial index.
            // This optimization does not work on an outer join, as shown by:
            //
            // 2019-11-02 ticket 623eff57e76d45f6      (LEFT JOIN)
            // 2025-05-29 forum post 7dee41d32506c4ae  (RIGHT JOIN)
            let part = p.idx(idx).p_part_idx_where;
            if part.nn() && w.a[i_level].p_rj.is_none() {
                where_apply_partial_index_constraints(p, w, part, i_cur, wc);
            }
        }

        // Record the instruction used to terminate the loop.
        let l = w.loops[i_loop].clone();
        if (l.ws_flags & WHERE_ONEROW as u32) != 0
            || (!w.a[i_level].a_in_loop.is_empty() && reg_bignull == 0 && where_loop_is_one_row(p, w, &l))
        {
            w.a[i_level].op = OP_Noop;
        } else if b_rev {
            w.a[i_level].op = OP_Prev;
        } else {
            w.a[i_level].op = OP_Next;
        }
        w.a[i_level].p1 = i_idx_cur;
        w.a[i_level].p3 = ((l.ws_flags & WHERE_UNQ_WANTED as u32) != 0) as u8;
        if (l.ws_flags & WHERE_CONSTRAINT as u32) == 0 {
            w.a[i_level].p5 = SQLITE_STMTSTATUS_FULLSCAN_STEP;
        }
        if omit_table {
            idx = IndexId::NULL;
        }
    } else if l_ws_flags & WHERE_MULTI_OR as u32 != 0 {
        // Case 5: Two or more separately indexed terms connected by OR
        //
        // Example:
        //
        //   CREATE TABLE t1(a,b,c,d);
        //   CREATE INDEX i1 ON t1(a);
        //   CREATE INDEX i2 ON t1(b);
        //   CREATE INDEX i3 ON t1(c);
        //
        //   SELECT * FROM t1 WHERE a=5 OR b=7 OR (c=11 AND d=13)
        //
        // In the example, there are three indexed terms connected by OR. The top of the loop
        // looks like this:
        //
        //          Null       1                # Zero the rowset in reg 1
        //
        // Then, for each indexed term, the following. The arguments to RowSetTest are such
        // that the rowid of the current row is inserted into the RowSet. If it is already
        // present, control skips the Gosub opcode and jumps straight to the code generated
        // by WhereEnd().
        //
        //        sqlite3WhereBegin(<term>)
        //          RowSetTest                  # Insert rowid into rowset
        //          Gosub      2 A
        //        sqlite3WhereEnd()
        //
        // Following the above, code to terminate the loop. Label A, the target of the Gosub
        // above, jumps to the instruction right after the Goto.
        //
        //          Null       1                # Zero the rowset in reg 1
        //          Goto       B                # The loop is finished.
        //
        //       A: <loop body>                 # Return data, whatever.
        //
        //          Return     2                # Jump back to the Gosub
        //
        //       B: <after the loop>
        //
        // Added 2014-05-26: If the table is a WITHOUT ROWID table, then use a ephemeral
        // index instead of a RowSet to record the primary keys of the rows we have already
        // seen.
        let mut cov = IndexId::NULL; // Potential covering index (or NULL)
        let i_cov_cur = p.n_tab; // Cursor used for index scans (if any)
        p.n_tab += 1;
        p.n_mem += 1;
        let reg_return = p.n_mem; // Register used with OP_Gosub
        let mut reg_rowset = 0; // Register for RowSet object
        let mut reg_rowid = 0; // Register holding rowid
        let i_loop_body = p.v.make_label(); // Start of loop body
        let mut untested_terms = false; // Some terms not completely tested
        let mut and_expr = ExprId::NULL; // An ".. AND (...)" expression
        let tab = p.src(tab_list).a[i_from].p_s_tab;

        let t = w.loops[i_loop].a_l_term[0].expect("term");
        let or_wc = w.term(t).p_or_info.expect("pOrInfo").wc;
        w.a[i_level].op = OP_Return;
        w.a[i_level].p1 = reg_return;

        // Set up a new SrcList in pOrTab containing the table being scanned by this loop in
        // the a[0] slot and all notReady tables in a[1..] slots. This becomes the SrcList in
        // the recursive call to sqlite3WhereBegin().
        let or_tab: SrcListId;
        if w.n_level > 1 || p.src(tab_list).a[i_from].from_exists {
            let n_not_ready = w.n_level as usize - i_level - 1; // The number of notReady tables
            let mut items = vec![p.src(tab_list).a[i_from].clone()];
            for k in 1..=n_not_ready {
                let f = w.a[i_level + k].i_from as usize;
                items.push(p.src(tab_list).a[f].clone());
            }
            // Clear the fromExists flag on the OR-optimized table entry so that the calls to
            // sqlite3WhereEnd() do not code "Goto addrBrk" for it.
            items[0].from_exists = false;
            or_tab = p.new_src(SrcList { a: items });
        } else {
            or_tab = tab_list;
        }

        // Initialize the rowset register to contain NULL. An SQL NULL is equivalent to an
        // empty rowset. Or, create an ephemeral index capable of holding primary keys in the
        // case of a WITHOUT ROWID.
        //
        // Also initialize regReturn to contain the address of the instruction immediately
        // following the OP_Return at the bottom of the loop. This is required in a few
        // obscure LEFT JOIN cases where control jumps over the top of the loop into the body
        // of it. In this case the correct response for the end-of-loop code (the OP_Return)
        // is to fall through to the next instruction, just as an OP_Next does if called on
        // an uninitialized cursor.
        if (w.wctrl_flags as i32 & WHERE_DUPLICATES_OK) == 0 {
            if p.tab(tab).has_rowid() {
                p.n_mem += 1;
                reg_rowset = p.n_mem;
                p.v.add_op2(OP_Null, 0, reg_rowset);
            } else {
                let pk = sqlite3_primary_key_index(p, tab);
                reg_rowset = p.n_tab;
                p.n_tab += 1;
                let n_key = p.idx(pk).n_key_col as i32;
                p.v.add_op2(OP_OpenEphemeral, reg_rowset, n_key);
                sqlite3_vdbe_set_p4_key_info(p, pk);
            }
            p.n_mem += 1;
            reg_rowid = p.n_mem;
        }
        let i_ret_init = p.v.add_op2(OP_Integer, 0, reg_return);

        // If the original WHERE clause is z of the form:  (x1 OR x2 OR ...) AND y Then for
        // every term xN, evaluate as the subexpression: xN AND y That way, terms in y that
        // are factored into the disjunction will be picked up by the recursive calls to
        // sqlite3WhereBegin() below.
        //
        // Actually, each subexpression is converted to "xN AND w" where w is the "interesting"
        // terms of z - terms that did not originate in the ON/USING clause of a LEFT JOIN,
        // and terms that are usable as indices.
        //
        // This optimization also only applies if the (x1 OR x2 OR ...) term is not contained
        // in the ON clause of a LEFT JOIN. See ticket http://sqlite.org/src/info/f2369304e4
        //
        // 2022-02-04: Do not push down slices of a row-value comparison. In other words,
        // "w" or "y" may not be a slice of a vector. Otherwise the initialization of the
        // right-hand operand of the vector comparison might not occur, or might occur only
        // in an OR branch that is not taken. dbsqlfuzz 80a9fade844b4fb43564efc972bcb2c68270f5d1.
        //
        // 2022-03-03: Do not push down expressions that involve subqueries. The subquery
        // might get coded as a subroutine. Any table-references in the subquery might be
        // resolved to index-references for the index on the OR branch in which the
        // subroutine is coded. But if the subroutine is invoked from a different OR branch
        // that uses a different index, such index-references will not work.
        // tag-20220303a https://sqlite.org/forum/forumpost/36937b197273d403
        if w.a_wc[wc].a.len() > 1 {
            for i_term in 0..w.a_wc[wc].a.len() {
                let (e, wt_flags, e_operator) = {
                    let term = &w.a_wc[wc].a[i_term];
                    (term.p_expr, term.wt_flags, term.e_operator)
                };
                if TermRef::new(wc, i_term) == t {
                    continue;
                }
                if (wt_flags & (TERM_VIRTUAL | TERM_CODED | TERM_SLICE)) != 0 {
                    continue;
                }
                if (e_operator & WO_ALL) == 0 {
                    continue;
                }
                if p.e(e).has(EP_Subquery) {
                    continue; // tag-20220303a
                }
                if !where_expr_is_ready(p, w, i_level, jointype, e) {
                    continue;
                }
                let dup = sqlite3_expr_dup(p, e, 0);
                and_expr = sqlite3_expr_and(p, and_expr, dup);
            }
            if and_expr.nn() {
                // The extra 0x10000 bit on the opcode is masked off and does not become part
                // of the new Expr.op. However, it does make the op value unique from TK_AND
                // so that sqlite3PExpr() does not reduce the AND.
                and_expr = sqlite3_p_expr(p, TK_AND, ExprId::NULL, and_expr);
            }
        }

        // Run a separate WHERE clause for each term of the OR clause. After eliminating
        // duplicates from other WHERE clauses, the action for each sub-WHERE clause is to
        // invoke the main loop body as a subroutine.
        sqlite3_vdbe_explain(p, true, "MULTI-INDEX OR".to_owned());
        let n_or = w.a_wc[or_wc].a.len();
        for ii in 0..n_or {
            let (o_cur, o_op, o_expr) = {
                let term = &w.a_wc[or_wc].a[ii];
                (term.left_cursor, term.e_operator, term.p_expr)
            };
            if o_cur == i_cur || (o_op & WO_AND) != 0 {
                let mut jmp1 = 0; // Address of jump operation
                let mut or_expr = sqlite3_expr_dup(p, o_expr, 0); // Local copy of OR clause term
                if and_expr.nn() {
                    p.em(and_expr).p_left = or_expr;
                    or_expr = and_expr;
                }
                // Loop through table entries that match term pOrTerm.
                sqlite3_vdbe_explain(p, true, format!("INDEX {}", ii + 1));
                let sub = sqlite3_where_begin(
                    p,
                    or_tab,
                    or_expr,
                    ExprListId::NULL,
                    ExprListId::NULL,
                    SelectId::NULL,
                    WHERE_OR_SUBCLAUSE as u16,
                    i_cov_cur,
                );
                if let Some(sub_w) = sub {
                    let _addr_explain = sqlite3_where_explain_one_scan(p, &sub_w, or_tab, 0, 0);
                    // This is the sub-WHERE clause body. First skip over duplicate rows from
                    // prior sub-WHERE clauses, and record the row in the RowSet so that
                    // subsequent sub-WHERE clauses will skip it. Then invoke the main loop
                    // body as a subroutine.
                    if (w.wctrl_flags as i32 & WHERE_DUPLICATES_OK) == 0 {
                        let i_set: i32 = if ii == n_or - 1 { -1 } else { ii as i32 };
                        if p.tab(tab).has_rowid() {
                            sqlite3_expr_code_get_column_of_table(p, tab, i_cur, -1, reg_rowid);
                            jmp1 = p.v.add_op4_int(OP_RowSetTest, reg_rowset, 0, reg_rowid, i_set);
                        } else {
                            let pk = sqlite3_primary_key_index(p, tab);
                            let n_pk = p.idx(pk).n_key_col as i32;

                            // Read the PK into an array of temp registers.
                            let r = sqlite3_get_temp_range(p, n_pk);
                            for i_pk in 0..n_pk {
                                let i_col = p.idx(pk).ai_column[i_pk as usize] as i32;
                                sqlite3_expr_code_get_column_of_table(p, tab, i_cur, i_col, r + i_pk);
                            }

                            // Check if the temp table already contains this key. If so, the
                            // row has already been included in the result set and can be
                            // ignored (by jumping past the Gosub below). Otherwise, insert
                            // the key into the temp table and proceed with processing the
                            // row.
                            //
                            // Use some of the same optimizations as OP_RowSetTest: If iSet is
                            // zero, assume that the key cannot already be present in the
                            // temp table. And if iSet is -1, assume that there is no need to
                            // insert the key into the temp table, as it will never be
                            // tested for.
                            if i_set != 0 {
                                jmp1 = p.v.add_op4_int(OP_Found, reg_rowset, 0, r, n_pk);
                            }
                            if i_set >= 0 {
                                p.v.add_op3(OP_MakeRecord, r, n_pk, reg_rowid);
                                p.v.add_op4_int(OP_IdxInsert, reg_rowset, reg_rowid, r, n_pk);
                                if i_set != 0 {
                                    p.v.change_p5(OPFLAG_USESEEKRESULT as u16);
                                }
                            }

                            // Release the array of temp registers
                            sqlite3_release_temp_range(p, r, n_pk);
                        }
                    }

                    // Invoke the main loop body as a subroutine
                    p.v.add_op2(OP_Gosub, reg_return, i_loop_body);

                    // Jump here (skipping the main loop body subroutine) if the current
                    // sub-WHERE row is a duplicate from prior sub-WHEREs.
                    if jmp1 != 0 {
                        p.v.jump_here(jmp1);
                    }

                    // The pSubWInfo->untestedTerms flag means that this OR term contained
                    // one or more AND term from a notReady table. The terms from the
                    // notReady table could not be tested and will need to be tested later.
                    if sub_w.untested_terms {
                        untested_terms = true;
                    }

                    // If all of the OR-connected terms are optimized using the same index,
                    // and the index is opened using the same cursor number by each call to
                    // sqlite3WhereBegin() made by this loop, it may be possible to use that
                    // index as a covering index.
                    //
                    // If the call to sqlite3WhereBegin() above resulted in a scan that uses
                    // an index, and this is either the first OR-connected term processed or
                    // the index is the same as that used by all previous terms, set pCov to
                    // the candidate covering index. Otherwise, set it to NULL to indicate
                    // that no candidate covering index will be available.
                    let sub_loop = &sub_w.loops[sub_w.a[0].p_w_loop];
                    if (sub_loop.ws_flags & WHERE_INDEXED as u32) != 0
                        && (ii == 0 || sub_loop.p_index == cov)
                        && (p.tab(tab).has_rowid() || !p.idx(sub_loop.p_index).is_primary_key())
                    {
                        cov = sub_loop.p_index;
                    } else {
                        cov = IndexId::NULL;
                    }
                    if sub_w.uses_deferred_seek() {
                        w.b_deferred_seek = true;
                    }

                    // Finish the loop through table entries that match term pOrTerm.
                    sqlite3_where_end(p, sub_w);
                    sqlite3_vdbe_explain_pop(p);
                }
                if p.unsupported.is_some() {
                    return w.a[i_level].not_ready;
                }
            }
        }
        sqlite3_vdbe_explain_pop(p);
        w.a[i_level].p_covering_idx = cov;
        if cov.nn() {
            w.a[i_level].i_idx_cur = i_cov_cur;
        }
        if and_expr.nn() {
            p.em(and_expr).p_left = ExprId::NULL;
        }
        let a = p.v.current_addr();
        p.v.change_p1(i_ret_init, a);
        p.v.goto(w.a[i_level].addr_brk);
        p.v.resolve_label(i_loop_body);

        // Set the P2 operand of the OP_Return opcode that will end the current loop to point
        // to this spot, which is the top of the next containing loop. The byte-code
        // formatter will use that P2 value as a hint to indent the loop properly. This is
        // also used for verifying that sqlite3VdbeNoJumpsOutsideSubrtn().
        w.a[i_level].p2 = p.v.current_addr();

        if !untested_terms {
            disable_term(p, w, i_level, t);
        }
    } else {
        // Case 6: There is no usable index. We must do a complete scan of the entire table.
        const A_STEP: [u8; 2] = [OP_Next, OP_Prev];
        const A_START: [u8; 2] = [OP_Rewind, OP_Last];
        if is_recursive {
            // Tables marked isRecursive have only a single row that is stored in a
            // pseudo-cursor. No need to Rewind or Next such cursors.
            w.a[i_level].op = OP_Noop;
        } else {
            w.a[i_level].op = A_STEP[b_rev as usize];
            w.a[i_level].p1 = i_cur;
            w.a[i_level].p2 = 1 + p.v.add_op2(A_START[b_rev as usize], i_cur, w.a[i_level].addr_halt);
            w.a[i_level].p5 = SQLITE_STMTSTATUS_FULLSCAN_STEP;
        }
    }

    // Insert code to test every subexpression that can be completely computed using the
    // current set of tables.
    //
    // This loop may run between one and three times, depending on the constraints to be
    // generated. The value of stack variable iLoop determines the constraints coded by each
    // pass:
    //
    // iLoop==1: Code only expressions that are entirely covered by pIdx.
    // iLoop==2: Code remaining expressions that do not contain correlated sub-queries.
    // iLoop==3: Code all remaining expressions.
    //
    // An effort is made to skip unnecessary iterations of the loop.
    //
    // This optimization of causing simple query restrictions to occur before more complex one
    // is call the "push-down" optimization in MySQL. Here in SQLite, the name is "MySQL
    // push-down", to disambiguate it from the WHERE-clause push-down optimization.
    let mut i_loop_pass = if idx.nn() { 1 } else { 2 };
    loop {
        let mut i_next = 0; // Next value for iLoop
        let n_term = w.a_wc[wc].a.len();
        for k in 0..n_term {
            let (wt_flags, prereq_all, e) = {
                let term = &w.a_wc[wc].a[k];
                (term.wt_flags, term.prereq_all, term.p_expr)
            };
            let mut skip_like_addr = 0;
            if wt_flags & (TERM_VIRTUAL | TERM_CODED) != 0 {
                continue;
            }
            if (prereq_all & w.a[i_level].not_ready) != 0 {
                w.untested_terms = true;
                continue;
            }
            if !where_expr_is_ready(p, w, i_level, jointype, e) {
                continue;
            }
            if i_loop_pass == 1 && !sqlite3_expr_covered_by_index(p, e, w.a[i_level].i_tab_cur, idx) {
                i_next = 2;
                continue;
            }
            if i_loop_pass < 3 && (wt_flags & TERM_VARSELECT) != 0 {
                if i_next == 0 {
                    i_next = 3;
                }
                continue;
            }

            if (wt_flags & TERM_LIKECOND) != 0 {
                // If the TERM_LIKECOND flag is set, that means that the range search is
                // sufficient to guarantee that the LIKE operator is true, so we can skip the
                // LIKE operator test... unless the range search is over BLOBs (the second
                // pass of the LIKE loop).
                let x = w.a[i_level].i_like_rep_cntr;
                if x > 0 {
                    skip_like_addr = p.v.add_op1(if x & 1 != 0 { OP_IfNot } else { OP_If }, (x >> 1) as i32);
                }
            }
            sqlite3_expr_if_false(p, e, addr_cont, SQLITE_JUMPIFNULL);
            if skip_like_addr != 0 {
                p.v.jump_here(skip_like_addr);
            }
            w.a_wc[wc].a[k].wt_flags |= TERM_CODED;
        }
        i_loop_pass = i_next;
        if i_loop_pass <= 0 {
            break;
        }
    }

    // Insert code to test for implied constraints based on transitivity of the "==" operator.
    //
    // Example: If the WHERE clause contains "t1.a=t2.b" and "t2.b=123" and we are coding the
    // t1 loop and the t2 loop has not yet coded, then we cannot use the "t1.a=t2.b"
    // constraint, but we can code the implied "t1.a=123" constraint.
    let n_base = w.a_wc[wc].n_base as usize;
    for k in 0..n_base {
        let term = w.a_wc[wc].a[k].clone();
        if term.wt_flags & (TERM_VIRTUAL | TERM_CODED) != 0 {
            continue;
        }
        if (term.e_operator & (WO_EQ | WO_IS)) == 0 {
            continue;
        }
        if (term.e_operator & WO_EQUIV) == 0 {
            continue;
        }
        if term.left_cursor != i_cur {
            continue;
        }
        if jointype & (JT_LEFT | JT_LTORJ | JT_RIGHT) != 0 {
            continue;
        }
        let e = term.p_expr;
        let Some(alt) = sqlite3_where_find_term(p, w, wc, i_cur, term.left_column, not_ready, (WO_EQ | WO_IN | WO_IS) as u32, IndexId::NULL)
        else {
            continue;
        };
        let (a_flags, a_expr, a_op) = {
            let a = w.term(alt);
            (a.wt_flags, a.p_expr, a.e_operator)
        };
        if a_flags & TERM_CODED != 0 {
            continue;
        }
        if p.e(a_expr).has(EP_Collate) {
            continue;
        }
        if (a_op & WO_IN) != 0 && p.e(a_expr).use_x_select() && p.l(p.s(p.e(a_expr).x_select()).p_e_list).n_expr() > 1 {
            continue;
        }
        // sEAlt = *pAlt->pExpr; sEAlt.pLeft = pE->pLeft;
        let mut s_e_alt = p.e(a_expr).clone();
        s_e_alt.p_left = p.e(e).p_left;
        let alt_id = p.new_expr(s_e_alt);
        sqlite3_expr_if_false(p, alt_id, addr_cont, SQLITE_JUMPIFNULL);
        w.term_mut(alt).wt_flags |= TERM_CODED;
    }

    // For a RIGHT OUTER JOIN, record the fact that the current row has been matched at least
    // once.
    if let Some(rj) = w.a[i_level].p_rj {
        // pTab is the right-hand table of the RIGHT JOIN. Generate code that will record the
        // PK of the current row in the iMatch ephemeral table. Thus if a table row gets
        // matched, we know it does not need to be NULL-extended later.
        let tab = p.src(tab_list).a[i_from].p_s_tab;
        let (n_pk, r);
        if p.tab(tab).has_rowid() {
            r = sqlite3_get_temp_range(p, 2);
            sqlite3_expr_code_get_column_of_table(p, tab, w.a[i_level].i_tab_cur, -1, r + 1);
            n_pk = 1;
        } else {
            let pk = sqlite3_primary_key_index(p, tab);
            n_pk = p.idx(pk).n_key_col as i32;
            r = sqlite3_get_temp_range(p, n_pk + 1);
            for i_pk in 0..n_pk {
                let i_col = p.idx(pk).ai_column[i_pk as usize] as i32;
                sqlite3_expr_code_get_column_of_table(p, tab, i_cur, i_col, r + 1 + i_pk);
            }
        }
        let jmp1 = p.v.add_op4_int(OP_Found, rj.i_match, 0, r + 1, n_pk);
        p.v.add_op3(OP_MakeRecord, r + 1, n_pk, r);
        p.v.add_op4_int(OP_IdxInsert, rj.i_match, r, r + 1, n_pk);
        if rj.reg_bloom != 0 {
            p.v.add_op4_int(OP_FilterAdd, rj.reg_bloom, 0, r + 1, n_pk);
            p.v.change_p5(OPFLAG_USESEEKRESULT as u16);
        }
        p.v.jump_here(jmp1);
        sqlite3_release_temp_range(p, r, n_pk + 1);
    }

    // For a LEFT OUTER JOIN, generate code that will record the fact that at least one row
    // of the right table has matched the left table.
    let mut code_outer = false;
    if w.a[i_level].i_left_join != 0 {
        w.a[i_level].addr_first = p.v.current_addr();
        p.v.add_op2(OP_Integer, 1, w.a[i_level].i_left_join);
        if w.a[i_level].p_rj.is_none() {
            code_outer = true; // WHERE clause constraints
        }
    }

    if !code_outer && w.a[i_level].p_rj.is_some() {
        // Create a subroutine used to process all interior loops and code of the RIGHT JOIN.
        // During normal operation, the subroutine will be in-line with the rest of the code.
        // But at the end, a separate loop will run that invokes this subroutine for
        // unmatched rows of pTab, in order to generate the output rows that are NULL-padded
        // on the left.
        let mut rj = w.a[i_level].p_rj.expect("pRJ");
        p.v.add_op2(OP_BeginSubrtn, 0, rj.reg_return);
        rj.addr_subrtn = p.v.current_addr();
        w.a[i_level].p_rj = Some(rj);
        p.within_rj_subrtn = p.within_rj_subrtn.wrapping_add(1);
        // WHERE clause constraints must be deferred until after outer join row elimination
        // has completed, since WHERE clause constraints apply to the results of the OUTER
        // JOIN. The following loop generates the appropriate WHERE clause constraint checks.
        // tag-20220513a.
        code_outer = true;
    }
    if code_outer {
        // code_outer_join_constraints:
        let n_base = w.a_wc[wc].n_base as usize;
        for k in 0..n_base {
            let (wt_flags, prereq_all, e) = {
                let term = &w.a_wc[wc].a[k];
                (term.wt_flags, term.prereq_all, term.p_expr)
            };
            if wt_flags & (TERM_VIRTUAL | TERM_CODED) != 0 {
                continue;
            }
            if (prereq_all & w.a[i_level].not_ready) != 0 {
                continue;
            }
            if jointype & JT_LTORJ != 0 {
                continue;
            }
            sqlite3_expr_if_false(p, e, addr_cont, SQLITE_JUMPIFNULL);
            w.a_wc[wc].a[k].wt_flags |= TERM_CODED;
        }
    }
    let _ = i_release_reg;
    let _ = i_rowid_reg;
    w.a[i_level].not_ready
}

/// `sqlite3WhereRightJoinLoop(pWInfo, iLevel, pLevel)`: generate the code for the loop that
/// finds all non-matched terms for a RIGHT JOIN.
pub fn sqlite3_where_right_join_loop(p: &mut Parse, w: &mut WhereInfo, i_level: usize) {
    let rj = w.a[i_level].p_rj.expect("pRJ");
    let mut sub_where = ExprId::NULL;
    let wc: WcId = 0;
    let i_loop = w.a[i_level].p_w_loop;
    let i_from = w.a[i_level].i_from as usize;
    let tab_list = w.p_tab_list;
    let tab = p.src(tab_list).a[i_from].p_s_tab;
    let mut m_all: Bitmask = 0;

    let name = p.tab(tab).z_name.clone();
    sqlite3_vdbe_explain(p, true, format!("RIGHT-JOIN {name}"));
    // (sqlite3VdbeNoJumpsOutsideSubrtn(): a SQLITE_DEBUG check)
    for k in 0..i_level {
        let f = w.a[k].i_from as usize;
        m_all |= w.loops[w.a[k].p_w_loop].mask_self;
        let (via_coroutine, reg_result, n_expr) = {
            let it = &p.src(tab_list).a[f];
            match (&it.p_subq, it.via_coroutine) {
                (Some(q), true) => (true, q.reg_result, p.l(p.s(q.p_select).p_e_list).n_expr()),
                _ => (false, 0, 0),
            }
        };
        if via_coroutine {
            p.v.add_op3(OP_Null, 0, reg_result, reg_result + n_expr - 1);
        }
        p.v.add_op1(OP_NullRow, w.a[k].i_tab_cur);
        let i_idx_cur = w.a[k].i_idx_cur;
        if i_idx_cur != 0 {
            p.v.add_op1(OP_NullRow, i_idx_cur);
        }
    }
    if (p.src(tab_list).a[i_from].jointype as i32 & JT_LTORJ) == 0 {
        m_all |= w.loops[i_loop].mask_self;
        for k in 0..w.a_wc[wc].a.len() {
            let (wt_flags, e_operator, prereq_all, e) = {
                let term = &w.a_wc[wc].a[k];
                (term.wt_flags, term.e_operator, term.prereq_all, term.p_expr)
            };
            if (wt_flags & (TERM_VIRTUAL | TERM_SLICE)) != 0 && e_operator != WO_ROWVAL {
                break;
            }
            if prereq_all & !m_all != 0 {
                continue;
            }
            if p.e(e).has(EP_OuterON | EP_InnerON) {
                continue;
            }
            let dup = sqlite3_expr_dup(p, e, 0);
            sub_where = sqlite3_expr_and(p, sub_where, dup);
        }
    }
    if w.a[i_level].i_idx_cur != 0 {
        // pSubWhere may contain expressions that read from an index on the table on the RHS
        // of the right join. All such expressions first test if the index is pointing at a
        // NULL row, and if so, read from the table cursor instead. So ensure that the index
        // cursor really is pointing at a NULL row here, so that no values are read from it
        // during the scan of the RHS of the RIGHT join below.
        p.v.add_op1(OP_NullRow, w.a[i_level].i_idx_cur);
    }
    let mut item = p.src(tab_list).a[i_from].clone();
    item.jointype = 0;
    let from = p.new_src(SrcList { a: vec![item] });
    if p.within_rj_subrtn >= 100 {
        // This limit --------------^^^ is somewhat arbitrary. Its purpose is to prevent
        // runaway code generation.
        p.error_msg("too many RIGHT JOINs");
        return;
    }
    p.within_rj_subrtn += 1;
    let sub = sqlite3_where_begin(p, from, sub_where, ExprListId::NULL, ExprListId::NULL, SelectId::NULL, WHERE_RIGHT_JOIN as u16, 0);
    if let Some(sub_w) = sub {
        let i_cur = w.a[i_level].i_tab_cur; // Table on RHS of RIGHT JOIN
        p.n_mem += 1;
        let r = p.n_mem; // Register range to hold primary key
        let n_pk; // Number of values in the primary key
        let addr_cont = sub_w.continue_label();
        if p.tab(tab).has_rowid() {
            sqlite3_expr_code_get_column_of_table(p, tab, i_cur, -1, r);
            n_pk = 1;
        } else {
            let pk = sqlite3_primary_key_index(p, tab);
            n_pk = p.idx(pk).n_key_col as i32;
            p.n_mem += n_pk - 1;
            for i_pk in 0..n_pk {
                let i_col = p.idx(pk).ai_column[i_pk as usize] as i32;
                sqlite3_expr_code_get_column_of_table(p, tab, i_cur, i_col, r + i_pk);
            }
        }
        // Generate code that checks to see if the current row of the RHS table has already
        // been matched (it is in iMatch). If so, skip it.
        if rj.reg_bloom != 0 {
            let a = p.v.current_addr() + 2;
            p.v.add_op4_int(OP_Filter, rj.reg_bloom, a, r, n_pk);
        }
        p.v.add_op4_int(OP_Found, rj.i_match, addr_cont, r, n_pk);
        let r2 = sqlite3_get_temp_reg(p);
        // Generate code that inserts the PK of the RHS table into the match table
        p.v.add_op3(OP_MakeRecord, r, n_pk, r2);
        p.v.add_op4_int(OP_IdxInsert, rj.i_match, r2, r, n_pk);
        sqlite3_release_temp_reg(p, r2);
        if rj.reg_bloom != 0 {
            p.v.add_op4_int(OP_FilterAdd, rj.reg_bloom, 0, r, n_pk);
            p.v.change_p5(OPFLAG_USESEEKRESULT as u16);
        }
        // Invoke the subroutine that actually puts the current RHS table row into the output
        p.v.add_op2(OP_Gosub, rj.reg_return, rj.addr_subrtn);
        sqlite3_where_end(p, sub_w);
    }
    sqlite3_vdbe_explain_pop(p);
    p.within_rj_subrtn = p.within_rj_subrtn.wrapping_sub(1);
}
