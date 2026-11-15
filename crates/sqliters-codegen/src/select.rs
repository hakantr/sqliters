//! `select.c`: the code generator for SELECT statements, as far as step V1 goes — SELECT
//! statements without a FROM clause (result row, DISTINCT, LIMIT / OFFSET, WHERE, aggregate
//! functions without GROUP BY), `VALUES`, compound selects whose arms are of that class, and
//! the same forms as scalar / EXISTS / IN sub-selects.
//!
//! Functions keep C's names and order of operations. Everything a statement needs beyond this
//! (FROM clause terms other than the co-routine of a multi-row VALUES, GROUP BY, sorters, the
//! query planner of `where.c`, window functions, recursive CTEs, query flattening) calls
//! [`Parse::not_supported`].

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{KeyInfo, P4, Vdbe};

use crate::build::sqlite3_may_abort;
use crate::callback::*;
use crate::consts::*;
use crate::expr::*;
use crate::exprcode::*;
use crate::resolve::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::walker::*;
use crate::where_::*;

/// `struct DistinctCtx`: context for DISTINCT processing.
#[derive(Debug, Clone, Default)]
pub struct DistinctCtx {
    /// 0: Not distinct. 1: DISTINCT  2: DISTINCT and ORDER BY
    pub is_tnct: u8,
    /// One of the WHERE_DISTINCT_* operators
    pub e_tnct_type: u8,
    /// Ephemeral table used for DISTINCT processing
    pub tab_tnct: i32,
    /// Address of OP_OpenEphemeral opcode for tabTnct
    pub addr_tnct: i32,
}

/// `struct SortCtx`: context for ORDER BY processing.
#[derive(Debug, Clone, Default)]
pub struct SortCtx {
    /// The ORDER BY (or GROUP BY clause)
    pub p_order_by: ExprListId,
    /// Number of ORDER BY terms satisfied by indices
    pub n_ob_sat: i32,
    /// Cursor number for the sorter
    pub i_e_cursor: i32,
    /// Register holding block-output return address
    pub reg_return: i32,
    /// Start label for the block-output subroutine
    pub label_bk_out: i32,
    /// Address of the OP_SorterOpen or OP_OpenEphemeral
    pub addr_sort_index: i32,
    /// Jump here when done, ex: LIMIT reached
    pub label_done: i32,
    /// Jump here when sorter is full
    pub label_ob_lopt: i32,
    /// Zero or more SORTFLAG_* bits
    pub sort_flags: u8,
    /// Deferred row loading info or NULL
    pub p_deferred_row_load: Option<RowLoadInfo>,
}

/// `SORTFLAG_UseSorter`: use SorterOpen instead of OpenEphemeral
pub const SORTFLAG_USE_SORTER: u8 = 0x01;
/// `BTREE_UNORDERED`: use of a hash implementation is OK
const BTREE_UNORDERED: u16 = 8;

/// `sqlite3LogEst(x)`: convert an integer into a LogEst (10 times log base 2).
pub fn sqlite3_log_est(mut x: u64) -> LogEst {
    const A: [LogEst; 8] = [0, 2, 3, 5, 6, 7, 8, 9];
    let mut y: LogEst = 40;
    if x < 8 {
        if x < 2 {
            return 0;
        }
        while x < 8 {
            y -= 10;
            x <<= 1;
        }
    } else {
        let i = 60 - x.leading_zeros() as i32;
        y += (i * 10) as LogEst;
        x >>= i;
    }
    A[(x & 7) as usize] + y - 10
}

/// `sqlite3LogEstAdd(a, b)`: find (an approximate) sum of two LogEst values.
pub fn sqlite3_log_est_add(a: LogEst, b: LogEst) -> LogEst {
    const X: [u8; 32] = [
        10, 10, 9, 9, 8, 8, 7, 7, 7, 6, 6, 6, 5, 5, 5, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2,
    ];
    if a >= b {
        if a > b + 49 {
            return a;
        }
        if a > b + 31 {
            return a + 1;
        }
        a + X[(a - b) as usize] as LogEst
    } else {
        if b > a + 49 {
            return b;
        }
        if b > a + 31 {
            return b + 1;
        }
        b + X[(b - a) as usize] as LogEst
    }
}

/// `sqlite3SelectDestInit(pDest, eDest, iParm)`
pub fn sqlite3_select_dest_init(e_dest: i32, i_parm: i32) -> SelectDest {
    SelectDest { e_dest: e_dest as u8, i_sd_parm: i_parm, ..SelectDest::default() }
}

/// `sqlite3SelectNew(...)`: allocate a new Select structure and return a pointer to that
/// structure.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_select_new(
    p: &mut Parse,
    e_list: ExprListId,
    src: SrcListId,
    where_: ExprId,
    group_by: ExprListId,
    having: ExprId,
    order_by: ExprListId,
    sel_flags: u32,
    limit: ExprId,
) -> SelectId {
    let e_list = if e_list.is_null() {
        let star = sqlite3_expr(p, TK_ASTERISK, None);
        sqlite3_expr_list_append(p, ExprListId::NULL, star)
    } else {
        e_list
    };
    p.n_select += 1;
    let sel_id = p.n_select as u32;
    let src = if src.is_null() { p.new_src(SrcList::default()) } else { src };
    p.new_select(Select {
        p_e_list: e_list,
        op: TK_SELECT,
        sel_flags,
        i_limit: 0,
        i_offset: 0,
        sel_id,
        n_select_row: 0,
        p_src: src,
        p_where: where_,
        p_group_by: group_by,
        p_having: having,
        p_order_by: order_by,
        p_prior: SelectId::NULL,
        p_next: SelectId::NULL,
        p_limit: limit,
        p_with: WithId::NULL,
        p_win: WindowId::NULL,
        p_win_defn: WindowId::NULL,
    })
}

/// `findRightmost(p)`: return a pointer to the right-most SELECT statement in a compound.
fn find_rightmost(p: &Parse, mut s: SelectId) -> SelectId {
    while p.s(s).p_next.nn() {
        s = p.s(s).p_next;
    }
    s
}

/// `sqlite3KeyInfoFromExprList(pParse, pList, iStart, nExtra)`: given an expression list,
/// generate a KeyInfo structure that records the collating sequence for each expression in
/// that expression list. Space to hold the KeyInfo structure is obtained from malloc.
pub fn sqlite3_key_info_from_expr_list(p: &mut Parse, list: ExprListId, i_start: i32, n_extra: i32) -> KeyInfo {
    let n_expr = p.l(list).n_expr();
    let mut info = KeyInfo::alloc(p.enc(), (n_expr - i_start) as usize, (n_extra + 1) as usize);
    for i in i_start..n_expr {
        let (e, sort_flags) = {
            let it = &p.l(list).a[i as usize];
            (it.p_expr, it.sort_flags)
        };
        info.a_coll[(i - i_start) as usize] = Some(sqlite3_expr_nn_coll_seq(p, e));
        info.a_sort_flags[(i - i_start) as usize] = sort_flags;
    }
    info
}

/// `sqlite3SelectOpName(id)`: name of the connection operator, used for error messages.
pub fn sqlite3_select_op_name(id: u8) -> &'static str {
    match id {
        TK_ALL => "UNION ALL",
        TK_INTERSECT => "INTERSECT",
        TK_EXCEPT => "EXCEPT",
        _ => "UNION",
    }
}

/// `sqlite3GetVdbe(pParse)`: get a VDBE for the given parser context. Create a new one if
/// necessary.
pub fn sqlite3_get_vdbe(p: &mut Parse) {
    if p.has_vdbe {
        return;
    }
    // if( pParse->pToplevel==0 ) pParse->okConstFactor = 1;
    p.ok_const_factor = true;
    // sqlite3VdbeCreate(pParse)
    p.v = Vdbe::new();
    p.v.add_op2(OP_Init, 0, 1);
    p.has_vdbe = true;
}

/// `sqlite3CteNew` + `sqlite3WithAdd(pParse, pWith, pCte)`: add a new CTE to the end of the
/// WITH clause `with` (a new one is created if it is NULL).
pub fn sqlite3_with_add(p: &mut Parse, with: WithId, cte: Cte) -> WithId {
    // Check that the CTE name is unique within this WITH clause. If not, store an error in
    // the Parse structure.
    if with.nn() {
        let dup = p.a_with[with.ix()].a.iter().any(|c| c.z_name.eq_ignore_ascii_case(&cte.z_name));
        if dup {
            p.error_msg(format!("duplicate WITH table name: {}", cte.z_name));
        }
    }
    let with = if with.nn() {
        with
    } else {
        p.a_with.push(With::default());
        WithId(p.a_with.len() as u32 - 1)
    };
    p.a_with[with.ix()].a.push(cte);
    with
}

/// `sqlite3WithPush(pParse, pWith, bFree)`: the code generator maintains a stack of active
/// WITH clauses with the inner-most WITH clause being at the top of the stack. This routine
/// pushes the WITH clause passed as the second argument onto the top of the stack.
pub fn sqlite3_with_push(p: &mut Parse, with: WithId) -> WithId {
    if with.nn() && p.n_err == 0 {
        p.a_with[with.ix()].p_outer = p.p_with;
        p.p_with = with;
    }
    with
}

/// `sqlite3SelectPopWith(pWalker, p)`: if the SELECT passed as the second argument has an
/// associated WITH clause, pop it from the stack stored as part of the Parse object.
fn sqlite3_select_pop_with(p: &mut Parse, _w: &mut Walker, sel: SelectId) {
    if p.p_with.nn() && p.s(sel).p_prior.is_null() {
        let with = p.s(find_rightmost(p, sel)).p_with;
        if with.nn() {
            p.p_with = p.a_with[with.ix()].p_outer;
        }
    }
}

/// `convertCompoundSelectToSubquery(pWalker, p)`: this routine is a Walker callback for
/// sqlite3SelectExpand(). If the given Select statement is a compound (with an operator
/// other than UNION ALL) and has an ORDER BY whose terms use a COLLATE operator, convert
/// the statement by pushing the compound down into a subquery and making the outer query a
/// simple SELECT with the ORDER BY:
///
///     SELECT x FROM t1 UNION SELECT y FROM t2 ORDER BY 1 COLLATE nocase
///     =>  SELECT * FROM (SELECT x FROM t1 UNION SELECT y FROM t2) ORDER BY 1 COLLATE nocase
///
/// The merge algorithm of a compound cannot use a collation other than those of its arms.
fn convert_compound_select_to_subquery(p: &mut Parse, _w: &mut Walker, sel: SelectId) -> i32 {
    if p.s(sel).p_prior.is_null() {
        return WRC_Continue;
    }
    if p.s(sel).p_order_by.is_null() {
        return WRC_Continue;
    }
    let mut x = sel;
    while x.nn() && (p.s(x).op == TK_ALL || p.s(x).op == TK_SELECT) {
        x = p.s(x).p_prior;
    }
    if x.is_null() {
        return WRC_Continue;
    }
    let ob = p.s(sel).p_order_by;
    // If iOrderByCol is already non-zero, then it has already been matched to a result
    // column of the SELECT statement. This occurs when the SELECT is rewritten for
    // window-functions processing and then passed to sqlite3SelectPrep() and similar a
    // second time. The rewriting done by this function is not required in this case.
    if p.l(ob).a[0].i_order_by_col != 0 {
        return WRC_Continue;
    }
    let has_collate = p.l(ob).a.iter().any(|it| p.e(it.p_expr).flags & EP_Collate != 0);
    if !has_collate {
        return WRC_Continue;
    }

    // If we reach this point, that means the transformation is required.
    // pNew = sqlite3DbMallocZero(db, sizeof(*pNew) )
    let new = p.new_select(Select::default());
    let dummy = Token::EMPTY;
    let new_src = crate::build::sqlite3_src_list_append_from_term(p, SrcListId::NULL, None, None, &dummy, new, None);
    if p.n_err != 0 {
        return WRC_Abort;
    }
    // *pNew = *p;
    let copy = p.s(sel).clone();
    *p.sm(new) = copy;
    let star = sqlite3_expr(p, TK_ASTERISK, None);
    let el = sqlite3_expr_list_append(p, ExprListId::NULL, star);
    {
        let x = p.sm(sel);
        x.p_src = new_src;
        x.p_e_list = el;
        x.op = TK_SELECT;
        x.p_where = ExprId::NULL;
    }
    {
        let n = p.sm(new);
        n.p_group_by = ExprListId::NULL;
        n.p_having = ExprId::NULL;
        n.p_order_by = ExprListId::NULL;
    }
    {
        let x = p.sm(sel);
        x.p_prior = SelectId::NULL;
        x.p_next = SelectId::NULL;
        x.p_with = WithId::NULL;
        x.p_win_defn = WindowId::NULL;
        x.sel_flags &= !SF_Compound;
        x.sel_flags |= SF_Converted;
    }
    let prior = p.s(new).p_prior;
    p.sm(prior).p_next = new;
    p.sm(new).p_limit = ExprId::NULL;
    WRC_Continue
}

/// `sqlite3JoinType(pParse, pA, pB, pC)`: given 1 to 3 identifiers preceding the JOIN
/// keyword, determine the type of join. Return an integer constant that expresses that type
/// in terms of the following bit values:
///
///     JT_INNER
///     JT_CROSS
///     JT_OUTER
///     JT_NATURAL
///     JT_LEFT
///     JT_RIGHT
///
/// A full outer join is the combination of JT_LEFT and JT_RIGHT.
///
/// If an illegal or unsupported join type is seen, then still return a join type, but put an
/// error in the pParse structure.
///
/// These are the valid join types:
///
///     pA       pB       pC               Return Value
///     -------  -----    -----            ------------
///     CROSS      -        -                 JT_CROSS
///     INNER      -        -                 JT_INNER
///     LEFT       -        -                 JT_LEFT|JT_OUTER
///     LEFT     OUTER      -                 JT_LEFT|JT_OUTER
///     RIGHT      -        -                 JT_RIGHT|JT_OUTER
///     RIGHT    OUTER      -                 JT_RIGHT|JT_OUTER
///     FULL       -        -                 JT_LEFT|JT_RIGHT|JT_OUTER
///     FULL     OUTER      -                 JT_LEFT|JT_RIGHT|JT_OUTER
///     NATURAL  INNER      -                 JT_NATURAL|JT_INNER
///     NATURAL  LEFT       -                 JT_NATURAL|JT_LEFT|JT_OUTER
///     NATURAL  LEFT     OUTER               JT_NATURAL|JT_LEFT|JT_OUTER
///     NATURAL  RIGHT      -                 JT_NATURAL|JT_RIGHT|JT_OUTER
///     NATURAL  RIGHT    OUTER               JT_NATURAL|JT_RIGHT|JT_OUTER
///     NATURAL  FULL       -                 JT_NATURAL|JT_LEFT|JT_RIGHT
///     NATURAL  FULL     OUTER               JT_NATRUAL|JT_LEFT|JT_RIGHT
///
/// To preserve historical compatibly, SQLite also accepts a variety of other non-standard
/// and in many cases nonsensical join types. This routine makes as much sense at it can
/// from the nonsense join type and returns a result. Examples of accepted nonsense join
/// types include:
///
///     INNER CROSS JOIN        ->   same as JOIN
///     NATURAL CROSS JOIN      ->   same as NATURAL JOIN
///     OUTER LEFT JOIN         ->   same as LEFT JOIN
///     LEFT NATURAL JOIN       ->   same as NATURAL LEFT JOIN
///     LEFT RIGHT JOIN         ->   same as FULL JOIN
///     RIGHT OUTER FULL JOIN   ->   same as FULL JOIN
///     CROSS CROSS CROSS JOIN  ->   same as JOIN
///
/// The only restrictions on the join type name are:
///
///    *   "INNER" cannot appear together with "OUTER", "LEFT", "RIGHT", or "FULL".
///
///    *   "CROSS" cannot appear together with "OUTER", "LEFT", "RIGHT, or "FULL".
///
///    *   If "OUTER" is present then there must also be one of "LEFT", "RIGHT", or "FULL".
pub fn sqlite3_join_type(p: &mut Parse, a: &Token, b: Option<&Token>, c: Option<&Token>) -> i32 {
    const A_KEYWORD: [(&str, i32); 7] = [
        /* (0) natural */ ("natural", JT_NATURAL),
        /* (1) left    */ ("left", JT_LEFT | JT_OUTER),
        /* (2) outer   */ ("outer", JT_OUTER),
        /* (3) right   */ ("right", JT_RIGHT | JT_OUTER),
        /* (4) full    */ ("full", JT_LEFT | JT_RIGHT | JT_OUTER),
        /* (5) inner   */ ("inner", JT_INNER),
        /* (6) cross   */ ("cross", JT_INNER | JT_CROSS),
    ];
    let mut jointype = 0;
    let ap_all = [Some(a), b, c];
    for t in ap_all.iter().take_while(|t| t.is_some()).flatten() {
        let z = p.tok_str(t);
        let mut found = false;
        for (kw, code) in A_KEYWORD.iter() {
            if z.len() == kw.len() && z.eq_ignore_ascii_case(kw) {
                jointype |= code;
                found = true;
                break;
            }
        }
        if !found {
            jointype |= JT_ERROR;
            break;
        }
    }
    if (jointype & (JT_INNER | JT_OUTER)) == (JT_INNER | JT_OUTER)
        || (jointype & JT_ERROR) != 0
        || (jointype & (JT_OUTER | JT_LEFT | JT_RIGHT)) == JT_OUTER
    {
        let sp1 = if b.is_none() { "" } else { " " };
        let sp2 = if c.is_none() { "" } else { " " };
        let msg = format!(
            "unknown join type: {}{}{}{}{}",
            p.tok_str(a),
            sp1,
            b.map_or("", |t| p.tok_str(t)),
            sp2,
            c.map_or("", |t| p.tok_str(t))
        );
        p.error_msg_tok(msg, a);
        jointype = JT_INNER;
    }
    jointype
}

/// `sqlite3SrcItemColumnUsed(pItem, iCol)`: mark a subquery result column as having been
/// used.
pub fn sqlite3_src_item_column_used(p: &mut Parse, src: SrcListId, i: usize, i_col: i32) {
    if p.src(src).a[i].is_nested_from {
        let sel = p.src(src).a[i].p_subq.as_ref().expect("subquery").p_select;
        let results = p.s(sel).p_e_list;
        p.lm(results).a[i_col as usize].b_used = true;
    }
}

/// `tableAndColumnIndex(pSrc, iStart, iEnd, zCol, piTab, piCol, bIgnoreHidden)`: search the
/// tables iStart..iEnd (inclusive) in pSrc, looking for a table that has a column named
/// zCol. The search is left-to-right. The first match found is returned.
///
/// When found, set *piTab and *piCol to the table index and column index of the matching
/// column and return TRUE.
///
/// If not found, return FALSE.
fn table_and_column_index(
    p: &mut Parse,
    src: SrcListId,
    i_start: usize,
    i_end: usize,
    z_col: &str,
    out: Option<(&mut usize, &mut i32)>,
    b_ignore_hidden: bool,
) -> bool {
    let mut out = out;
    let mut i = i_start;
    while i <= i_end {
        let tab = p.src(src).a[i].p_s_tab;
        let i_col = sqlite3_column_index(p, tab, z_col);
        if i_col >= 0 && (!b_ignore_hidden || (p.tab(tab).a_col[i_col as usize].col_flags & COLFLAG_HIDDEN as u16) == 0) {
            if let Some((pi_tab, pi_col)) = out.as_mut() {
                sqlite3_src_item_column_used(p, src, i, i_col);
                **pi_tab = i;
                **pi_col = i_col;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// `sqlite3SetJoinExpr(p, iTable, joinFlag)`: set the EP_OuterON property on all terms of
/// the given expression. And set the Expr.w.iJoin to iTable for every term in the
/// expression.
///
/// The EP_OuterON property is used on terms of an expression to tell the OUTER JOIN
/// processing logic that this term is part of the join restriction specified in the ON or
/// USING clause and not a part of the more general WHERE clause. These terms are moved over
/// to the WHERE clause during join processing but we need to remember that they originated
/// in the ON or USING clause.
///
/// The Expr.w.iJoin tells the WHERE clause processing that the expression depends on table
/// w.iJoin even if that table is not explicitly mentioned in the expression. That
/// information is needed for cases like this:
///
///    SELECT * FROM t1 LEFT JOIN t2 ON t1.a=t2.b AND t1.x=5
///
/// The where clause needs to defer the handling of the t1.x=5 term until after the t2 loop
/// of the join. In that way, a NULL t2 row will be inserted whenever t1.x!=5. If we do not
/// defer the handling of t1.x=5, it will be processed immediately after the t1 loop and
/// rows with t1.x!=5 will never appear in the output, which is incorrect.
pub fn sqlite3_set_join_expr(p: &mut Parse, mut e: ExprId, i_table: i32, join_flag: u32) {
    while e.nn() {
        {
            let x = p.em(e);
            x.flags |= join_flag;
            x.w = i_table;
        }
        if p.e(e).use_x_list() {
            let l = p.e(e).x_list();
            if l.nn() {
                for i in 0..p.l(l).a.len() {
                    let c = p.l(l).a[i].p_expr;
                    sqlite3_set_join_expr(p, c, i_table, join_flag);
                }
            }
        }
        let left = p.e(e).p_left;
        sqlite3_set_join_expr(p, left, i_table, join_flag);
        e = p.e(e).p_right;
    }
}

/// `unsetJoinExpr(p, iTable, nullable)`: undo the work of sqlite3SetJoinExpr(). This is used
/// when a LEFT JOIN is simplified into an ordinary JOIN, and when an ON expression is
/// optimized out. So instead of unsetting the EP_OuterON property, we change it to
/// EP_InnerON.
///
/// If the nullable flag is true, that means that the table is still the RHS of a RIGHT JOIN
/// (or FULL JOIN) and so its columns can still be NULL: the EP_CanBeNull flag stays.
pub(crate) fn unset_join_expr(p: &mut Parse, mut e: ExprId, i_table: i32, nullable: bool) {
    while e.nn() {
        {
            let x = p.em(e);
            if i_table < 0 || (x.has(EP_OuterON) && x.w == i_table) {
                x.flags &= !(EP_OuterON | EP_InnerON);
                if i_table >= 0 {
                    x.flags |= EP_InnerON;
                }
            }
            if x.op == TK_COLUMN && x.i_table == i_table && !nullable {
                x.flags &= !EP_CanBeNull;
            }
        }
        if p.e(e).op == TK_FUNCTION {
            let l = p.e(e).x_list();
            if l.nn() {
                for i in 0..p.l(l).a.len() {
                    let c = p.l(l).a[i].p_expr;
                    unset_join_expr(p, c, i_table, nullable);
                }
            }
        }
        let left = p.e(e).p_left;
        unset_join_expr(p, left, i_table, nullable);
        e = p.e(e).p_right;
    }
}

/// `sqlite3ProcessJoin(pParse, p)`: this routine processes the join information for a SELECT
/// statement.
///
///   *  A NATURAL join is converted into a USING join. After that, we do not need to be
///      concerned with NATURAL joins and we only have think about USING joins.
///
///   *  ON and USING clauses result in extra terms being added to the WHERE clause to
///      enforce the specified constraints. The extra WHERE clause terms will be tagged with
///      EP_OuterON or EP_InnerON so that we can distinguish them from the original SQL text.
///
/// The terms of a FROM clause are contained in the Select.pSrc structure. The left most
/// table is the first entry in Select.pSrc. The right-most table is the last entry. The join
/// operator is held in the entry to the right. Thus entry 1 contains the join operator for
/// the join between entries 0 and 1. Any ON or USING clauses associated with the join are
/// also attached to the right entry.
///
/// This routine returns the number of errors encountered.
fn sqlite3_process_join(p: &mut Parse, sel: SelectId) -> i32 {
    let src = p.s(sel).p_src;
    let n_src = p.src(src).a.len();
    let mut b_seen_t_func = false; // True if have seen table-valued function
    let mut i = 0;
    while i + 1 < n_src {
        let right_tab = p.src(src).a[i + 1].p_s_tab;
        if p.src(src).a[i].p_s_tab.is_null() || right_tab.is_null() {
            i += 1;
            continue;
        }
        let join_type = if (p.src(src).a[i + 1].jointype as i32 & JT_OUTER) != 0 { EP_OuterON } else { EP_InnerON };

        // If this is a NATURAL join, synthesize an appropriate USING clause to specify which
        // columns should be joined.
        if p.src(src).a[i + 1].jointype as i32 & JT_NATURAL != 0 {
            let mut using = IdListId::NULL;
            if p.src(src).a[i + 1].is_using || p.src(src).a[i + 1].p_on.nn() {
                p.error_msg("a NATURAL join may not have an ON or USING clause");
                return 1;
            }
            for j in 0..p.tab(right_tab).a_col.len() {
                if p.tab(right_tab).a_col[j].col_flags & COLFLAG_HIDDEN as u16 != 0 {
                    continue;
                }
                let z_name = p.tab(right_tab).a_col[j].z_cn_name.clone(); // Name of column in the right table
                if table_and_column_index(p, src, 0, i, &z_name, None, true) {
                    using = crate::build::sqlite3_id_list_append_str(p, using, z_name);
                }
            }
            if using.nn() {
                let it = &mut p.srcm(src).a[i + 1];
                it.is_using = true;
                it.is_synth_using = true;
                it.p_using = using;
            }
            if p.n_err != 0 {
                return 1;
            }
        }

        // Create extra terms on the WHERE clause for each column named in the USING clause.
        // Example: If the two tables to be joined are A and B and the USING clause names X,
        // Y, and Z, then add this to the WHERE clause:    A.X=B.X AND A.Y=B.Y AND A.Z=B.Z
        // Report an error if any column mentioned in the USING clause is not contained in
        // both tables to be joined.
        if p.src(src).a[i + 1].is_using {
            let list = p.src(src).a[i + 1].p_using;
            let is_synth_using = p.src(src).a[i + 1].is_synth_using;
            for j in 0..p.a_idlist[list.ix()].a.len() {
                let z_name = p.a_idlist[list.ix()].a[j].clone(); // Name of the term in the USING clause
                let mut i_left = 0usize; // Table on the left with matching column name
                let mut i_left_col = 0i32; // Column number of matching column on the left
                let i_right_col = sqlite3_column_index(p, right_tab, &z_name); // Column number of matching column on the right
                if i_right_col < 0
                    || !table_and_column_index(p, src, 0, i, &z_name, Some((&mut i_left, &mut i_left_col)), is_synth_using)
                {
                    p.error_msg(format!("cannot join using column {z_name} - column not present in both tables"));
                    return 1;
                }
                let mut e1 = sqlite3_create_column_expr(p, src, i_left, i_left_col); // Reference to the column on the LEFT of the join
                sqlite3_src_item_column_used(p, src, i_left, i_left_col);
                if (p.src(src).a[0].jointype as i32 & JT_LTORJ) != 0 && p.n_err == 0 {
                    // This branch runs if the query contains one or more RIGHT or FULL JOINs.
                    // If only a single table on the left side of this join contains the
                    // zName column, then this branch is a no-op. But if there are two or
                    // more tables on the left side of the join, construct a coalesce()
                    // function that gathers all such tables. Raise an error if more than one
                    // of those references to zName is not also within a prior USING clause.
                    //
                    // We really ought to raise an error if there are two or more non-USING
                    // references to zName on the left of an INNER or LEFT JOIN. But older
                    // versions of SQLite do not do that, so we avoid adding a new error so
                    // as to not break legacy applications.
                    let mut func_args = ExprListId::NULL; // Arguments to the coalesce()
                    p.em(e1).flags |= EP_CanBeNull;
                    while i_left + 1 <= i
                        && table_and_column_index(p, src, i_left + 1, i, &z_name, Some((&mut i_left, &mut i_left_col)), is_synth_using)
                    {
                        if !p.src(src).a[i_left].is_using
                            || crate::build::sqlite3_id_list_index(p, p.src(src).a[i_left].p_using, &z_name) < 0
                        {
                            p.error_msg(format!("ambiguous reference to {z_name} in USING()"));
                            break;
                        }
                        func_args = sqlite3_expr_list_append(p, func_args, e1);
                        e1 = sqlite3_create_column_expr(p, src, i_left, i_left_col);
                        sqlite3_src_item_column_used(p, src, i_left, i_left_col);
                    }
                    if func_args.nn() {
                        func_args = sqlite3_expr_list_append(p, func_args, e1);
                        e1 = sqlite3_expr_function_str(p, func_args, "coalesce");
                        if e1.nn() {
                            p.em(e1).aff_expr = SQLITE_AFF_DEFER;
                        }
                    }
                } else if (p.src(src).a[i + 1].jointype as i32 & JT_LEFT) != 0 && p.n_err == 0 {
                    p.em(e1).flags |= EP_CanBeNull;
                }
                let e2 = sqlite3_create_column_expr(p, src, i + 1, i_right_col); // Reference to the column on the RIGHT of the join
                sqlite3_src_item_column_used(p, src, i + 1, i_right_col);
                let eq = sqlite3_p_expr(p, TK_EQ, e1, e2); // Equality constraint. pE1 == pE2
                if eq.nn() {
                    let t = p.e(e2).i_table;
                    let x = p.em(eq);
                    x.flags |= join_type;
                    x.w = t;
                }
                let w0 = p.s(sel).p_where;
                let w1 = sqlite3_expr_and(p, w0, eq);
                p.sm(sel).p_where = w1;
            }
        }
        // Add the ON clause to the end of the WHERE clause, connected by an AND operator.
        else if p.src(src).a[i + 1].p_on.nn() {
            let (on, cur) = (p.src(src).a[i + 1].p_on, p.src(src).a[i + 1].i_cursor);
            sqlite3_set_join_expr(p, on, cur, join_type);
            let w0 = p.s(sel).p_where;
            let w1 = sqlite3_expr_and(p, w0, on);
            p.sm(sel).p_where = w1;
            let it = &mut p.srcm(src).a[i + 1];
            it.p_on = ExprId::NULL;
            it.is_on = true;
            p.sm(sel).sel_flags |= SF_OnToWhere;
        }

        if !b_seen_t_func {
            b_seen_t_func = p.src(src).a[i].is_tab_func && p.src(src).a[i].p_func_arg.nn();
        }

        // We also need to call sqlite3SelectCheckOnClauses() to verify that table-valued
        // function arguments do not illegally refer to any tables to their right. This test
        // is required if either (a) there is a RIGHT JOIN in the SrcList, or (b) there is a
        // LEFT JOIN to the right of a table-valued function.
        let r = &p.src(src).a[i + 1];
        if (r.is_tab_func && join_type == EP_OuterON && r.p_func_arg.nn()) || (b_seen_t_func && join_type == EP_OuterON) {
            p.sm(sel).sel_flags |= SF_OnToWhere;
        }
        i += 1;
    }
    0
}

/// `sqlite3ExprFunction(pParse, pList, &tk, 0)` for a function whose name is a string of
/// the code generator rather than a token of the SQL text (`tkCoalesce` in
/// `sqlite3ProcessJoin`).
pub fn sqlite3_expr_function_str(p: &mut Parse, list: ExprListId, name: &str) -> ExprId {
    let e = sqlite3_expr_alloc_str(p, TK_FUNCTION, Some(name), 1);
    p.em(e).x = list.0;
    p.em(e).flags |= EP_HasFunc;
    sqlite3_expr_set_height_and_flags(p, e);
    e
}

/// `sqlite3IndexedByLookup(pParse, pFrom)`: if the source-list item passed as an argument
/// was augmented with an INDEXED BY clause, then try to locate the specified index. If there
/// was such a clause and the named index cannot be found, return SQLITE_ERROR and leave an
/// error in pParse. Otherwise, populate pFrom->pIndex and return SQLITE_OK.
pub fn sqlite3_indexed_by_lookup(p: &mut Parse, src: SrcListId, i: usize) -> i32 {
    let tab = p.src(src).a[i].p_s_tab;
    let z_indexed_by = p.src(src).a[i].z_indexed_by.clone().unwrap_or_default();
    let found = p.tab(tab).p_index.iter().copied().find(|&ix| p.idx(ix).z_name.eq_ignore_ascii_case(&z_indexed_by));
    match found {
        None => {
            p.error_msg(format!("no such index: {z_indexed_by}"));
            p.check_schema = true;
            SQLITE_ERROR
        }
        Some(ix) => {
            p.srcm(src).a[i].p_ib_index = ix;
            SQLITE_OK
        }
    }
}

/// `cannotBeFunction(pParse, pFrom)`: check to see if the FROM clause term pFrom has
/// table-valued function arguments. If it does, leave an error message in pParse and return
/// non-zero. Since pFrom is not a table-valued function, the arguments are an error.
fn cannot_be_function(p: &mut Parse, src: SrcListId, i: usize) -> bool {
    if p.src(src).a[i].is_tab_func {
        let name = p.src(src).a[i].z_name.clone().unwrap_or_default();
        p.error_msg(format!("'{name}' is not a function"));
        return true;
    }
    false
}

/// `searchWith(pWith, pItem, ppContext)`: argument pWith (which may be NULL) points to a
/// linked list of nested WITH contexts, from inner to outermost. If the table identified by
/// FROM clause element pItem is really a common-table-expression (CTE) then return a pointer
/// to the CTE definition for that table (the WITH clause and the index of the CTE in it).
/// Otherwise return NULL.
fn search_with(p: &Parse, with: WithId, z_name: &str) -> Option<(WithId, usize)> {
    let mut w = with;
    while w.nn() {
        let wi = &p.a_with[w.ix()];
        for (i, c) in wi.a.iter().enumerate() {
            if z_name.eq_ignore_ascii_case(&c.z_name) {
                return Some((w, i));
            }
        }
        if wi.b_view != 0 {
            break;
        }
        w = wi.p_outer;
    }
    None
}

/// `resolveFromTermToCte(pParse, pWalker, pFrom)`: this function checks if argument pFrom
/// refers to a CTE declared by a WITH clause of some SELECT statement in the stack of
/// contexts. If so, it populates the pFrom->pTab object and returns 1. If it is not a CTE,
/// 0 is returned; on an error 2 is returned with the message in pParse.
///
/// A recursive CTE is reported as unsupported (the code generator for it,
/// `generateWithRecursiveQuery`, is a later step).
fn resolve_from_term_to_cte(p: &mut Parse, w: &mut Walker, src: SrcListId, i_from: usize) -> i32 {
    if p.p_with.is_null() {
        // There are no WITH clauses in the stack. No match is possible
        return 0;
    }
    if p.n_err != 0 {
        // Prior errors might have left pParse->pWith in a goofy state, so go no further.
        return 0;
    }
    {
        let it = &p.src(src).a[i_from];
        if !it.fixed_schema && it.z_database.is_some() {
            // The FROM term contains a schema qualifier (ex: main.t1) and so it cannot
            // possibly be a CTE reference.
            return 0;
        }
        if it.not_cte {
            // The FROM term is specifically excluded from matching a CTE.
            return 0;
        }
    }
    let z_name = p.src(src).a[i_from].z_name.clone().unwrap_or_default();
    let Some((with, i_cte)) = search_with(p, p.p_with, &z_name) else {
        return 0; // No match
    };
    let cte = p.a_with[with.ix()].a[i_cte].clone();
    let mut i_rec_tab = -1; // Cursor for recursive table

    // If pCte->zCteErr is non-NULL at this point, then this is an illegal recursive
    // reference to CTE pCte. Leave an error in pParse and return early. If pCte->zCteErr is
    // NULL, then this is not a recursive reference. In this case, proceed.
    if let Some(fmt) = cte.z_cte_err {
        p.error_msg(fmt.replace("%s", &cte.z_name));
        return 2;
    }
    if cannot_be_function(p, src, i_from) {
        return 2;
    }
    let mut cte_use = cte.p_use;
    if cte_use == 0 {
        p.a_cte_use.push(CteUse { e_m10d: cte.e_m10d, ..CteUse::default() });
        cte_use = p.a_cte_use.len() as u32;
        p.a_with[with.ix()].a[i_cte].p_use = cte_use;
    }
    let tab = p.new_tab(Table {
        z_name: cte.z_name.clone(),
        n_tab_ref: 1,
        i_p_key: -1,
        n_row_log_est: 200,
        tab_flags: TF_Ephemeral | TF_NoVisibleRowid,
        i_db: -1,
        ..Table::default()
    });
    p.srcm(src).a[i_from].p_s_tab = tab;
    crate::build::sqlite3_src_item_attach_subquery(p, src, i_from, cte.p_select, true);
    let sel = p.src(src).a[i_from].p_subq.as_ref().expect("subquery").p_select;
    p.sm(sel).sel_flags |= SF_CopyCte;
    if p.src(src).a[i_from].is_indexed_by {
        let z = p.src(src).a[i_from].z_indexed_by.clone().unwrap_or_default();
        p.error_msg(format!("no such index: \"{z}\""));
        return 2;
    }
    {
        let it = &mut p.srcm(src).a[i_from];
        it.is_cte = true;
        it.p_cte_use = cte_use;
    }
    p.a_cte_use[cte_use as usize - 1].n_use += 1;

    // Check if this is a recursive CTE.
    let mut rec_term = sel; // Left-most recursive term
    let b_may_recursive = p.s(sel).op == TK_ALL || p.s(sel).op == TK_UNION;
    while b_may_recursive && p.s(rec_term).op == p.s(sel).op {
        let rsrc = p.s(rec_term).p_src;
        for i in 0..p.src(rsrc).a.len() {
            let hit = {
                let it = &p.src(rsrc).a[i];
                it.z_name.is_some()
                    && !it.had_schema
                    && !it.is_subquery
                    && (it.fixed_schema || it.z_database.is_none())
                    && it.z_name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&cte.z_name))
            };
            if hit {
                p.srcm(rsrc).a[i].p_s_tab = tab;
                p.tabm(tab).n_tab_ref += 1;
                p.srcm(rsrc).a[i].is_recursive = true;
                if p.s(rec_term).sel_flags & SF_Recursive != 0 {
                    p.error_msg(format!("multiple references to recursive table: {}", cte.z_name));
                    return 2;
                }
                p.sm(rec_term).sel_flags |= SF_Recursive;
                if i_rec_tab < 0 {
                    i_rec_tab = p.n_tab;
                    p.n_tab += 1;
                }
                p.srcm(rsrc).a[i].i_cursor = i_rec_tab;
            }
        }
        if (p.s(rec_term).sel_flags & SF_Recursive) == 0 {
            break;
        }
        rec_term = p.s(rec_term).p_prior;
    }

    if p.n_tab >= p.db.a_limit[SQLITE_LIMIT_EXPR_DEPTH as usize] {
        // Bug 2026-10-04T05:35:14Z: Prevent nested CTEs from generating an exponential
        // number of cursors. The error message here will be "Nested too deep", which isn't
        // exactly correct, but it is sufficient, and we don't want to use extra code space
        // for more detail on such an obscure error.
        p.n_nest_sel = p.n_tab;
    }
    p.n_nest_sel += 1;
    if check_subquery_nesting_depth(p) {
        return 2;
    }

    p.a_with[with.ix()].a[i_cte].z_cte_err = Some("circular reference: %s");
    let saved_with = p.p_with; // Initial value of pParse->pWith
    p.p_with = with;
    if p.s(sel).sel_flags & SF_Recursive != 0 {
        let sw = p.s(sel).p_with;
        p.sm(rec_term).p_with = sw;
        let rc = sqlite3_walk_select(p, w, rec_term);
        p.sm(rec_term).p_with = WithId::NULL;
        if rc != 0 {
            p.p_with = saved_with;
            return 2;
        }
    } else if sqlite3_walk_select(p, w, sel) != 0 {
        p.p_with = saved_with;
        return 2;
    }
    p.p_with = with;
    p.n_nest_sel -= 1;

    let mut left = sel; // Left-most SELECT statement
    while p.s(left).p_prior.nn() {
        left = p.s(left).p_prior;
    }
    let mut e_list = p.s(left).p_e_list;
    if cte.p_cols.nn() {
        if e_list.nn() && p.l(e_list).n_expr() != p.l(cte.p_cols).n_expr() {
            p.error_msg(format!(
                "table {} has {} values for {} columns",
                cte.z_name,
                p.l(e_list).n_expr(),
                p.l(cte.p_cols).n_expr()
            ));
            p.p_with = saved_with;
            return 2;
        }
        e_list = cte.p_cols;
    }

    let cols = sqlite3_columns_from_expr_list(p, e_list);
    p.tabm(tab).a_col = cols;
    if b_may_recursive {
        if p.s(sel).sel_flags & SF_Recursive != 0 {
            p.a_with[with.ix()].a[i_cte].z_cte_err = Some("multiple recursive references: %s");
        } else {
            p.a_with[with.ix()].a[i_cte].z_cte_err = Some("recursive reference in a subquery: %s");
        }
        sqlite3_walk_select(p, w, sel);
    }
    p.a_with[with.ix()].a[i_cte].z_cte_err = None;
    p.p_with = saved_with;
    1 // Success
}

/// `sqlite3ExpandSubquery(pParse, pFrom)`: the SrcItem structure passed as the second
/// argument represents a sub-query in the FROM clause of a SELECT statement. This function
/// allocates and populates the SrcItem.pTab object. If successful, SQLITE_OK is returned.
/// Otherwise, if an OOM error is encountered, SQLITE_NOMEM.
pub fn sqlite3_expand_subquery(p: &mut Parse, src: SrcListId, i_from: usize) -> i32 {
    let mut sel = p.src(src).a[i_from].p_subq.as_ref().map(|q| q.p_select).unwrap_or(SelectId::NULL);
    // pTab->zName = sqlite3MPrintf(pParse->db, "%!S", pFrom)
    let z_name = match p.src(src).a[i_from].z_alias.clone() {
        Some(a) => a,
        None => crate::wherecode::src_item_name(p, src, i_from, true),
    };
    while p.s(sel).p_prior.nn() {
        sel = p.s(sel).p_prior;
    }
    let e_list = p.s(sel).p_e_list;
    let a_col = sqlite3_columns_from_expr_list(p, e_list);
    let tab = p.new_tab(Table {
        z_name,
        a_col,
        n_tab_ref: 1,
        i_p_key: -1,
        e_tab_type: TABTYP_VIEW as u8,
        n_row_log_est: 200,
        // The usual case - do not allow ROWID on a subquery
        tab_flags: TF_Ephemeral | TF_NoVisibleRowid,
        i_db: -1,
        ..Table::default()
    });
    p.srcm(src).a[i_from].p_s_tab = tab;
    if p.n_err != 0 { SQLITE_ERROR } else { SQLITE_OK }
}

/// `inAnyUsingClause(zName, pBase, N)`: check the N SrcItem objects to the right of pBase.
/// (N might be zero!) If any of those SrcItem objects have a USING clause containing zName
/// then return true.
///
/// If N is zero, or none of the N SrcItem objects to the right of pBase contains a USING
/// clause, or if none of the USING clauses contain zName, then return false.
fn in_any_using_clause(p: &Parse, z_name: &str, src: SrcListId, i_base: usize, mut n: i32) -> bool {
    let mut k = i_base;
    while n > 0 {
        n -= 1;
        k += 1;
        let it = &p.src(src).a[k];
        if !it.is_using {
            continue;
        }
        if it.p_using.is_null() {
            continue;
        }
        if crate::build::sqlite3_id_list_index(p, it.p_using, z_name) >= 0 {
            return true;
        }
    }
    false
}

/// `sqlite3RowidAlias(pTab)` (expr.c): return a pointer to a buffer containing a usable
/// rowid alias for table pTab, or NULL if no such alias exists.
fn sqlite3_rowid_alias(p: &Parse, tab: TableId) -> Option<&'static str> {
    const AZ_OPT: [&str; 3] = ["_ROWID_", "ROWID", "OID"];
    AZ_OPT.iter().copied().find(|&z| sqlite3_column_index(p, tab, z) < 0)
}

/// `selectExpander(pWalker, p)`: this routine is a Walker callback for "expanding" a SELECT
/// statement. "Expanding" means to do the following:
///
///    (1)  Make sure VDBE cursor numbers have been assigned to every element of the FROM
///         clause.
///
///    (2)  Fill in the pTabList->a[].pTab fields in the SrcList that defines FROM clause.
///         When views appear in the FROM clause, fill in pTabList->a[].pSelect with a copy
///         of the SELECT statement that implements the view. A copy is made of the view's
///         SELECT statement so that we can freely modify or delete that statement without
///         worrying about messing up the persistent representation of the view.
///
///    (3)  Add terms to the WHERE clause to accommodate the NATURAL keyword on joins and the
///         ON and USING clause of joins.
///
///    (4)  Scan the list of columns in the result set (pEList) looking for instances of the
///         "*" operator or "TABLE.*" operator. If found, expand each "*" to be every column
///         in every table and TABLE.* to be every column in TABLE.
fn select_expander(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let sel_flags = p.s(sel).sel_flags;
    let mut elist_flags: u32 = 0;
    p.sm(sel).sel_flags |= SF_Expanded;
    if (sel_flags & SF_Expanded) != 0 {
        return WRC_Prune;
    }
    if w.e_code != 0 {
        // Renumber selId because it has been copied from a view
        p.n_select += 1;
        p.sm(sel).sel_id = p.n_select as u32;
    }
    let tab_list = p.s(sel).p_src;
    let e_list = p.s(sel).p_e_list;
    if p.p_with.nn() && (p.s(sel).sel_flags & SF_View) != 0 {
        if p.s(sel).p_with.is_null() {
            p.a_with.push(With::default());
            let wid = WithId(p.a_with.len() as u32 - 1);
            p.sm(sel).p_with = wid;
        }
        let wid = p.s(sel).p_with;
        p.a_with[wid.ix()].b_view = 1;
    }
    let with = p.s(sel).p_with;
    sqlite3_with_push(p, with);

    // Make sure cursor numbers have been assigned to all entries in the FROM clause of the
    // SELECT statement.
    sqlite3_src_list_assign_cursors(p, tab_list);

    // Look up every table named in the FROM clause of the select. If an entry of the FROM
    // clause is a subquery instead of a table or view, then create a transient table
    // structure to describe the subquery.
    for i in 0..p.src(tab_list).a.len() {
        if p.src(tab_list).a[i].p_s_tab.nn() {
            continue;
        }
        if p.src(tab_list).a[i].z_name.is_none() {
            // A sub-query in the FROM clause of a SELECT
            let sub = p.src(tab_list).a[i].p_subq.as_ref().map(|q| q.p_select).unwrap_or(SelectId::NULL);
            if sqlite3_walk_select(p, w, sub) != 0 {
                return WRC_Abort;
            }
            if sqlite3_expand_subquery(p, tab_list, i) != 0 {
                return WRC_Abort;
            }
        } else {
            let rc = resolve_from_term_to_cte(p, w, tab_list, i);
            if rc != 0 {
                if rc > 1 {
                    return WRC_Abort;
                }
            } else {
                // An ordinary table or view name in the FROM clause
                let tab = crate::schema::sqlite3_locate_table_item(p, 0, tab_list, i);
                p.srcm(tab_list).a[i].p_s_tab = tab;
                if tab.is_null() {
                    return WRC_Abort;
                }
                if p.tab(tab).n_tab_ref >= 0xffff {
                    let name = p.tab(tab).z_name.clone();
                    p.error_msg(format!("too many references to \"{name}\": max 65535"));
                    p.srcm(tab_list).a[i].p_s_tab = TableId::NULL;
                    return WRC_Abort;
                }
                p.tabm(tab).n_tab_ref += 1;
                if !p.tab(tab).is_virtual() && cannot_be_function(p, tab_list, i) {
                    return WRC_Abort;
                }
                if !p.tab(tab).is_ordinary() {
                    let e_code_orig = w.e_code;
                    if sqlite3_view_get_column_names(p, tab) != 0 {
                        return WRC_Abort;
                    }
                    if p.tab(tab).is_view() {
                        if (p.db.flags & SQLITE_EnableView) == 0 && p.tab(tab).i_db != 1 {
                            let name = p.tab(tab).z_name.clone();
                            p.error_msg(format!("access to view \"{name}\" prohibited"));
                        }
                        let vs = p.tab(tab).p_select;
                        crate::build::sqlite3_src_item_attach_subquery(p, tab_list, i, vs, true);
                    }
                    // nCol = pTab->nCol; pTab->nCol = -1;
                    p.tabm(tab).view_busy = true;
                    w.e_code = 1; // Turn on Select.selId renumbering
                    if p.src(tab_list).a[i].is_subquery {
                        let sub = p.src(tab_list).a[i].p_subq.as_ref().expect("subquery").p_select;
                        sqlite3_walk_select(p, w, sub);
                    }
                    w.e_code = e_code_orig;
                    p.tabm(tab).view_busy = false;
                }
            }
        }

        // Locate the index named by the INDEXED BY clause, if any.
        if p.src(tab_list).a[i].is_indexed_by && sqlite3_indexed_by_lookup(p, tab_list, i) != 0 {
            return WRC_Abort;
        }
    }

    // Process NATURAL keywords, and ON and USING clauses of joins.
    if p.n_err != 0 || sqlite3_process_join(p, sel) != 0 {
        return WRC_Abort;
    }

    // For every "*" that occurs in the column list, insert the names of all columns in all
    // tables. And for every TABLE.* insert the names of all columns in TABLE. The parser
    // inserted a special expression with the TK_ASTERISK operator for each "*" that it found
    // in the column list. The following code just has to locate the TK_ASTERISK expressions
    // and expand each one to the list of all columns in all tables.
    //
    // The first loop just checks to see if there are any "*" operators that need expanding.
    let n = p.l(e_list).a.len();
    let mut k = 0;
    while k < n {
        let pe = p.e(p.l(e_list).a[k].p_expr);
        if pe.op == TK_ASTERISK {
            break;
        }
        if pe.op == TK_DOT && p.e(pe.p_right).op == TK_ASTERISK {
            break;
        }
        elist_flags |= pe.flags;
        k += 1;
    }
    if k < n {
        // If we get here it means the result set contains one or more "*" operators that
        // need to be expanded. Loop through each expression in the result set and expand
        // them one by one.
        let old = p.l(e_list).a.clone();
        let mut new_list = ExprListId::NULL;
        let flags = p.db.flags;
        let long_names = (flags & SQLITE_FullColNames) != 0 && (flags & SQLITE_ShortColNames) == 0;
        for a in &old {
            let pe = a.p_expr;
            elist_flags |= p.e(pe).flags;
            let right = p.e(pe).p_right;
            if p.e(pe).op != TK_ASTERISK && (p.e(pe).op != TK_DOT || p.e(right).op != TK_ASTERISK) {
                // This particular expression does not need to be expanded.
                new_list = sqlite3_expr_list_append(p, new_list, pe);
                let it = p.lm(new_list).a.last_mut().expect("item");
                it.z_e_name = a.z_e_name.clone();
                it.e_e_name = a.e_e_name;
            } else {
                // This expression is a "*" or a "TABLE.*" and needs to be expanded.
                let mut table_seen = false; // Set to 1 when TABLE matches
                let (z_t_name, i_err_ofst) = if p.e(pe).op == TK_DOT {
                    (Some(p.e(p.e(pe).p_left).tok().to_owned()), p.e(p.e(pe).p_right).w)
                } else {
                    (None, p.e(pe).w)
                };
                let n_src = p.src(tab_list).a.len();
                for i in 0..n_src {
                    let (tab, alias, is_nested_from, is_using, p_using, jointype) = {
                        let it = &p.src(tab_list).a[i];
                        (it.p_s_tab, it.z_alias.clone(), it.is_nested_from, it.is_using, it.p_using, it.jointype as i32)
                    };
                    let z_tab_name = alias.unwrap_or_else(|| p.tab(tab).z_name.clone()); // AS name for this data source
                    let mut z_schema_name: Option<String> = None; // Schema name for this data source
                    let nested_from: ExprListId; // Result-set of a nested FROM clause
                    if is_nested_from {
                        let sub = p.src(tab_list).a[i].p_subq.as_ref().expect("subquery").p_select;
                        nested_from = p.s(sub).p_e_list;
                    } else {
                        if let Some(zt) = z_t_name.as_deref()
                            && !zt.eq_ignore_ascii_case(&z_tab_name)
                        {
                            continue;
                        }
                        nested_from = ExprListId::NULL;
                        let i_db = p.tab(tab).i_db;
                        z_schema_name = Some(if i_db >= 0 { p.db.a_db[i_db as usize].z_db_s_name.clone() } else { "*".to_owned() });
                    }
                    let using: IdListId; // USING clause for pFrom[1]
                    if i + 1 < n_src && p.src(tab_list).a[i + 1].is_using && (sel_flags & SF_NestedFrom) != 0 {
                        using = p.src(tab_list).a[i + 1].p_using;
                        for ii in 0..p.a_idlist[using.ix()].a.len() {
                            let z_u_name = p.a_idlist[using.ix()].a[ii].clone();
                            let r = sqlite3_expr(p, TK_ID, Some(&z_u_name));
                            sqlite3_expr_set_error_offset(p, r, i_err_ofst);
                            new_list = sqlite3_expr_list_append(p, new_list, r);
                            let x = p.lm(new_list).a.last_mut().expect("item");
                            x.z_e_name = Some(format!("..{z_u_name}"));
                            x.e_e_name = ENAME_TAB as u8;
                            x.b_using_term = true;
                        }
                    } else {
                        using = IdListId::NULL;
                    }

                    let n_col = p.tab(tab).a_col.len();
                    let mut n_add = n_col; // Number of cols including rowid
                    if p.tab(tab).visible_rowid() && (sel_flags & SF_NestedFrom) != 0 {
                        n_add += 1;
                    }
                    for j in 0..n_add {
                        let z_name: String;
                        if j == n_col {
                            match sqlite3_rowid_alias(p, tab) {
                                Some(z) => z_name = z.to_owned(),
                                None => continue,
                            }
                        } else {
                            z_name = p.tab(tab).a_col[j].z_cn_name.clone();

                            // If pTab is actually an SF_NestedFrom sub-select, do not expand
                            // any ENAME_ROWID columns.
                            if nested_from.nn() && p.l(nested_from).a[j].e_e_name as i32 == ENAME_ROWID {
                                continue;
                            }
                            if let Some(zt) = z_t_name.as_deref()
                                && nested_from.nn()
                                && !sqlite3_match_e_name(p, nested_from, j, None, Some(zt), None, None)
                            {
                                continue;
                            }

                            // If a column is marked as 'hidden', omit it from the expanded
                            // result-set list unless the SELECT has the SF_IncludeHidden bit
                            // set.
                            if (p.s(sel).sel_flags & SF_IncludeHidden) == 0
                                && (p.tab(tab).a_col[j].col_flags & COLFLAG_HIDDEN as u16) != 0
                            {
                                continue;
                            }
                            if (p.tab(tab).a_col[j].col_flags & COLFLAG_NOEXPAND as u16) != 0
                                && z_t_name.is_none()
                                && (sel_flags & SF_NestedFrom) == 0
                            {
                                continue;
                            }
                        }
                        table_seen = true;

                        if i > 0
                            && z_t_name.is_none()
                            && (sel_flags & SF_NestedFrom) == 0
                            && is_using
                            && crate::build::sqlite3_id_list_index(p, p_using, &z_name) >= 0
                        {
                            // In a join with a USING clause, omit columns in the using clause
                            // from the table on the right.
                            continue;
                        }
                        let r = sqlite3_expr(p, TK_ID, Some(&z_name));
                        let px;
                        if n_src > 1
                            && ((jointype & JT_LTORJ) == 0
                                || z_t_name.is_some()
                                || (sel_flags & SF_NestedFrom) != 0
                                || !in_any_using_clause(p, &z_name, tab_list, i, (n_src - i - 1) as i32))
                        {
                            let l = sqlite3_expr(p, TK_ID, Some(&z_tab_name));
                            let mut e = sqlite3_p_expr(p, TK_DOT, l, r);
                            if let Some(zs) = z_schema_name.as_deref() {
                                let l2 = sqlite3_expr(p, TK_ID, Some(zs));
                                e = sqlite3_p_expr(p, TK_DOT, l2, e);
                            }
                            px = e;
                        } else {
                            px = r;
                        }
                        sqlite3_expr_set_error_offset(p, px, i_err_ofst);
                        new_list = sqlite3_expr_list_append(p, new_list, px);
                        let nested_name = if nested_from.nn() && j < p.l(nested_from).a.len() {
                            p.l(nested_from).a[j].z_e_name.clone()
                        } else {
                            None
                        };
                        let b_no_expand = (is_using && crate::build::sqlite3_id_list_index(p, p_using, &z_name) >= 0)
                            || (using.nn() && crate::build::sqlite3_id_list_index(p, using, &z_name) >= 0)
                            || (j < n_col && (p.tab(tab).a_col[j].col_flags & COLFLAG_NOEXPAND as u16) != 0);
                        let x = p.lm(new_list).a.last_mut().expect("item");
                        if (sel_flags & SF_NestedFrom) != 0 {
                            if nested_from.nn() {
                                x.z_e_name = nested_name;
                            } else {
                                x.z_e_name =
                                    Some(format!("{}.{}.{}", z_schema_name.as_deref().unwrap_or(""), z_tab_name, z_name));
                            }
                            x.e_e_name = if j == n_col { ENAME_ROWID as u8 } else { ENAME_TAB as u8 };
                            if b_no_expand {
                                x.b_no_expand = true;
                            }
                        } else if long_names {
                            x.z_e_name = Some(format!("{z_tab_name}.{z_name}"));
                            x.e_e_name = ENAME_NAME as u8;
                        } else {
                            x.z_e_name = Some(z_name);
                            x.e_e_name = ENAME_NAME as u8;
                        }
                    }
                }
                if !table_seen {
                    if let Some(zt) = z_t_name {
                        p.error_msg(format!("no such table: {zt}"));
                    } else {
                        p.error_msg("no tables specified");
                    }
                }
            }
        }
        p.sm(sel).p_e_list = new_list;
    }
    let e_list = p.s(sel).p_e_list;
    if e_list.nn() {
        if p.l(e_list).n_expr() > p.db.a_limit[SQLITE_LIMIT_COLUMN as usize] {
            p.error_msg("too many columns in result set");
            return WRC_Abort;
        }
        if (elist_flags & (EP_HasFunc | EP_Subquery)) != 0 {
            p.sm(sel).sel_flags |= SF_ComplexResult;
        }
    }
    WRC_Continue
}

/// `sqlite3SrcListAssignCursors(pParse, pList)`: assign VdbeCursor index numbers to all
/// tables in a SrcList.
pub fn sqlite3_src_list_assign_cursors(p: &mut Parse, list: SrcListId) {
    if list.is_null() {
        return;
    }
    for i in 0..p.src(list).a.len() {
        if p.src(list).a[i].i_cursor >= 0 {
            continue;
        }
        let c = p.n_tab;
        p.n_tab += 1;
        p.srcm(list).a[i].i_cursor = c;
        let sub = {
            let it = &p.src(list).a[i];
            if it.is_subquery { it.p_subq.as_ref().map(|q| q.p_select) } else { None }
        };
        if let Some(sub) = sub {
            let s = p.s(sub).p_src;
            sqlite3_src_list_assign_cursors(p, s);
        }
    }
}

/// `sqlite3ColumnsFromExprList(pParse, pEList, pnCol, paCol)`: given an expression list
/// (which is really the list of expressions that form the result set of a SELECT statement)
/// compute appropriate column names for a table that would hold the expression list.
///
/// All column names will be unique.
///
/// Only the column names are computed. Column.zType, Column.zColl, and Column.affinity are
/// not filled in by this routine.
pub fn sqlite3_columns_from_expr_list(p: &mut Parse, e_list: ExprListId) -> Vec<Column> {
    let mut a_col: Vec<Column> = Vec::new();
    // Hash table of column names (name in lower case -> index of the item in pEList)
    let mut ht: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let n_col = if e_list.nn() { p.l(e_list).a.len().min(32767) } else { 0 };
    let mut i = 0;
    while i < n_col && p.n_err == 0 {
        let (z_e_name, e_e_name, pe, b_no_expand) = {
            let it = &p.l(e_list).a[i];
            (it.z_e_name.clone(), it.e_e_name, it.p_expr, it.b_no_expand)
        };
        let mut col = Column::default();
        // Get an appropriate name for the column
        let mut z_name: Option<String> = z_e_name;
        if z_name.is_some() && e_e_name as i32 == ENAME_NAME {
            // If the column contains an "AS <name>" phrase, use <name> as the name
        } else {
            let mut c = sqlite3_expr_skip_collate_and_likely(p, pe);
            while c.nn() && p.e(c).op == TK_DOT {
                c = p.e(c).p_right;
            }
            if c.nn() && p.e(c).op == TK_COLUMN && p.e(c).y_tab.nn() {
                // For columns use the column name name
                let mut i_col = p.e(c).i_column as i32;
                let t = p.tab(p.e(c).y_tab);
                if i_col < 0 {
                    i_col = t.i_p_key as i32;
                }
                z_name = Some(if i_col >= 0 { t.a_col[i_col as usize].z_cn_name.clone() } else { "rowid".to_owned() });
            } else if c.nn() && p.e(c).op == TK_ID {
                z_name = Some(p.e(c).tok().to_owned());
            } else {
                // Use the original text of the column expression as its name
            }
        }
        let mut z_name = match z_name {
            Some(z) if sqlite3_is_true_or_false(&z) == 0 => z,
            _ => format!("column{}", i + 1),
        };

        // Make sure the column name is unique. If the name is not unique, append an integer
        // to the name so that it becomes unique.
        let mut cnt: u32 = 0;
        while let Some(&collide) = ht.get(&z_name.to_ascii_lowercase()) {
            if p.l(e_list).a[collide].b_using_term {
                col.col_flags |= COLFLAG_NOEXPAND as u16;
            }
            let b = z_name.as_bytes();
            let mut n_name = b.len();
            if n_name > 0 {
                let mut j = n_name - 1;
                while j > 0 && b[j].is_ascii_digit() {
                    j -= 1;
                }
                if b[j] == b':' {
                    n_name = j;
                }
            }
            cnt += 1;
            z_name = format!("{}:{}", &z_name[..n_name], cnt);
            // (if( cnt>3 ) sqlite3_randomness(sizeof(cnt), &cnt): C switches to random
            // suffixes after the third collision; such a name cannot be referred to.)
        }
        col.z_cn_name = z_name.clone();
        if b_no_expand {
            col.col_flags |= COLFLAG_NOEXPAND as u16;
        }
        ht.insert(z_name.to_ascii_lowercase(), i);
        a_col.push(col);
        i += 1;
    }
    if p.n_err != 0 {
        a_col.clear();
    }
    a_col
}

/// `columnTypeImpl(pNC, pExpr)`: return a pointer to a string containing the 'declaration
/// type' of the expression pExpr. The string may be treated as static by the caller.
///
/// The declaration type is the exact datatype definition extracted from the original CREATE
/// TABLE statement if the expression is a column. The declaration type for a ROWID field is
/// INTEGER. Exactly when an expression is considered a column can be complex in the presence
/// of subqueries. The result-set expression in all of the following SELECT statements is
/// considered a column by this function.
///
///   SELECT col FROM tbl;
///   SELECT (SELECT col FROM tbl;
///   SELECT (SELECT col FROM tbl);
///   SELECT abc FROM (SELECT col AS abc FROM tbl);
///
/// The declaration type for any expression other than a column is NULL.
///
/// `nc` is the chain of FROM clauses (C's NameContext list `pNC`, innermost first).
fn column_type(p: &Parse, nc: &[SrcListId], e: ExprId) -> Option<String> {
    let x = p.e(e);
    match x.op {
        TK_COLUMN => {
            // The expression is a column. Locate the table the column is being extracted
            // from in NameContext.pSrcList. This table may be real database table or a
            // subquery.
            let i_col = x.i_column as i32;
            let mut k = 0;
            let mut found: Option<(TableId, Option<SelectId>, usize)> = None;
            while k < nc.len() && found.is_none() {
                let tl = p.src(nc[k]);
                if let Some(it) = tl.a.iter().find(|it| it.i_cursor == x.i_table) {
                    let s = if it.is_subquery { it.p_subq.as_ref().map(|q| q.p_select) } else { None };
                    found = Some((it.p_s_tab, s, k));
                } else {
                    k += 1;
                }
            }
            let (tab, sub, k) = found?;
            if tab.is_null() {
                // At one time, code such as "SELECT new.x" within a trigger would cause this
                // condition to run. ... In this case, set the column type to NULL, even
                // though it should really be "INTEGER".
                return None;
            }
            if let Some(s) = sub {
                // The "table" is actually a sub-select or a view in the FROM clause of the
                // SELECT statement. Return the declaration type and origin data for the
                // result-set column of the sub-select.
                if i_col >= 0 && i_col < p.l(p.s(s).p_e_list).n_expr() {
                    // If iCol is less than zero, then the expression requests the rowid of
                    // the sub-select or view. This expression is legal (see test case
                    // misc2.2.2) - it always evaluates to NULL.
                    let sub_e = p.l(p.s(s).p_e_list).a[i_col as usize].p_expr;
                    let mut chain = vec![p.s(s).p_src];
                    chain.extend_from_slice(&nc[k..]);
                    return column_type(p, &chain, sub_e);
                }
                None
            } else {
                // A real table or a CTE table
                if i_col < 0 {
                    Some("INTEGER".to_owned())
                } else {
                    crate::schema::sqlite3_column_type(&p.tab(tab).a_col[i_col as usize], None).map(|s| s.to_owned())
                }
            }
        }
        TK_SELECT => {
            // The expression is a sub-select. Return the declaration type and origin info for
            // the single column in the result set of the SELECT statement.
            let s = x.x_select();
            let sub_e = p.l(p.s(s).p_e_list).a[0].p_expr;
            let mut chain = vec![p.s(s).p_src];
            chain.extend_from_slice(nc);
            column_type(p, &chain, sub_e)
        }
        _ => None,
    }
}

/// `sqlite3SubqueryColumnTypes(pParse, pTab, pSelect, aff)`: pTab is a transient Table object
/// that represents a subquery of some kind (maybe a parenthesized subquery in the FROM
/// clause of a SELECT statement, or a CTE table, or a view, or something else). Add type
/// information to that Table object.
///
/// The column names are already set: this routine only fills in affinity, the declared type
/// and the collating sequence. The aff parameter is the default affinity for columns whose
/// affinity cannot be determined (NONE for a subquery, BLOB for a CREATE TABLE AS).
///
/// A column affinity is taken from the first arm of a compound that gives it one; a
/// compound whose arms disagree in a way that could change comparisons gets BLOB
/// (no affinity).
pub fn sqlite3_subquery_column_types(p: &mut Parse, tab: TableId, select: SelectId, aff: u8) {
    if p.in_rename_object() {
        return;
    }
    let mut select = select;
    while p.s(select).p_prior.nn() {
        select = p.s(select).p_prior;
    }
    let el = p.s(select).p_e_list;
    let nc = [p.s(select).p_src];
    let n_col = p.tab(tab).a_col.len();
    for i in 0..n_col {
        let mut m = 0;
        let mut s2 = select;
        let noins = p.tab(tab).a_col[i].col_flags as u32 & COLFLAG_NOINSERT as u32;
        p.tabm(tab).tab_flags |= noins;
        let e = p.l(el).a[i].p_expr;
        // pCol->szEst = ... // Column size est for SELECT tables never used
        let mut affinity = sqlite3_expr_affinity(p, e);
        while affinity <= SQLITE_AFF_NONE && p.s(s2).p_next.nn() {
            m |= sqlite3_expr_data_type(p, p.l(p.s(s2).p_e_list).a[i].p_expr);
            s2 = p.s(s2).p_next;
            affinity = sqlite3_expr_affinity(p, p.l(p.s(s2).p_e_list).a[i].p_expr);
        }
        if affinity <= SQLITE_AFF_NONE {
            affinity = aff;
        }
        if affinity >= SQLITE_AFF_TEXT && (p.s(s2).p_next.nn() || s2 != select) {
            s2 = p.s(s2).p_next;
            while s2.nn() {
                m |= sqlite3_expr_data_type(p, p.l(p.s(s2).p_e_list).a[i].p_expr);
                s2 = p.s(s2).p_next;
            }
            if affinity == SQLITE_AFF_TEXT && (m & 0x01) != 0 {
                affinity = SQLITE_AFF_BLOB;
            } else if affinity >= SQLITE_AFF_NUMERIC && (m & 0x02) != 0 {
                affinity = SQLITE_AFF_BLOB;
            }
            if affinity >= SQLITE_AFF_NUMERIC && p.e(e).op == TK_CAST {
                affinity = SQLITE_AFF_FLEXNUM;
            }
        }
        let mut z_type = column_type(p, &nc, e);
        if z_type.as_deref().is_none_or(|z| affinity != crate::util::sqlite3_affinity_type(z)) {
            if affinity == SQLITE_AFF_NUMERIC || affinity == SQLITE_AFF_FLEXNUM {
                z_type = Some("NUM".to_owned());
            } else {
                // for(j=1; j<SQLITE_N_STDTYPE; j++) if( sqlite3StdTypeAffinity[j]==pCol->affinity )
                z_type = match affinity {
                    SQLITE_AFF_BLOB => Some("BLOB".to_owned()),
                    SQLITE_AFF_INTEGER => Some("INT".to_owned()),
                    SQLITE_AFF_REAL => Some("REAL".to_owned()),
                    SQLITE_AFF_TEXT => Some("TEXT".to_owned()),
                    _ => None,
                };
            }
        }
        let coll = sqlite3_expr_coll_seq(p, e);
        let col = &mut p.tabm(tab).a_col[i];
        col.affinity = affinity;
        if let Some(z) = z_type {
            col.col_flags &= !((COLFLAG_HASTYPE | COLFLAG_HASCOLL) as u16);
            col.z_type = Some(z);
            col.z_coll = None;
            col.col_flags |= COLFLAG_HASTYPE as u16;
        }
        if let Some(c) = coll {
            col.z_coll = Some(c.name.clone());
            col.col_flags |= COLFLAG_HASCOLL as u16;
        }
    }
    p.tabm(tab).sz_tab_row = 1; // Any non-zero value works
}

/// `checkSubqueryNestingDepth(pParse)`: verify that the nesting of subqueries and views is
/// below the SQLITE_LIMIT_EXPR_DEPTH limit.
fn check_subquery_nesting_depth(p: &mut Parse) -> bool {
    if p.n_nest_sel >= p.db.a_limit[SQLITE_LIMIT_EXPR_DEPTH as usize] {
        p.error_msg("VIEWs and/or subqueries nested too deep");
        return true;
    }
    false
}

/// `sqlite3ResultSetOfSelect(pParse, pSelect, aff)`: given a SELECT statement, generate a
/// Table structure that describes the result set of that SELECT.
pub fn sqlite3_result_set_of_select(p: &mut Parse, select: SelectId, aff: u8) -> TableId {
    p.n_nest_sel += 1;
    if check_subquery_nesting_depth(p) {
        return TableId::NULL;
    }
    let saved_flags = p.db.flags;
    p.db.flags &= !SQLITE_FullColNames;
    p.db.flags |= SQLITE_ShortColNames;
    sqlite3_select_prep(p, select, NcId::NULL);
    p.db.flags = saved_flags;
    if p.n_err != 0 {
        return TableId::NULL;
    }
    let mut select = select;
    while p.s(select).p_prior.nn() {
        select = p.s(select).p_prior;
    }
    let el = p.s(select).p_e_list;
    let a_col = sqlite3_columns_from_expr_list(p, el);
    let tab = p.new_tab(Table { n_tab_ref: 1, n_row_log_est: 200, a_col, i_p_key: -1, i_db: -1, ..Table::default() });
    sqlite3_subquery_column_types(p, tab, select, aff);
    p.n_nest_sel -= 1;
    tab
}

/// `sqlite3ViewGetColumnNames(pParse, pTable)` / `viewGetColumnNames()` (build.c): the Table
/// structure pTable is really a VIEW. Fill in the names of the columns of the view in the
/// pTable structure. Return non-zero if there are errors. If an error is seen an error
/// message is left in pParse->zErrMsg.
pub fn sqlite3_view_get_column_names(p: &mut Parse, tab: TableId) -> i32 {
    // A positive nCol means the columns names for this view are already known.
    if !p.tab(tab).is_virtual() && !p.tab(tab).a_col.is_empty() && !p.tab(tab).view_busy {
        return 0;
    }
    let mut n_err = 0;
    if p.tab(tab).is_virtual() {
        p.not_supported("virtual table");
        return 1;
    }

    // A negative nCol is a special marker meaning that we are currently trying to compute
    // the column names. If we enter this routine with a negative nCol, it means two or more
    // views form a loop, like this:
    //
    //     CREATE VIEW one AS SELECT * FROM two;
    //     CREATE VIEW two AS SELECT * FROM one;
    //
    // Actually, the error above is now caught prior to reaching this point. But the
    // following test is still important as it does come up in the following:
    //
    //     CREATE TABLE main.ex1(a);
    //     CREATE TEMP VIEW ex1 AS SELECT a FROM ex1;
    //     SELECT * FROM temp.ex1;
    if p.tab(tab).view_busy {
        let name = p.tab(tab).z_name.clone();
        p.error_msg(format!("view {name} is circularly defined"));
        return 1;
    }

    // If we get this far, it means we need to compute the table names. Note that the call to
    // sqlite3ResultSetOfSelect() will expand any "*" elements in the results set of the
    // view and will assign cursors to the elements of the FROM clause. But we do not want
    // these changes to be permanent. So the computation is done on a copy of the SELECT
    // statement that defines the view.
    let vs = p.tab(tab).p_select;
    let sel = sqlite3_select_dup(p, vs, 0);
    if sel.nn() {
        let e_parse_mode = p.e_parse_mode;
        let n_tab = p.n_tab;
        let n_select = p.n_select;
        p.e_parse_mode = PARSE_MODE_NORMAL as u8;
        let ssrc = p.s(sel).p_src;
        sqlite3_src_list_assign_cursors(p, ssrc);
        p.tabm(tab).view_busy = true;
        let sel_tab = sqlite3_result_set_of_select(p, sel, SQLITE_AFF_NONE);
        p.n_tab = n_tab;
        p.n_select = n_select;
        if sel_tab.is_null() {
            p.tabm(tab).a_col.clear();
            n_err += 1;
        } else if p.tab(tab).p_check.nn() {
            // CREATE VIEW name(arglist) AS ... The names of the columns in the table are
            // taken from arglist which is stored in pTable->pCheck. The pCheck field
            // normally holds CHECK constraints on an ordinary table, but for a VIEW it holds
            // the list of column names.
            let check = p.tab(tab).p_check;
            let cols = sqlite3_columns_from_expr_list(p, check);
            p.tabm(tab).a_col = cols;
            if p.n_err == 0 && p.tab(tab).a_col.len() == p.l(p.s(sel).p_e_list).a.len() {
                sqlite3_subquery_column_types(p, tab, sel, SQLITE_AFF_NONE);
            }
        } else {
            // CREATE VIEW name AS... without an argument list. Construct the column names
            // from the select statement that defines the view.
            let cols = p.tab(sel_tab).a_col.clone();
            let f = p.tab(sel_tab).tab_flags & COLFLAG_NOINSERT as u32;
            let t = p.tabm(tab);
            t.a_col = cols;
            t.tab_flags |= f;
        }
        let n = p.tab(tab).a_col.len() as i16;
        let t = p.tabm(tab);
        t.n_nv_col = n;
        t.view_busy = false;
        // sqlite3DeleteTable(db, pSelTab); sqlite3SelectDelete(db, pSel): the tables the copy
        // of the view's SELECT referred to are released (Table.nTabRef--).
        fn release_cb(p: &mut Parse, _w: &mut Walker, s: SelectId) -> i32 {
            let src = p.s(s).p_src;
            if src.nn() {
                for i in 0..p.src(src).a.len() {
                    let t = p.src(src).a[i].p_s_tab;
                    if t.nn() && p.tab(t).n_tab_ref > 0 {
                        p.tabm(t).n_tab_ref -= 1;
                    }
                }
            }
            WRC_Continue
        }
        let mut w = Walker {
            x_expr_callback: Some(sqlite3_expr_walk_noop),
            x_select_callback: Some(release_cb),
            ..Walker::default()
        };
        sqlite3_walk_select(p, &mut w, sel);
        p.e_parse_mode = e_parse_mode;
    } else {
        n_err += 1;
    }
    n_err + p.n_err
}

/// `sqlite3GenerateColumnNames(pParse, pSelect)`: compute the column names for a SELECT
/// statement.
///
/// The only guarantee that SQLite makes about column names is that if the column has an AS
/// clause assigning it a name, that will be the name used. That is the only documented
/// guarantee. However, countless applications developed over the years have made baseless
/// assumptions about column names and will break if those assumptions changes. Hence, use
/// extreme caution when modifying this routine to avoid breaking legacy.
///
/// See Also: sqlite3ColumnsFromExprList()
///
/// The PRAGMA short_column_names and PRAGMA full_column_names settings are deprecated. The
/// default setting is short=ON, full=OFF. Applications that depend on any other settings
/// should use caution...
///
/// short=OFF, full=OFF:      Column name is the text of the expression has it was originally
///                           typed into the SQL statement. That means that TABLE.COLUMN if
///                           written that way, or just COLUMN if written that way.
///
/// short=ON, full=OFF:       (This is the default setting). If the result refers directly to
///                           a table column, then the result column name is just the table
///                           column name: COLUMN. Otherwise use the text of the expression
///                           as originally typed.
///
/// short=any, full=ON:       If the result refers directly to a table column, then the
///                           result column name with the table name prefix, ex:
///                           TABLE.COLUMN. Otherwise use the original text.
pub fn sqlite3_generate_column_names(p: &mut Parse, select: SelectId) {
    if p.col_names_set {
        return;
    }
    // Column names are determined by the left-most term of a compound select
    let mut select = select;
    while p.s(select).p_prior.nn() {
        select = p.s(select).p_prior;
    }
    let tab_list = p.s(select).p_src;
    let e_list = p.s(select).p_e_list;
    p.col_names_set = true;
    let full_name = (p.db.flags & SQLITE_FullColNames) != 0;
    let src_name = (p.db.flags & SQLITE_ShortColNames) != 0 || full_name;
    let n = p.l(e_list).n_expr();
    p.v.set_num_cols(n);
    for i in 0..n as usize {
        let (e, z_e_name, e_e_name) = {
            let it = &p.l(e_list).a[i];
            (it.p_expr, it.z_e_name.clone(), it.e_e_name)
        };
        let name: String;
        if let Some(z) = z_e_name.as_deref()
            && e_e_name as i32 == ENAME_NAME
        {
            // An AS clause always takes first priority
            name = z.to_owned();
        } else if src_name && p.e(e).op == TK_COLUMN {
            let tab = p.tab(p.e(e).y_tab);
            let mut i_col = p.e(e).i_column as i32;
            if i_col < 0 {
                i_col = tab.i_p_key as i32;
            }
            let z_col = if i_col < 0 { "rowid".to_owned() } else { tab.a_col[i_col as usize].z_cn_name.clone() };
            name = if full_name { format!("{}.{}", tab.z_name, z_col) } else { z_col };
        } else {
            name = match z_e_name {
                None => format!("column{}", i + 1),
                Some(z) => z,
            };
        }
        p.v.set_col_name(i as i32, sqliters_vdbe::vdbeaux::COLNAME_NAME, Some(name.as_str()));
    }
    // generateColumnTypes(pParse, pTabList, pEList)
    for i in 0..n as usize {
        let e = p.l(e_list).a[i].p_expr;
        let z_type = column_type(p, &[tab_list], e);
        p.v.set_col_name(i as i32, sqliters_vdbe::vdbeaux::COLNAME_DECLTYPE, z_type.as_deref());
    }
}

/// `sqlite3SelectExpand(pParse, pSelect)`: this routine "expands" a SELECT statement and all
/// of its subqueries. For additional information on what it means to "expand" a SELECT
/// statement, see the comment on the selectExpand worker callback above.
///
/// Expanding a SELECT statement is the first step in processing a SELECT statement. The
/// SELECT statement must be expanded before name resolution is performed.
///
/// If anything goes wrong, an error message is written into the Parse. The calling function
/// can detect the problem by looking at pParse->nErr and/or pParse->db->mallocFailed.
fn sqlite3_select_expand(p: &mut Parse, sel: SelectId) {
    let mut w = Walker { x_expr_callback: Some(sqlite3_expr_walk_noop), ..Walker::default() };
    if p.has_compound {
        w.x_select_callback = Some(convert_compound_select_to_subquery);
        w.x_select_callback2 = None;
        sqlite3_walk_select(p, &mut w, sel);
    }
    w.x_select_callback = Some(select_expander);
    w.x_select_callback2 = Some(sqlite3_select_pop_with);
    w.e_code = 0;
    sqlite3_walk_select(p, &mut w, sel);
}

/// `selectAddSubqueryTypeInfo(pWalker, p)`: this is a Walker.xSelectCallback callback for
/// the sqlite3SelectTypeInfo() interface.
///
/// For each FROM-clause subquery, add Column.zType, Column.zColl, and Column.affinity
/// information to the Table structure that represents the result set of that subquery.
///
/// The Table structure that represents the result set was constructed by selectExpander()
/// but the type and collation and affinity information was omitted at that point because
/// identifiers had not yet been resolved. This routine is called after identifier
/// resolution.
fn select_add_subquery_type_info(p: &mut Parse, _w: &mut Walker, sel: SelectId) {
    if p.s(sel).sel_flags & SF_HasTypeInfo != 0 {
        return;
    }
    p.sm(sel).sel_flags |= SF_HasTypeInfo;
    let tab_list = p.s(sel).p_src;
    for i in 0..p.src(tab_list).a.len() {
        let (tab, sub) = {
            let it = &p.src(tab_list).a[i];
            (it.p_s_tab, if it.is_subquery { it.p_subq.as_ref().map(|q| q.p_select) } else { None })
        };
        if (p.tab(tab).tab_flags & TF_Ephemeral) != 0
            && let Some(sub) = sub
        {
            // A sub-query in the FROM clause of a SELECT
            sqlite3_subquery_column_types(p, tab, sub, SQLITE_AFF_NONE);
        }
    }
}

/// `sqlite3SelectAddTypeInfo(pParse, pSelect)`: this routine adds datatype and collating
/// sequence information to the Table structures of all FROM-clause subqueries in a SELECT
/// statement.
///
/// Use this routine after name resolution.
fn sqlite3_select_add_type_info(p: &mut Parse, sel: SelectId) {
    let mut w = Walker {
        x_select_callback: Some(sqlite3_select_walk_noop),
        x_select_callback2: Some(select_add_subquery_type_info),
        x_expr_callback: Some(sqlite3_expr_walk_noop),
        ..Walker::default()
    };
    sqlite3_walk_select(p, &mut w, sel);
}

/// `sqlite3SelectPrep(pParse, p, pOuterNC)`: this routine sets up a SELECT statement for
/// processing. The following is accomplished:
///
///     *  VDBE Cursor numbers are assigned to all FROM-clause terms.
///     *  Ephemeral Table objects are created for all FROM-clause subqueries.
///     *  ON and USING clauses are shifted into WHERE statements
///     *  Wildcards "*" and "TABLE.*" in result sets are expanded.
///     *  Identifiers in expression are matched to tables.
///
/// This routine acts recursively on all subqueries within the SELECT.
pub fn sqlite3_select_prep(p: &mut Parse, sel: SelectId, outer_nc: NcId) {
    if p.s(sel).sel_flags & SF_HasTypeInfo != 0 {
        return;
    }
    sqlite3_select_expand(p, sel);
    if p.n_err != 0 {
        return;
    }
    sqlite3_resolve_select_names(p, sel, outer_nc);
    if p.n_err != 0 {
        return;
    }
    sqlite3_select_add_type_info(p, sel);
}

/// `sqlite3SelectWrongNumTermsError(pParse, p)`: error message for when two or more terms of
/// a compound select have different size result sets.
pub fn sqlite3_select_wrong_num_terms_error(p: &mut Parse, sel: SelectId) {
    if p.s(sel).sel_flags & SF_Values != 0 {
        p.error_msg("all VALUES must have the same number of terms");
    } else {
        p.error_msg(format!(
            "SELECTs to the left and right of {} do not have the same number of result columns",
            sqlite3_select_op_name(p.s(sel).op)
        ));
    }
}

/// `codeOffset(v, iOffset, iContinue)`: add code to implement the OFFSET
fn code_offset(p: &mut Parse, i_offset: i32, i_continue: i32) {
    if i_offset > 0 {
        p.v.add_op3(OP_IfPos, i_offset, i_continue, 1);
    }
}

/// `codeDistinct(pParse, eTnctType, iTab, addrRepeat, pEList, regElem)`: add code that will
/// check to make sure the array of registers starting at `reg_elem` has never been seen
/// before. If it has, jump to `addr_repeat`.
///
/// Returns the register number of the first of the "previous value" registers for
/// WHERE_DISTINCT_ORDERED, `i_tab` for WHERE_DISTINCT_UNORDERED, 0 for WHERE_DISTINCT_UNIQUE.
fn code_distinct(p: &mut Parse, e_tnct_type: i32, i_tab: i32, addr_repeat: i32, e_list: ExprListId, reg_elem: i32) -> i32 {
    let mut i_ret = 0;
    let n_result_col = p.l(e_list).n_expr();
    match e_tnct_type {
        WHERE_DISTINCT_ORDERED => {
            // Allocate space for the previous row
            let reg_prev = p.n_mem + 1;
            i_ret = reg_prev;
            p.n_mem += n_result_col;
            let i_jump = p.v.current_addr() + n_result_col;
            for i in 0..n_result_col {
                let e = p.l(e_list).a[i as usize].p_expr;
                let coll = sqlite3_expr_coll_seq(p, e);
                if i < n_result_col - 1 {
                    p.v.add_op3(OP_Ne, reg_elem + i, i_jump, reg_prev + i);
                } else {
                    p.v.add_op3(OP_Eq, reg_elem + i, addr_repeat, reg_prev + i);
                }
                if let Some(c) = coll {
                    p.v.change_p4(-1, P4::CollSeq(c));
                }
                p.v.change_p5(SQLITE_NULLEQ as u16);
            }
            p.v.add_op3(OP_Copy, reg_elem, reg_prev, n_result_col - 1);
        }
        WHERE_DISTINCT_UNIQUE => {
            // nothing to do
        }
        _ => {
            let r1 = sqlite3_get_temp_reg(p);
            p.v.add_op4_int(OP_Found, i_tab, addr_repeat, reg_elem, n_result_col);
            p.v.add_op3(OP_MakeRecord, reg_elem, n_result_col, r1);
            p.v.add_op4_int(OP_IdxInsert, i_tab, r1, reg_elem, n_result_col);
            p.v.change_p5(OPFLAG_USESEEKRESULT as u16);
            sqlite3_release_temp_reg(p, r1);
            i_ret = i_tab;
        }
    }
    i_ret
}

/// `fixDistinctOpenEph(pParse, eTnctType, iVal, iOpenEphAddr)`: this routine runs after
/// sqlite3WhereBegin(). It is used to remove an OP_OpenEphemeral opcode that was coded
/// before it was known whether the DISTINCT could be done without the ephemeral table
/// (WHERE_DISTINCT_UNIQUE or WHERE_DISTINCT_ORDERED).
///
/// For WHERE_DISTINCT_ORDERED the opcode becomes an OP_Null that initializes the
/// "previous value" register `i_val`.
fn fix_distinct_open_eph(p: &mut Parse, e_tnct_type: i32, i_val: i32, i_open_eph_addr: i32) {
    if p.n_err == 0 && (e_tnct_type == WHERE_DISTINCT_UNIQUE || e_tnct_type == WHERE_DISTINCT_ORDERED) {
        p.v.change_to_noop(i_open_eph_addr);
        if p.v.get_op(i_open_eph_addr + 1).opcode == OP_Explain {
            p.v.change_to_noop(i_open_eph_addr + 1);
        }
        if e_tnct_type == WHERE_DISTINCT_ORDERED {
            // Change the OP_OpenEphemeral to an OP_Null that sets the MEM_Cleared bit on the
            // first register.
            let op = p.v.get_op_mut(i_open_eph_addr);
            op.opcode = OP_Null;
            op.p1 = 1;
            op.p2 = i_val;
        }
    }
}

/// `struct RowLoadInfo`: an instance of this object holds information (beyond pParse and
/// pSelect) needed to load the next result row that is to be added to the sorter.
#[derive(Debug, Clone, Copy, Default)]
pub struct RowLoadInfo {
    /// Store results in array of registers here
    pub reg_result: i32,
    /// Flag argument to ExprCodeExprList()
    pub ecel_flags: u8,
}

/// `innerLoopLoadRow(pParse, pSelect, pInfo)`: this routine does the work of loading query
/// data into an array of registers so that it can be added to the sorter.
fn inner_loop_load_row(p: &mut Parse, sel: SelectId, info: &RowLoadInfo) {
    let el = p.s(sel).p_e_list;
    sqlite3_expr_code_expr_list(p, el, info.reg_result, 0, info.ecel_flags);
}

/// `makeSorterRecord(pParse, pSort, pSelect, regBase, nBase)`: code the OP_MakeRecord
/// instruction that generates the entry to be added into the sorter.
///
/// Return the register in which the result is stored.
fn make_sorter_record(p: &mut Parse, sort: &SortCtx, sel: SelectId, reg_base: i32, n_base: i32) -> i32 {
    let n_ob_sat = sort.n_ob_sat;
    p.n_mem += 1;
    let reg_out = p.n_mem;
    if let Some(info) = sort.p_deferred_row_load {
        inner_loop_load_row(p, sel, &info);
    }
    p.v.add_op3(OP_MakeRecord, reg_base + n_ob_sat, n_base - n_ob_sat, reg_out);
    reg_out
}

/// `pushOntoSorter(pParse, pSort, pSelect, regData, regOrigData, nData, nPrefixReg)`:
/// generate code that will push the record in registers regData through regData+nData-1
/// onto the sorter.
#[allow(clippy::too_many_arguments)]
fn push_onto_sorter(p: &mut Parse, sort: &mut SortCtx, sel: SelectId, reg_data: i32, reg_orig_data: i32, n_data: i32, n_prefix_reg: i32) {
    let b_seq = ((sort.sort_flags & SORTFLAG_USE_SORTER) == 0) as i32;
    let n_expr = p.l(sort.p_order_by).n_expr(); // No. of ORDER BY terms
    let n_base = n_expr + b_seq + n_data; // Fields in sorter record
    let reg_base; // Regs for sorter record
    let mut reg_record = 0; // Assembled sorter record
    let n_ob_sat = sort.n_ob_sat; // ORDER BY terms to skip
    let mut i_skip = 0; // End of the sorter insert loop

    // Three cases:
    //   (1) The data to be sorted has already been packed into a Record by a prior
    //       OP_MakeRecord. In this case nData==1 and regData will be completely unrelated
    //       to regOrigData.
    //   (2) All output columns are included in the sort record. In that case
    //       regData==regOrigData.
    //   (3) Some output columns are omitted from the sort record due to the
    //       SQLITE_ENABLE_SORTER_REFERENCES optimization, or due to the SQLITE_ECEL_OMITREF
    //       optimization, or due to the SortCtx.pDeferredRowLoad optimization. In any of
    //       these cases regOrigData is 0 to prevent this routine from trying to copy values
    //       that might not yet exist.
    if n_prefix_reg != 0 {
        reg_base = reg_data - n_prefix_reg;
    } else {
        reg_base = p.n_mem + 1;
        p.n_mem += n_base;
    }
    let i_limit = if p.s(sel).i_offset != 0 { p.s(sel).i_offset + 1 } else { p.s(sel).i_limit }; // LIMIT counter
    sort.label_done = p.v.make_label();
    let ob = sort.p_order_by;
    sqlite3_expr_code_expr_list(
        p,
        ob,
        reg_base,
        reg_orig_data,
        (SQLITE_ECEL_DUP | if reg_orig_data != 0 { SQLITE_ECEL_REF } else { 0 }) as u8,
    );
    if b_seq != 0 {
        p.v.add_op2(OP_Sequence, sort.i_e_cursor, reg_base + n_expr);
    }
    if n_prefix_reg == 0 && n_data > 0 {
        sqlite3_expr_code_move(p, reg_data, reg_base + n_expr + b_seq, n_data);
    }
    if n_ob_sat > 0 {
        reg_record = make_sorter_record(p, sort, sel, reg_base, n_base);
        let reg_prev_key = p.n_mem + 1; // The first nOBSat columns of the previous row
        p.n_mem += sort.n_ob_sat;
        let n_key = n_expr - sort.n_ob_sat + b_seq; // Number of sorting key columns, including OP_Sequence
        let addr_first = if b_seq != 0 {
            p.v.add_op1(OP_IfNot, reg_base + n_expr)
        } else {
            p.v.add_op1(OP_SequenceTest, sort.i_e_cursor)
        };
        p.v.add_op3(OP_Compare, reg_prev_key, reg_base, sort.n_ob_sat);
        // pOp = sqlite3VdbeGetOp(v, pSort->addrSortIndex): the opcode that opens the sorter
        let ki_old = {
            let op = p.v.get_op_mut(sort.addr_sort_index);
            op.p2 = n_key + n_data;
            match &op.p4 {
                P4::KeyInfo(k) => (**k).clone(),
                _ => unreachable!("sorter without a KeyInfo"),
            }
        };
        // memset(pKI->aSortFlags, 0, pKI->nKeyField); /* Makes OP_Jump testable */
        let mut ki = ki_old.clone();
        for f in ki.a_sort_flags.iter_mut().take(ki.n_key_field as usize) {
            *f = 0;
        }
        p.v.change_p4(-1, P4::KeyInfo(Arc::new(ki)));
        let new_ki = sqlite3_key_info_from_expr_list(p, ob, n_ob_sat, ki_old.n_all_field as i32 - ki_old.n_key_field as i32 - 1);
        p.v.get_op_mut(sort.addr_sort_index).p4 = P4::KeyInfo(Arc::new(new_ki));
        let addr_jmp = p.v.current_addr(); // Address of the OP_Jump opcode
        p.v.add_op3(OP_Jump, addr_jmp + 1, 0, addr_jmp + 1);
        sort.label_bk_out = p.v.make_label();
        p.n_mem += 1;
        sort.reg_return = p.n_mem;
        p.v.add_op2(OP_Gosub, sort.reg_return, sort.label_bk_out);
        p.v.add_op1(OP_ResetSorter, sort.i_e_cursor);
        if i_limit != 0 {
            p.v.add_op2(OP_IfNot, i_limit, sort.label_done);
        }
        p.v.jump_here(addr_first);
        sqlite3_expr_code_move(p, reg_base, reg_prev_key, sort.n_ob_sat);
        p.v.jump_here(addr_jmp);
    }
    if i_limit != 0 {
        // At this point the values for the new sorter entry are stored in an array of
        // registers. They need to be composed into a record and inserted into the sorter if
        // either (a) there are currently less than LIMIT+OFFSET items or (b) the new record
        // is smaller than the largest record currently in the sorter. If (b) is true and
        // there are already LIMIT+OFFSET items in the sorter, delete the largest entry
        // before inserting the new one. This way there are never more than LIMIT+OFFSET
        // items in the sorter.
        //
        // If the new record does not need to be inserted into the sorter, jump to the next
        // iteration of the loop. If the pSort->labelOBLopt value is not zero, then it is a
        // label of where to jump. Otherwise, just bypass the row insert logic. See the
        // header comment on the sqlite3WhereOrderByLimitOptLabel() function for additional
        // info.
        let i_csr = sort.i_e_cursor;
        let a = p.v.current_addr() + 4;
        p.v.add_op2(OP_IfNotZero, i_limit, a);
        p.v.add_op2(OP_Last, i_csr, 0);
        i_skip = p.v.add_op4_int(OP_IdxLE, i_csr, 0, reg_base + n_ob_sat, n_expr - n_ob_sat);
        p.v.add_op1(OP_Delete, i_csr);
    }
    if reg_record == 0 {
        reg_record = make_sorter_record(p, sort, sel, reg_base, n_base);
    }
    let op = if sort.sort_flags & SORTFLAG_USE_SORTER != 0 { OP_SorterInsert } else { OP_IdxInsert };
    p.v.add_op4_int(op, sort.i_e_cursor, reg_record, reg_base + n_ob_sat, n_base - n_ob_sat);
    if i_skip != 0 {
        let t = if sort.label_ob_lopt != 0 { sort.label_ob_lopt } else { p.v.current_addr() };
        p.v.change_p2(i_skip, t);
    }
}

/// `selectInnerLoop(pParse, p, srcTab, pSort, pDistinct, pDest, iContinue, iBreak)`: this
/// routine generates the code for the inside of the inner loop of a SELECT.
///
/// If srcTab is negative, then the p->pEList expressions are evaluated in order to get the
/// data for this row. If srcTab is zero or more, then data is pulled from srcTab and
/// p->pEList is used only to get the number of columns and the collation sequence for each
/// column.
#[allow(clippy::too_many_arguments)]
fn select_inner_loop(
    p: &mut Parse,
    sel: SelectId,
    src_tab: i32,
    sort: Option<&mut SortCtx>,
    distinct: Option<&DistinctCtx>,
    dest: &mut SelectDest,
    i_continue: i32,
    i_break: i32,
) {
    let e_dest = dest.e_dest as i32; // How to dispose of results
    let i_parm = dest.i_sd_parm; // First argument to disposal method
    let mut n_prefix_reg = 0; // Number of extra registers before regResult
    let has_distinct = distinct.map_or(WHERE_DISTINCT_NOOP, |d| d.e_tnct_type as i32);
    let mut sort = match sort {
        Some(s) if s.p_order_by.nn() => Some(s),
        _ => None,
    };
    if sort.is_none() && has_distinct == 0 {
        let off = p.s(sel).i_offset;
        code_offset(p, off, i_continue);
    }

    // Pull the requested columns.
    let mut n_result_col = p.l(p.s(sel).p_e_list).n_expr();

    if dest.i_sdst == 0 {
        if let Some(s) = sort.as_deref() {
            n_prefix_reg = p.l(s.p_order_by).n_expr();
            if (s.sort_flags & SORTFLAG_USE_SORTER) == 0 {
                n_prefix_reg += 1;
            }
            p.n_mem += n_prefix_reg;
        }
        dest.i_sdst = p.n_mem + 1;
        p.n_mem += n_result_col;
    } else if dest.i_sdst + n_result_col > p.n_mem {
        // This is an error condition that can result, for example, when a SELECT on the
        // right-hand side of an INSERT contains more result columns than there are columns in
        // the table on the left. The error will be caught and reported later. But we need to
        // make sure enough memory is allocated to avoid other spurious errors in the
        // meantime.
        p.n_mem += n_result_col;
    }
    dest.n_sdst = n_result_col;
    // Usually, regResult is the first cell in an array of memory cells containing the
    // current result row. In this case regOrig is set to the same value. However, if the
    // results are being sent to the sorter, the values for any expressions that are also
    // part of the sort-key are omitted from this array. In this case regOrig is set to zero.
    let reg_result = dest.i_sdst; // Start of memory holding current results
    let mut reg_orig = reg_result; // Start of memory holding full result (or 0)
    if src_tab >= 0 {
        for i in 0..n_result_col {
            p.v.add_op3(OP_Column, src_tab, i, reg_result + i);
        }
    } else if e_dest != SRT_Exists {
        // If the destination is an EXISTS(...) expression, the actual values returned by the
        // SELECT are not required.
        let mut ecel_flags: u8 = if e_dest == SRT_Mem || e_dest == SRT_Output || e_dest == SRT_Coroutine {
            SQLITE_ECEL_DUP as u8
        } else {
            0
        };
        if let Some(s) = sort.as_deref()
            && has_distinct == 0
            && e_dest != SRT_EphemTab
            && e_dest != SRT_Table
        {
            // For each expression in p->pEList that is a copy of an expression in the ORDER
            // BY clause (pSort->pOrderBy), set the associated iOrderByCol value to one more
            // than the index of the ORDER BY expression within the sort-key that
            // pushOntoSorter() will generate. This allows the p->pEList field to be omitted
            // from the sorted record, saving space and CPU cycles.
            ecel_flags |= (SQLITE_ECEL_OMITREF | SQLITE_ECEL_REF) as u8;
            let el = p.s(sel).p_e_list;
            for i in s.n_ob_sat..p.l(s.p_order_by).n_expr() {
                let j = p.l(s.p_order_by).a[i as usize].i_order_by_col as i32;
                if j > 0 {
                    p.lm(el).a[(j - 1) as usize].i_order_by_col = (i + 1 - s.n_ob_sat) as u16;
                }
            }

            // Adjust nResultCol to account for columns that are omitted from the sorter by
            // the optimizations in this branch
            for i in 0..p.l(el).a.len() {
                if p.l(el).a[i].i_order_by_col > 0 {
                    n_result_col -= 1;
                    reg_orig = 0;
                }
            }
        }
        let s_row_load_info = RowLoadInfo { reg_result, ecel_flags };
        if p.s(sel).i_limit != 0 && (ecel_flags & SQLITE_ECEL_OMITREF as u8) != 0 && n_prefix_reg > 0 {
            sort.as_deref_mut().expect("sorter").p_deferred_row_load = Some(s_row_load_info);
            reg_orig = 0;
        } else {
            inner_loop_load_row(p, sel, &s_row_load_info);
        }
    }

    // If the DISTINCT keyword was present on the SELECT statement and this row has been seen
    // before, then do not make this row part of the result.
    if has_distinct != 0 {
        let d = distinct.expect("distinct context");
        let e_type = d.e_tnct_type as i32;
        let el = p.s(sel).p_e_list;
        let i_tab = code_distinct(p, e_type, d.tab_tnct, i_continue, el, reg_result);
        fix_distinct_open_eph(p, e_type, i_tab, d.addr_tnct);
        if sort.is_none() {
            let off = p.s(sel).i_offset;
            code_offset(p, off, i_continue);
        }
    }

    match e_dest {
        // Store the result as data using a unique key.
        SRT_Fifo | SRT_DistFifo | SRT_Table | SRT_EphemTab => {
            let r1 = sqlite3_get_temp_range(p, n_prefix_reg + 1);
            p.v.add_op3(OP_MakeRecord, reg_result, n_result_col, r1 + n_prefix_reg);
            if e_dest == SRT_DistFifo {
                // If the destination is DistFifo, then cursor (iParm+1) is open on an
                // ephemeral index. If the current row is already present in the index, do not
                // write it to the output. If not, add the current row to the index and
                // proceed with writing it to the output table as well.
                let addr = p.v.current_addr() + 4;
                p.v.add_op4_int(OP_Found, i_parm + 1, addr, r1, 0);
                p.v.add_op4_int(OP_IdxInsert, i_parm + 1, r1, reg_result, n_result_col);
            }
            if let Some(s) = sort.as_deref_mut() {
                push_onto_sorter(p, s, sel, r1 + n_prefix_reg, reg_orig, 1, n_prefix_reg);
            } else {
                let r2 = sqlite3_get_temp_reg(p);
                p.v.add_op2(OP_NewRowid, i_parm, r2);
                p.v.add_op3(OP_Insert, i_parm, r1, r2);
                p.v.change_p5(OPFLAG_APPEND as u16);
                sqlite3_release_temp_reg(p, r2);
            }
            sqlite3_release_temp_range(p, r1, n_prefix_reg + 1);
        }
        SRT_Upfrom => {
            p.not_supported("SELECT destination of UPDATE ... FROM");
        }
        // If we are creating a set for an "expr IN (SELECT ...)" construct, then there should
        // be a single item on the stack. Write this item into the set table with bogus data.
        SRT_Set => {
            if let Some(s) = sort.as_deref_mut() {
                // At first glance you would think we could optimize out the ORDER BY in this
                // case since the order of entries in the set does not matter. But there might
                // be a LIMIT clause, in which case the order does matter
                push_onto_sorter(p, s, sel, reg_result, reg_orig, n_result_col, n_prefix_reg);
                dest.i_sd_parm2 = 0; // Signal that any Bloom filter is unpopulated
            } else {
                let r1 = sqlite3_get_temp_reg(p);
                let mut aff = dest.z_aff_sdst.clone().unwrap_or_default();
                aff.truncate(n_result_col as usize);
                p.v.add_op4(OP_MakeRecord, reg_result, n_result_col, r1, P4::Dynamic(aff));
                p.v.add_op4_int(OP_IdxInsert, i_parm, r1, reg_result, n_result_col);
                if dest.i_sd_parm2 != 0 {
                    p.v.add_op4_int(OP_FilterAdd, dest.i_sd_parm2, 0, reg_result, n_result_col);
                    crate::build::sqlite3_vdbe_explain(p, false, "CREATE BLOOM FILTER".to_owned());
                }
                sqlite3_release_temp_reg(p, r1);
            }
        }
        // If any row exist in the result set, record that fact and abort.
        SRT_Exists => {
            p.v.add_op2(OP_Integer, 1, i_parm);
            // The LIMIT clause will terminate the loop for us
        }
        // If this is a scalar select that is part of an expression, then store the results in
        // the appropriate memory cell or array of memory cells and break out of the scan loop.
        SRT_Mem => {
            if let Some(s) = sort.as_deref_mut() {
                push_onto_sorter(p, s, sel, reg_result, reg_orig, n_result_col, n_prefix_reg);
                dest.i_sd_parm = reg_result;
            } else if reg_result != i_parm {
                // This occurs in cases where the SELECT had both a DISTINCT and an OFFSET
                // clause.
                p.v.add_op3(OP_Copy, reg_result, i_parm, n_result_col - 1);
            }
            // The LIMIT clause will jump out of the loop for us
        }
        // Send the data to the callback function or to a subroutine. In the case of a
        // subroutine, the subroutine itself is responsible for popping the data from the
        // stack.
        SRT_Coroutine | SRT_Output => {
            if let Some(s) = sort.as_deref_mut() {
                push_onto_sorter(p, s, sel, reg_result, reg_orig, n_result_col, n_prefix_reg);
            } else if e_dest == SRT_Coroutine {
                p.v.add_op1(OP_Yield, dest.i_sd_parm);
            } else {
                p.v.add_op2(OP_ResultRow, reg_result, n_result_col);
            }
        }
        // Write the results into a priority queue that is order according to pDest->pOrderBy
        // (in pSO). pDest->iSDParm (in iParm) is the cursor for an index with pSO->nExpr+2
        // columns. Build a key using pSO for the first N columns, and the Sequence
        // (OP_Sequence) as the N+1-th column. The final column is the record containing the
        // result set.
        SRT_DistQueue | SRT_Queue => {
            let so = dest.p_order_by;
            let n_key = p.l(so).n_expr();
            let r1 = sqlite3_get_temp_reg(p);
            let r2 = sqlite3_get_temp_range(p, n_key + 2);
            let r3 = r2 + n_key + 1;
            let mut addr_test = 0;
            if e_dest == SRT_DistQueue {
                // If the destination is DistQueue, then cursor (iParm+1) is open on a second
                // ephemeral index that holds all values every previously added to the queue.
                addr_test = p.v.add_op4_int(OP_Found, i_parm + 1, 0, reg_result, n_result_col);
            }
            p.v.add_op3(OP_MakeRecord, reg_result, n_result_col, r3);
            if e_dest == SRT_DistQueue {
                p.v.add_op2(OP_IdxInsert, i_parm + 1, r3);
                p.v.change_p5(OPFLAG_USESEEKRESULT as u16);
            }
            for i in 0..n_key {
                let col = p.l(so).a[i as usize].i_order_by_col as i32;
                p.v.add_op2(OP_SCopy, reg_result + col - 1, r2 + i);
            }
            p.v.add_op2(OP_Sequence, i_parm, r2 + n_key);
            p.v.add_op3(OP_MakeRecord, r2, n_key + 2, r1);
            p.v.add_op4_int(OP_IdxInsert, i_parm, r1, r2, n_key + 2);
            if addr_test != 0 {
                p.v.jump_here(addr_test);
            }
            sqlite3_release_temp_reg(p, r1);
            sqlite3_release_temp_range(p, r2, n_key + 2);
        }
        // Discard the results. This is used for SELECT statements inside the body of a
        // TRIGGER. The purpose of such selects is to call user-defined functions that have
        // side effects. We do not care about the actual results of the select.
        _ => {}
    }

    // Jump to the end of the loop if the LIMIT is reached. Except, if there is a sorter, in
    // which case the sorter has already limited the output for us.
    let lim = p.s(sel).i_limit;
    if sort.is_none() && lim != 0 {
        p.v.add_op2(OP_DecrJumpZero, lim, i_break);
    }
}

/// `explainTempTable(pParse, zUsage)`: unless an "EXPLAIN QUERY PLAN" command is being
/// processed, this function is a no-op. Otherwise, it adds a single row of output to the EQP
/// result-set with the format:
///
///   "USE TEMP B-TREE FOR xxx"
///
/// where xxx is one of "DISTINCT", "ORDER BY" or "GROUP BY". Exactly which is determined by
/// the zUsage argument.
fn explain_temp_table(p: &mut Parse, z_usage: &str) {
    crate::build::sqlite3_vdbe_explain(p, false, format!("USE TEMP B-TREE FOR {z_usage}"));
}

/// `generateSortTail(pParse, p, pSort, nColumn, pDest)`: if the inner loop was generated
/// using a non-null pOrderBy argument, then the results were placed in a sorter. After the
/// loop is terminated we need to run the sorter and output the results. The following
/// routine generates the code needed to do that.
fn generate_sort_tail(p: &mut Parse, sel: SelectId, sort: &mut SortCtx, n_column: i32, dest: &mut SelectDest) {
    let mut n_column = n_column;
    let addr_break = sort.label_done; // Jump here to exit loop
    let addr_continue = p.v.make_label(); // Jump here for next cycle
    let mut addr_once = 0;
    let order_by = sort.p_order_by;
    let e_dest = dest.e_dest as i32;
    let i_parm = dest.i_sd_parm;
    let (reg_row, reg_rowid);
    let i_sort_tab; // Sorter cursor to read from
    let b_seq; // True if sorter record includes seq. no.
    let n_ref_key = 0;
    let e_list = p.s(sel).p_e_list;

    let n_key = p.l(order_by).n_expr() - sort.n_ob_sat; // Number of key columns in sorter record
    if sort.n_ob_sat == 0 || n_key == 1 {
        crate::build::sqlite3_vdbe_explain(
            p,
            false,
            format!("USE TEMP B-TREE FOR {}ORDER BY", if sort.n_ob_sat != 0 { "LAST TERM OF " } else { "" }),
        );
    } else {
        crate::build::sqlite3_vdbe_explain(p, false, format!("USE TEMP B-TREE FOR LAST {n_key} TERMS OF ORDER BY"));
    }

    if sort.label_bk_out != 0 {
        p.v.add_op2(OP_Gosub, sort.reg_return, sort.label_bk_out);
        p.v.goto(addr_break);
        p.v.resolve_label(sort.label_bk_out);
    }

    let i_tab = sort.i_e_cursor;
    if e_dest == SRT_Output || e_dest == SRT_Coroutine || e_dest == SRT_Mem {
        if e_dest == SRT_Mem && p.s(sel).i_offset != 0 {
            p.v.add_op2(OP_Null, 0, dest.i_sdst);
        }
        reg_rowid = 0;
        reg_row = dest.i_sdst;
    } else {
        reg_rowid = sqlite3_get_temp_reg(p);
        if e_dest == SRT_EphemTab || e_dest == SRT_Table {
            reg_row = sqlite3_get_temp_reg(p);
            n_column = 0;
        } else {
            reg_row = sqlite3_get_temp_range(p, n_column);
        }
    }
    let addr; // Top of output loop. Jump for Next.
    if sort.sort_flags & SORTFLAG_USE_SORTER != 0 {
        p.n_mem += 1;
        let reg_sort_out = p.n_mem;
        i_sort_tab = p.n_tab;
        p.n_tab += 1;
        if sort.label_bk_out != 0 {
            addr_once = p.v.add_op0(OP_Once);
        }
        p.v.add_op3(OP_OpenPseudo, i_sort_tab, reg_sort_out, n_key + 1 + n_column + n_ref_key);
        if addr_once != 0 {
            p.v.jump_here(addr_once);
        }
        addr = 1 + p.v.add_op2(OP_SorterSort, i_tab, addr_break);
        p.v.add_op3(OP_SorterData, i_tab, reg_sort_out, i_sort_tab);
        b_seq = 0;
    } else {
        addr = 1 + p.v.add_op2(OP_Sort, i_tab, addr_break);
        let off = p.s(sel).i_offset;
        code_offset(p, off, addr_continue);
        i_sort_tab = i_tab;
        b_seq = 1;
        if p.s(sel).i_offset > 0 {
            let lim = p.s(sel).i_limit;
            p.v.add_op2(OP_AddImm, lim, -1);
        }
    }
    let mut i_col = n_key + b_seq - 1;
    for i in 0..n_column {
        if p.l(e_list).a[i as usize].i_order_by_col == 0 {
            i_col += 1;
        }
    }
    for i in (0..n_column).rev() {
        let ob_col = p.l(e_list).a[i as usize].i_order_by_col as i32;
        let i_read;
        if ob_col != 0 {
            i_read = ob_col - 1;
        } else {
            i_read = i_col;
            i_col -= 1;
        }
        p.v.add_op3(OP_Column, i_sort_tab, i_read, reg_row + i);
    }
    match e_dest {
        SRT_Table | SRT_EphemTab => {
            p.v.add_op3(OP_Column, i_sort_tab, n_key + b_seq, reg_row);
            p.v.add_op2(OP_NewRowid, i_parm, reg_rowid);
            p.v.add_op3(OP_Insert, i_parm, reg_row, reg_rowid);
            p.v.change_p5(OPFLAG_APPEND as u16);
        }
        SRT_Set => {
            let mut aff = dest.z_aff_sdst.clone().unwrap_or_default();
            aff.truncate(n_column as usize);
            p.v.add_op4(OP_MakeRecord, reg_row, n_column, reg_rowid, P4::Dynamic(aff));
            p.v.add_op4_int(OP_IdxInsert, i_parm, reg_rowid, reg_row, n_column);
        }
        SRT_Mem => {
            // The LIMIT clause will terminate the loop for us
        }
        SRT_Upfrom => {
            p.not_supported("SELECT destination of UPDATE ... FROM");
        }
        _ => {
            if e_dest == SRT_Output {
                p.v.add_op2(OP_ResultRow, dest.i_sdst, n_column);
            } else {
                p.v.add_op1(OP_Yield, dest.i_sd_parm);
            }
        }
    }
    if reg_rowid != 0 {
        if e_dest == SRT_Set {
            sqlite3_release_temp_range(p, reg_row, n_column);
        } else {
            sqlite3_release_temp_reg(p, reg_row);
        }
        sqlite3_release_temp_reg(p, reg_rowid);
    }
    // The bottom of the loop
    p.v.resolve_label(addr_continue);
    if sort.sort_flags & SORTFLAG_USE_SORTER != 0 {
        p.v.add_op2(OP_SorterNext, i_tab, addr);
    } else {
        p.v.add_op2(OP_Next, i_tab, addr);
    }
    if sort.reg_return != 0 {
        p.v.add_op1(OP_Return, sort.reg_return);
    }
    p.v.resolve_label(addr_break);
}

/// `computeLimitRegisters(pParse, p, iBreak)`: compute the iLimit and iOffset fields of the
/// SELECT based on the pLimit expressions. pLimit->pLeft and pLimit->pRight hold the
/// expressions that contain the limit and offset, respectively. iLimit and iOffset are the
/// register numbers of counters used to compute the limit and offset. If there is no limit
/// and/or offset, then iLimit and iOffset are negative.
///
/// This routine changes the values of iLimit and iOffset only if a limit or offset is
/// defined by pLimit->pLeft and pLimit->pRight. iLimit and iOffset should have been preset
/// to appropriate default values (zero) prior to calling this routine.
///
/// The iOffset register (if it exists) is initialized to the value of the OFFSET. The
/// iLimit register is initialized to LIMIT. Register iOffset+1 is initialized to
/// LIMIT+OFFSET.
///
/// Only if pLimit->pLeft!=0 do the limit registers get redefined. The UNION ALL operator
/// uses this property to force the reuse of the same limit and offset registers across
/// multiple SELECT statements.
pub(crate) fn compute_limit_registers(p: &mut Parse, sel: SelectId, i_break: i32) {
    let limit = p.s(sel).p_limit;
    if p.s(sel).i_limit != 0 {
        return;
    }
    // "LIMIT -1" always shows all rows. There is some controversy about what the correct
    // behavior should be. The current implementation interprets "LIMIT 0" to mean no rows.
    if limit.nn() {
        p.n_mem += 1;
        let i_limit = p.n_mem;
        p.sm(sel).i_limit = i_limit;
        sqlite3_get_vdbe(p);
        let left = p.e(limit).p_left;
        if let Some(n) = sqlite3_expr_is_integer(p, left, true, false) {
            p.v.add_op2(OP_Integer, n, i_limit);
            if n == 0 {
                p.v.goto(i_break);
            } else if n >= 0 && p.s(sel).n_select_row > sqlite3_log_est(n as u64) {
                p.sm(sel).n_select_row = sqlite3_log_est(n as u64);
                p.sm(sel).sel_flags |= SF_FixedLimit;
            }
        } else {
            sqlite3_expr_code(p, left, i_limit);
            p.v.add_op1(OP_MustBeInt, i_limit);
            sqlite3_may_abort(p);
            p.v.add_op2(OP_IfNot, i_limit, i_break);
        }
        let right = p.e(limit).p_right;
        if right.nn() {
            p.n_mem += 1;
            let i_offset = p.n_mem;
            p.sm(sel).i_offset = i_offset;
            p.n_mem += 1; // Allocate an extra register for limit+offset
            sqlite3_expr_code(p, right, i_offset);
            p.v.add_op1(OP_MustBeInt, i_offset);
            sqlite3_may_abort(p);
            p.v.add_op3(OP_OffsetLimit, i_limit, i_offset + 1, i_offset);
        }
    }
}

/// `multiSelectValues(pParse, p, pDest)`: handle the special case of a compound-select that
/// originates from a VALUES clause. By handling this as a special case, we avoid deep
/// recursion, and thus do not need to enforce the SQLITE_LIMIT_COMPOUND_SELECT on a VALUES
/// clause.
///
/// Because the Select object originates from a VALUES clause:
///   (1) There is no LIMIT or OFFSET or else there is a LIMIT of exactly 1
///   (2) All terms are UNION ALL
///   (3) There is no ORDER BY clause
///
/// The "LIMIT of exactly 1" case of condition (1) comes about when a VALUES clause occurs
/// within scalar expression (ex: "SELECT (VALUES(1),(2),(3))"). The sqlite3CodeSubselect
/// will have added the LIMIT 1 clause in that case. Since the limit is exactly 1, we only
/// need to evaluate the left-most VALUES.
fn multi_select_values(p: &mut Parse, sel: SelectId, dest: &mut SelectDest) -> i32 {
    let mut n_row = 1;
    let b_show_all = p.s(sel).p_limit.is_null();
    let mut s = sel;
    loop {
        if p.s(s).p_prior.is_null() {
            break;
        }
        s = p.s(s).p_prior;
        n_row += b_show_all as i32;
    }
    crate::build::sqlite3_vdbe_explain(p, false, format!("SCAN {} CONSTANT ROW{}", n_row, if n_row == 1 { "" } else { "S" }));
    while s.nn() {
        select_inner_loop(p, s, -1, None, None, dest, 1, 1);
        if !b_show_all {
            break;
        }
        p.sm(s).n_select_row = n_row as LogEst;
        s = p.s(s).p_next;
    }
    0
}

/// `unionWithLimitOne(db, p)`: return true if the SELECT statement which is known to be the
/// recursive part of a recursive CTE still has its anchor terms attached... no: return true
/// if `p` is a UNION with "LIMIT 1" and no OFFSET, which is evaluated like a UNION ALL.
pub(crate) fn union_with_limit_one(p: &mut Parse, sel: SelectId) -> bool {
    if p.s(sel).op != TK_UNION {
        return false;
    }
    let lim = p.s(sel).p_limit;
    if lim.is_null() {
        return false;
    }
    if p.e(lim).p_right.nn() {
        return false;
    }
    let left = p.e(lim).p_left;
    match sqlite3_expr_is_integer(p, left, false, false) {
        Some(1) => {}
        _ => return false,
    }
    if p.db.optimization_disabled(SQLITE_UnionLimit) {
        return false;
    }
    true
}

/// `hasAnchor(p)`: return true if the SELECT statement which is known to be the recursive part
/// of a recursive CTE still has its anchor terms attached. If the anchor terms have already
/// been removed, return false.
fn has_anchor(p: &Parse, mut sel: SelectId) -> bool {
    while sel.nn() && (p.s(sel).sel_flags & SF_Recursive) != 0 {
        sel = p.s(sel).p_prior;
    }
    sel.nn()
}

/// `generateWithRecursiveQuery(pParse, p, pDest)`: this routine generates VDBE code to compute
/// the content of a WITH RECURSIVE query of the form:
///
/// ```text
///   <recursive-table> AS (<setup-query> UNION [ALL] <recursive-query>)
///                         \___________/             \_______________/
///                           p->pPrior                      p
/// ```
///
/// There is exactly one reference to the recursive-table in the FROM clause of
/// recursive-query, marked with the SrcList->a[].fg.isRecursive flag.
///
/// The setup-query runs once to generate an initial set of rows that go into a Queue table.
/// Rows are extracted from the Queue table one by one. Each row extracted from Queue is
/// output to pDest. Then the single extracted row (now in the iCurrent table) becomes the
/// content of the recursive-table for a recursive-query run. The output of the
/// recursive-query is added to the Queue table. Then another row is extracted from Queue and
/// the iteration continues until the Queue table is empty.
///
/// If the compound query operator is UNION then no duplicate rows are ever inserted into the
/// Queue table. The iDistinct table keeps a copy of all rows that have ever been inserted
/// into Queue and causes duplicates to be discarded. If the operator is UNION ALL, then
/// duplicates are allowed.
///
/// If the query has an ORDER BY, then entries in the Queue table are kept in ORDER BY order
/// and the first entry is extracted for each cycle. Without an ORDER BY, the Queue table is
/// just a FIFO.
///
/// If a LIMIT clause is provided, then the iteration stops after LIMIT rows have been output
/// to pDest. A LIMIT of zero means to output no rows and a negative LIMIT means to output
/// all rows. If there is also an OFFSET clause, then that many initial rows are skipped
/// before any rows are output.
fn generate_with_recursive_query(p: &mut Parse, sel: SelectId, dest: &mut SelectDest) {
    let src = p.s(sel).p_src; // The FROM clause of the recursive query
    let mut n_col = p.l(p.s(sel).p_e_list).n_expr(); // Number of columns in the recursive table
    let mut i_current = 0; // The Current table
    let mut i_distinct = 0; // To ensure unique results if UNION

    // (p->pWin: window functions are reported as unsupported by the parser)

    // (sqlite3AuthCheck(pParse, SQLITE_RECURSIVE, ...): authorization is done by the engine)

    // Process the LIMIT and OFFSET clauses, if they exist
    let addr_break = p.v.make_label();
    p.sm(sel).n_select_row = 320; // 4 billion rows
    compute_limit_registers(p, sel, addr_break);
    let limit = p.s(sel).p_limit;
    let reg_limit = p.s(sel).i_limit;
    let reg_offset = p.s(sel).i_offset;
    p.sm(sel).p_limit = ExprId::NULL;
    p.sm(sel).i_limit = 0;
    p.sm(sel).i_offset = 0;
    let order_by = p.s(sel).p_order_by;

    // Locate the cursor number of the Current table
    for it in &p.src(src).a {
        if it.is_recursive {
            i_current = it.i_cursor;
            break;
        }
    }

    // Allocate cursors numbers for Queue and Distinct. The cursor number for the Distinct
    // table must be exactly one greater than Queue in order for the SRT_DistFifo and
    // SRT_DistQueue destinations to work.
    let i_queue = p.n_tab;
    p.n_tab += 1;
    let e_dest;
    if p.s(sel).op == TK_UNION {
        e_dest = if order_by.nn() { SRT_DistQueue } else { SRT_DistFifo };
        i_distinct = p.n_tab;
        p.n_tab += 1;
    } else {
        e_dest = if order_by.nn() { SRT_Queue } else { SRT_Fifo };
    }
    let mut dest_queue = sqlite3_select_dest_init(e_dest, i_queue);

    // Allocate cursors for Current, Queue, and Distinct.
    p.n_mem += 1;
    let reg_current = p.n_mem;
    p.v.add_op3(OP_OpenPseudo, i_current, reg_current, n_col);
    if order_by.nn() {
        let key_info = crate::merge::multi_select_by_merge_key_info(p, sel, 1);
        let n = p.l(order_by).n_expr();
        p.v.add_op4(OP_OpenEphemeral, i_queue, n + 2, 0, P4::KeyInfo(Arc::new(key_info)));
        dest_queue.p_order_by = order_by;
    } else {
        p.v.add_op2(OP_OpenEphemeral, i_queue, n_col);
    }
    if i_distinct != 0 {
        // Generate an ephemeral table used to enforce distinctness on the output of the
        // recursive part of the CTE.
        n_col = p.l(p.s(sel).p_e_list).n_expr();
        let mut key_info = KeyInfo::alloc(p.enc(), n_col as usize, 1);
        for i in 0..n_col {
            let c = match crate::merge::multi_select_coll_seq(p, sel, i) {
                Some(c) => c,
                None => crate::callback::dflt_coll(p),
            };
            key_info.a_coll[i as usize] = Some(c);
        }
        p.v.add_op4(OP_OpenEphemeral, i_distinct, n_col, 0, P4::KeyInfo(Arc::new(key_info)));
    }

    // Detach the ORDER BY clause from the compound SELECT
    p.sm(sel).p_order_by = ExprListId::NULL;

    // Figure out how many elements of the compound SELECT are part of the recursive query.
    // Make sure no recursive elements use aggregate functions. Mark the recursive elements as
    // UNION ALL even if they are really UNION because the distinctness will be enforced by
    // the iDistinct table. pFirstRec is left pointing to the left-most recursive term of the
    // CTE.
    let mut first_rec = sel;
    let mut ok = true;
    loop {
        if p.s(first_rec).sel_flags & SF_Aggregate != 0 {
            p.error_msg("recursive aggregate queries not supported".to_owned());
            ok = false;
            break;
        }
        p.sm(first_rec).op = TK_ALL;
        let prior = p.s(first_rec).p_prior;
        if (p.s(prior).sel_flags & SF_Recursive) == 0 {
            break;
        }
        first_rec = prior;
    }

    if ok {
        // Store the results of the setup-query in Queue.
        let setup = p.s(first_rec).p_prior;
        p.sm(setup).p_next = SelectId::NULL;
        crate::build::sqlite3_vdbe_explain(p, true, "SETUP".to_owned());
        let rc = sqlite3_select(p, setup, &mut dest_queue);
        p.sm(setup).p_next = sel;
        if rc == 0 {
            // Find the next row in the Queue and output that row
            let addr_top = p.v.add_op2(OP_Rewind, i_queue, addr_break);

            // Transfer the next row in Queue over to Current
            p.v.add_op1(OP_NullRow, i_current); // To reset column cache
            if order_by.nn() {
                let n = p.l(order_by).n_expr();
                p.v.add_op3(OP_Column, i_queue, n + 1, reg_current);
            } else {
                p.v.add_op2(OP_RowData, i_queue, reg_current);
            }
            p.v.add_op1(OP_Delete, i_queue);

            // Output the single row in Current
            let addr_cont = p.v.make_label();
            code_offset(p, reg_offset, addr_cont);
            select_inner_loop(p, sel, i_current, None, None, dest, addr_cont, addr_break);
            if reg_limit != 0 {
                p.v.add_op2(OP_DecrJumpZero, reg_limit, addr_break);
            }
            p.v.resolve_label(addr_cont);

            // Execute the recursive SELECT taking the single row in Current as the value for
            // the recursive-table. Store the results in the Queue.
            p.sm(first_rec).p_prior = SelectId::NULL;
            crate::build::sqlite3_vdbe_explain(p, true, "RECURSIVE STEP".to_owned());
            sqlite3_select(p, sel, &mut dest_queue);
            p.sm(first_rec).p_prior = setup;

            // Keep running the loop until the Queue is empty
            p.v.goto(addr_top);
            p.v.resolve_label(addr_break);
        }
    }

    // end_of_recursive_query:
    p.sm(sel).p_order_by = order_by;
    p.sm(sel).p_limit = limit;
}

/// `multiSelect(pParse, p, pDest)`: this routine is called to process a compound query form
/// from two or more separate queries using UNION, UNION ALL, EXCEPT, or INTERSECT. The
/// right-most SELECT is `sel`; the others are found through `pPrior`.
fn multi_select(p: &mut Parse, sel: SelectId, p_dest: &mut SelectDest) -> i32 {
    let mut rc = SQLITE_OK;
    let prior = p.s(sel).p_prior;
    let mut dest = p_dest.clone();
    sqlite3_get_vdbe(p);

    // Create the destination temporary table if necessary
    if dest.e_dest as i32 == SRT_EphemTab {
        let n = p.l(p.s(sel).p_e_list).n_expr();
        p.v.add_op2(OP_OpenEphemeral, dest.i_sd_parm, n);
        dest.e_dest = SRT_Table as u8;
    }

    // Special handling for a compound-select that originates as a VALUES clause.
    let mut done = false;
    if p.s(sel).sel_flags & SF_MultiValue != 0 {
        rc = multi_select_values(p, sel, &mut dest);
        done = true;
    }
    if !done {
        // Make sure all SELECTs in the statement have the same number of elements in their
        // result sets.
        if (p.s(sel).sel_flags & SF_Recursive) != 0 && has_anchor(p, sel) {
            generate_with_recursive_query(p, sel, &mut dest);
        } else if p.s(sel).p_order_by.nn() {
            return crate::merge::multi_select_by_merge(p, sel, p_dest);
        } else if p.s(sel).op != TK_ALL && !union_with_limit_one(p, sel) {
            // UNION, INTERSECT and EXCEPT without an ORDER BY clause are computed by the merge
            // algorithm with a synthesized "ORDER BY 1".
            let one = sqlite3_expr_int32(p, 1);
            let ob = sqlite3_expr_list_append(p, ExprListId::NULL, one);
            p.sm(sel).p_order_by = ob;
            if p.n_err == 0 {
                p.lm(ob).a[0].i_order_by_col = 1;
                return crate::merge::multi_select_by_merge(p, sel, p_dest);
            }
        } else {
            // Generate code for all terms of the compound in order: UNION ALL (or a UNION
            // with LIMIT 1)
            let mut addr = 0;
            let mut n_limit = 0;
            if p.s(prior).p_prior.is_null() {
                crate::build::sqlite3_vdbe_explain(p, true, "COMPOUND QUERY".to_owned());
                crate::build::sqlite3_vdbe_explain(p, true, "LEFT-MOST SUBQUERY".to_owned());
            }
            let (lim, off, plimit) = (p.s(sel).i_limit, p.s(sel).i_offset, p.s(sel).p_limit);
            p.sm(prior).i_limit = lim;
            p.sm(prior).i_offset = off;
            let dup = sqlite3_expr_dup(p, plimit, 0);
            p.sm(prior).p_limit = dup;
            rc = sqlite3_select(p, prior, &mut dest);
            p.sm(prior).p_limit = ExprId::NULL;
            if rc == 0 {
                p.sm(sel).p_prior = SelectId::NULL;
                let (lim, off) = (p.s(prior).i_limit, p.s(prior).i_offset);
                p.sm(sel).i_limit = lim;
                p.sm(sel).i_offset = off;
                if lim != 0 {
                    addr = p.v.add_op1(OP_IfNot, lim);
                    if off != 0 {
                        p.v.add_op3(OP_OffsetLimit, lim, off + 1, off);
                    }
                }
                crate::build::sqlite3_vdbe_explain(p, true, "UNION ALL".to_owned());
                rc = sqlite3_select(p, sel, &mut dest);
                p.sm(sel).p_prior = prior;
                let n = sqlite3_log_est_add(p.s(sel).n_select_row, p.s(prior).n_select_row);
                p.sm(sel).n_select_row = n;
                let plimit = p.s(sel).p_limit;
                if plimit.nn() {
                    let left = p.e(plimit).p_left;
                    if let Some(v) = sqlite3_expr_is_integer(p, left, true, false) {
                        n_limit = v;
                        if n_limit > 0 && p.s(sel).n_select_row > sqlite3_log_est(n_limit as u64) {
                            p.sm(sel).n_select_row = sqlite3_log_est(n_limit as u64);
                        }
                    }
                }
                let _ = n_limit;
                if addr != 0 {
                    p.v.jump_here(addr);
                }
                if p.s(sel).p_next.is_null() {
                    crate::build::sqlite3_vdbe_explain_pop(p);
                }
            }
        }
    }
    // multi_select_end:
    p_dest.i_sdst = dest.i_sdst;
    p_dest.n_sdst = dest.n_sdst;
    p_dest.i_sd_parm2 = dest.i_sd_parm2;
    rc
}

/// `sqlite3CopySortOrder(p1, p2)`: if `p2` exists and has the same number of terms as `p1`,
/// copy the ASC/DESC sort order of each term of `p2` onto the corresponding term of `p1` and
/// return true; otherwise return false and leave `p1` unchanged.
fn sqlite3_copy_sort_order(p: &mut Parse, p1: ExprListId, p2: ExprListId) -> bool {
    if p2.nn() && p.l(p1).a.len() == p.l(p2).a.len() {
        for ii in 0..p.l(p1).a.len() {
            let sort_flags = p.l(p2).a[ii].sort_flags & sqliters_vdbe::vdbeaux::KEYINFO_ORDER_DESC;
            p.lm(p1).a[ii].sort_flags = sort_flags;
        }
        true
    } else {
        false
    }
}

/// `minMaxQuery(db, pFunc, ppMinMax)`: `func` is the only aggregate function in the query.
/// Check to see if the query is a candidate for the min/max optimization.
///
/// If the query is a candidate for the min/max optimization, then set `*pp_min_max` to be an
/// ORDER BY clause to be used for the optimization and return either WHERE_ORDERBY_MIN or
/// WHERE_ORDERBY_MAX depending on whether the function is a min() or max(). If the query is
/// not a candidate for the min/max optimization, return WHERE_ORDERBY_NORMAL (which must be
/// zero).
fn min_max_query(p: &mut Parse, func: ExprId, pp_min_max: &mut ExprListId) -> u8 {
    let e_ret = WHERE_ORDERBY_NORMAL as u8;
    let e_list = p.e(func).x_list();
    if e_list.is_null() || p.l(e_list).n_expr() != 1 || p.e(func).has(EP_WinFunc) || p.db.optimization_disabled(SQLITE_MinMaxOpt) {
        return e_ret;
    }
    let z_func = p.e(func).tok().to_owned();
    let (ret, sort_flags);
    if z_func.eq_ignore_ascii_case("min") {
        ret = WHERE_ORDERBY_MIN as u8;
        sort_flags = if sqlite3_expr_can_be_null(p, p.l(e_list).a[0].p_expr) {
            sqliters_vdbe::vdbeaux::KEYINFO_ORDER_BIGNULL
        } else {
            0
        };
    } else if z_func.eq_ignore_ascii_case("max") {
        ret = WHERE_ORDERBY_MAX as u8;
        sort_flags = sqliters_vdbe::vdbeaux::KEYINFO_ORDER_DESC;
    } else {
        return e_ret;
    }
    let ob = sqlite3_expr_list_dup(p, e_list, 0);
    p.lm(ob).a[0].sort_flags = sort_flags;
    *pp_min_max = ob;
    ret
}

/// `analyzeAggFuncArgs(pAggInfo, pNC)`: analyze the arguments to aggregate functions. Create
/// new pAggInfo->aCol[] entries for columns that are arguments to aggregate functions but
/// which are not otherwise used.
fn analyze_agg_func_args(p: &mut Parse, agg: AggInfoId, nc: NcId) {
    p.ncm(nc).nc_flags |= NC_InAggFunc;
    for i in 0..p.agg(agg).a_func.len() {
        let e = p.agg(agg).a_func[i].p_f_expr;
        let l = p.e(e).x_list();
        sqlite3_expr_analyze_agg_list(p, nc, l);
        let left = p.e(e).p_left;
        if left.nn() {
            let l = p.e(left).x_list();
            sqlite3_expr_analyze_agg_list(p, nc, l);
        }
        if p.e(e).has(EP_WinFunc) {
            let f = p.a_win[p.e(e).y_win.ix()].p_filter;
            sqlite3_expr_analyze_aggregates(p, nc, f);
        }
    }
    p.ncm(nc).nc_flags &= !NC_InAggFunc;
}

/// The aggregate-info dump (verification aid; the C side is `v3DumpAgg()` of the instrumented
/// scratch amalgamation): the AggInfo object as the code generator uses it, with the decisions
/// of the aggregate half of sqlite3Select() in `x`.
fn dump_agg(p: &mut Parse, sel: SelectId, agg: AggInfoId, p_where: ExprId, kind: &str, x: [i32; 4]) {
    if p.where_dump.is_none() {
        return;
    }
    fn count_and(p: &Parse, e: ExprId) -> i32 {
        if e.is_null() {
            0
        } else if p.e(e).op == TK_AND {
            count_and(p, p.e(e).p_left) + count_and(p, p.e(e).p_right)
        } else {
            1
        }
    }
    let a = p.agg(agg);
    let gb = p.s(sel).p_group_by;
    let mut out = vec![format!(
        "AGG {} sel={} nCol={} nAcc={} nFunc={} nSortCol={} sortIdx={} sortPTab={} useSortIdx={} firstReg={} x=[{},{},{},{}] nWhere={} nHaving={} nGroupBy={}",
        kind,
        p.s(sel).sel_id,
        a.n_column(),
        a.n_accumulator,
        a.n_func(),
        a.n_sorting_column,
        a.sorting_idx,
        a.sorting_idx_p_tab,
        a.use_sorting_idx,
        a.i_first_reg,
        x[0],
        x[1],
        x[2],
        x[3],
        count_and(p, p_where),
        count_and(p, p.s(sel).p_having),
        if gb.nn() { p.l(gb).n_expr() } else { 0 }
    )];
    for (i, c) in a.a_col.iter().enumerate() {
        out.push(format!(
            "AGGCOL {} tab={} iTable={} iColumn={} iSorter={} op={}",
            i,
            if c.p_tab.nn() { p.tab(c.p_tab).z_name.as_str() } else { "-" },
            c.i_table,
            c.i_column,
            c.i_sorter_column,
            if c.p_c_expr.nn() { p.e(c.p_c_expr).op as i32 } else { 0 }
        ));
    }
    for (i, f) in a.a_func.iter().enumerate() {
        let l = if f.p_f_expr.nn() { p.e(f.p_f_expr).x_list() } else { ExprListId::NULL };
        out.push(format!(
            "AGGFUNC {} name={} nArg={} iDistinct={} iOBTab={} bOBPayload={} bOBUnique={} bUseSubtype={} filter={}",
            i,
            f.p_func.as_ref().map_or("-", |d| d.name.as_str()),
            if l.nn() { p.l(l).n_expr() } else { 0 },
            f.i_distinct,
            f.i_ob_tab,
            f.b_ob_payload as i32,
            f.b_ob_unique as i32,
            f.b_use_subtype as i32,
            (f.p_f_expr.nn() && p.e(f.p_f_expr).has(EP_WinFunc)) as i32
        ));
    }
    if let Some(d) = p.where_dump.as_mut() {
        d.extend(out);
    }
}

/// `optimizeAggregateUseOfIndexedExpr(pParse, pSelect, pAggInfo, pNC)`: an index on
/// expressions is being used in the inner loop of an aggregate query with a GROUP BY clause.
/// This routine attempts to adjust the AggInfo object to take advantage of index and to
/// perhaps use the index as a covering index.
fn optimize_aggregate_use_of_indexed_expr(p: &mut Parse, sel: SelectId, agg: AggInfoId, nc: NcId) {
    let n_acc = p.agg(agg).n_accumulator as usize;
    p.aggm(agg).a_col.truncate(n_acc);
    if p.agg(agg).n_sorting_column > 0 {
        let mut mx = p.l(p.s(sel).p_group_by).n_expr() - 1;
        for c in &p.agg(agg).a_col {
            let k = c.i_sorter_column;
            if k > mx {
                mx = k;
            }
        }
        p.aggm(agg).n_sorting_column = (mx + 1) as u32;
    }
    analyze_agg_func_args(p, agg, nc);
}

/// `aggregateIdxEprRefToColCallback(pWalker, pExpr)`: walker callback for
/// aggregateConvertIndexedExprRefToColumn().
fn aggregate_idx_epr_ref_to_col_callback(p: &mut Parse, _w: &mut Walker, e: ExprId) -> i32 {
    let x = p.e(e);
    if x.p_agg_info.is_null() {
        return WRC_Continue;
    }
    if x.op == TK_AGG_COLUMN || x.op == TK_AGG_FUNCTION || x.op == TK_IF_NULL_ROW {
        return WRC_Continue;
    }
    let agg = x.p_agg_info;
    let i_agg = x.i_agg as i32;
    if i_agg >= p.agg(agg).n_column() {
        return WRC_Continue;
    }
    let (i_table, i_column) = {
        let c = &p.agg(agg).a_col[i_agg as usize];
        (c.i_table, c.i_column)
    };
    let x = p.em(e);
    x.op = TK_AGG_COLUMN;
    x.i_table = i_table;
    x.i_column = i_column as i16;
    x.flags &= !(EP_Skip | EP_Collate | EP_Unlikely);
    WRC_Prune
}

/// `aggregateConvertIndexedExprRefToColumn(pAggInfo)`: convert every pAggInfo->aFunc[].pExpr
/// such that any node within those expressions that has pAppInfo set is changed into a
/// TK_AGG_COLUMN opcode.
fn aggregate_convert_indexed_expr_ref_to_column(p: &mut Parse, agg: AggInfoId) {
    let mut w = Walker { x_expr_callback: Some(aggregate_idx_epr_ref_to_col_callback), ..Walker::default() };
    for i in 0..p.agg(agg).a_func.len() {
        let e = p.agg(agg).a_func[i].p_f_expr;
        sqlite3_walk_expr(p, &mut w, e);
    }
}

/// `assignAggregateRegisters(pParse, pAggInfo)`: allocate a block of registers so that there
/// is one register for each pAggInfo->aCol[] and pAggInfo->aFunc[] entry in pAggInfo.
fn assign_aggregate_registers(p: &mut Parse, agg: AggInfoId) {
    let n = p.agg(agg).n_column() + p.agg(agg).n_func();
    p.aggm(agg).i_first_reg = p.n_mem + 1;
    p.n_mem += n;
}

/// `resetAccumulator(pParse, pAggInfo)`: reset the aggregate accumulator.
///
/// The aggregate accumulator is a set of memory cells that hold intermediate results while
/// calculating an aggregate. This routine generates code that stores NULLs in all of those
/// memory cells.
fn reset_accumulator(p: &mut Parse, agg: AggInfoId) {
    let n_reg = p.agg(agg).n_func() + p.agg(agg).n_column();
    if n_reg == 0 {
        return;
    }
    if p.n_err != 0 {
        return;
    }
    let first = p.agg(agg).i_first_reg;
    p.v.add_op3(OP_Null, 0, first, first + n_reg - 1);
    for i in 0..p.agg(agg).a_func.len() {
        let (i_distinct, i_ob_tab, fe) = {
            let f = &p.agg(agg).a_func[i];
            (f.i_distinct, f.i_ob_tab, f.p_f_expr)
        };
        if i_distinct >= 0 {
            let list = p.e(fe).x_list();
            if list.is_null() || p.l(list).n_expr() != 1 {
                p.error_msg("DISTINCT aggregates must have exactly one argument");
                p.aggm(agg).a_func[i].i_distinct = -1;
            } else {
                let ki = sqlite3_key_info_from_expr_list(p, list, 0, 0);
                let a = p.v.add_op4(OP_OpenEphemeral, i_distinct, 0, 0, P4::KeyInfo(Arc::new(ki)));
                p.aggm(agg).a_func[i].i_dist_addr = a;
                if p.explain == 2 {
                    let name = p.agg(agg).a_func[i].p_func.as_ref().map(|f| f.name.to_string()).unwrap_or_default();
                    crate::build::sqlite3_vdbe_explain(p, false, format!("USE TEMP B-TREE FOR {name}(DISTINCT)"));
                }
            }
        }
        if i_ob_tab >= 0 {
            let (b_ob_unique, b_ob_payload, b_use_subtype) = {
                let f = &p.agg(agg).a_func[i];
                (f.b_ob_unique, f.b_ob_payload, f.b_use_subtype)
            };
            let ob_list = p.e(p.e(fe).p_left).x_list();
            let n_arg = p.l(p.e(fe).x_list()).n_expr();
            let mut n_extra = 0; // Number of extra columns
            if !b_ob_unique {
                n_extra += 1; // One extra column for the OP_Sequence
            }
            if b_ob_payload {
                // If the ORDER BY is not the same as the argument, then extra columns
                // contain the argument values
                n_extra += n_arg;
            }
            if b_use_subtype {
                n_extra += n_arg;
            }
            let mut ki = sqlite3_key_info_from_expr_list(p, ob_list, 0, n_extra);
            if !b_ob_unique && p.n_err == 0 {
                ki.n_key_field += 1;
            }
            let n_ob = p.l(ob_list).n_expr();
            p.v.add_op4(OP_OpenEphemeral, i_ob_tab, n_ob + n_extra, 0, P4::KeyInfo(Arc::new(ki)));
            if p.explain == 2 {
                let name = p.agg(agg).a_func[i].p_func.as_ref().map(|f| f.name.to_string()).unwrap_or_default();
                crate::build::sqlite3_vdbe_explain(p, false, format!("USE TEMP B-TREE FOR {name}(ORDER BY)"));
            }
        }
    }
}

/// `finalizeAggFunctions(pParse, pAggInfo)`: invoke the OP_AggFinalize opcode for every
/// aggregate function in the AggInfo structure.
fn finalize_agg_functions(p: &mut Parse, agg: AggInfoId) {
    for i in 0..p.agg(agg).a_func.len() {
        if p.n_err != 0 {
            return;
        }
        let f = p.agg(agg).a_func[i].clone();
        let list = p.e(f.p_f_expr).x_list();
        if f.i_ob_tab >= 0 {
            // For an ORDER BY aggregate, calls to OP_AggStep were deferred. Inputs were
            // stored in pFunc->iOBTab. Run through the sorted list now and invoke the
            // OP_AggStep for each one.
            let n_arg = p.l(list).n_expr();
            let reg_agg = sqlite3_get_temp_range(p, n_arg);
            let n_key = if !f.b_ob_payload {
                0
            } else {
                let mut k = p.l(p.e(p.e(f.p_f_expr).p_left).x_list()).n_expr();
                if !f.b_ob_unique {
                    k += 1;
                }
                k
            };
            let i_top = p.v.add_op1(OP_Rewind, f.i_ob_tab);
            for j in (0..n_arg).rev() {
                p.v.add_op3(OP_Column, f.i_ob_tab, n_key + j, reg_agg + j);
            }
            if f.b_use_subtype {
                let reg_subtype = sqlite3_get_temp_reg(p);
                let i_base_col = n_key + n_arg + (!f.b_ob_payload && !f.b_ob_unique) as i32;
                for j in (0..n_arg).rev() {
                    p.v.add_op3(OP_Column, f.i_ob_tab, i_base_col + j, reg_subtype);
                    p.v.add_op2(OP_SetSubtype, reg_subtype, reg_agg + j);
                }
                sqlite3_release_temp_reg(p, reg_subtype);
            }
            let reg = p.agg(agg).func_reg(i as i32);
            p.v.add_op3(OP_AggStep, 0, reg_agg, reg);
            p.v.append_p4(P4::FuncDef(f.p_func.clone().expect("aggregate function")));
            p.v.change_p5(n_arg as u16);
            p.v.add_op2(OP_Next, f.i_ob_tab, i_top + 1);
            p.v.jump_here(i_top);
            sqlite3_release_temp_range(p, reg_agg, n_arg);
        }
        let reg = p.agg(agg).func_reg(i as i32);
        let n = if list.nn() { p.l(list).n_expr() } else { 0 };
        p.v.add_op2(OP_AggFinal, reg, n);
        p.v.append_p4(P4::FuncDef(f.p_func.clone().expect("aggregate function")));
    }
}

/// `updateAccumulator(pParse, regAcc, pAggInfo, eDistinctType)`: generate code that will
/// update the accumulator memory cells for an aggregate based on the current cursor
/// position.
///
/// If `reg_acc` is non-zero and there are no min() or max() aggregates in pAggInfo, then only
/// populate the pAggInfo->nAccumulator accumulator registers if register `reg_acc` contains
/// 0. The caller will take care of setting it to 1 before this routine returns.
fn update_accumulator(p: &mut Parse, reg_acc: i32, agg: AggInfoId, e_distinct_type: i32) {
    let mut reg_hit = 0;
    let mut addr_hit_test = 0;
    if p.n_err != 0 {
        return;
    }
    p.aggm(agg).direct_mode = 1;
    for i in 0..p.agg(agg).a_func.len() {
        let f = p.agg(agg).a_func[i].clone();
        let mut addr_next = 0;
        let (n_arg, reg_agg);
        let mut reg_agg_sz = 0;
        let mut reg_distinct = 0;
        let list = p.e(f.p_f_expr).x_list();
        if p.e(f.p_f_expr).has(EP_WinFunc) {
            let filter = p.a_win[p.e(f.p_f_expr).y_win.ix()].p_filter;
            if p.agg(agg).n_accumulator != 0
                && f.p_func.as_ref().is_some_and(|d| (d.func_flags & SQLITE_FUNC_NEEDCOLL) != 0)
                && reg_acc != 0
            {
                // If regAcc==0, there there exists some min() or max() function without a
                // FILTER clause that will ensure the magnet registers are populated.
                if reg_hit == 0 {
                    p.n_mem += 1;
                    reg_hit = p.n_mem;
                }
                // If this is the first row of the group (regAcc contains 0), clear the
                // "magnet" register regHit so that the accumulator registers are populated if
                // the FILTER clause jumps over the the invocation of min() or max()
                // altogether. Or, if this is not the first row (regAcc contains 1), set the
                // magnet register so that the accumulators are not populated unless the
                // min()/max() is invoked and indicates that they should be.
                p.v.add_op2(OP_Copy, reg_acc, reg_hit);
            }
            addr_next = p.v.make_label();
            sqlite3_expr_if_false(p, filter, addr_next, SQLITE_JUMPIFNULL);
        }
        if f.i_ob_tab >= 0 {
            // Instead of invoking AggStep, we must push the arguments and inputs onto the
            // sorter and defer the AggStep until finalize.
            n_arg = p.l(list).n_expr();
            let ob_list = p.e(p.e(f.p_f_expr).p_left).x_list();
            reg_agg_sz = p.l(ob_list).n_expr();
            if !f.b_ob_unique {
                reg_agg_sz += 1; // One register for OP_Sequence
            }
            if f.b_ob_payload {
                reg_agg_sz += n_arg;
            }
            if f.b_use_subtype {
                reg_agg_sz += n_arg;
            }
            reg_agg_sz += 1; // One extra register to hold result of MakeRecord
            reg_agg = sqlite3_get_temp_range(p, reg_agg_sz);
            reg_distinct = reg_agg;
            sqlite3_expr_code_expr_list(p, ob_list, reg_agg, 0, SQLITE_ECEL_DUP as u8);
            let mut jj = p.l(ob_list).n_expr();
            if !f.b_ob_unique {
                p.v.add_op2(OP_Sequence, f.i_ob_tab, reg_agg + jj);
                jj += 1;
            }
            if f.b_ob_payload {
                reg_distinct = reg_agg + jj;
                sqlite3_expr_code_expr_list(p, list, reg_distinct, 0, SQLITE_ECEL_DUP as u8);
                jj += n_arg;
            }
            if f.b_use_subtype {
                let reg_base = if f.b_ob_payload { reg_distinct } else { reg_agg };
                for kk in 0..n_arg {
                    p.v.add_op2(OP_GetSubtype, reg_base + kk, reg_agg + jj);
                    jj += 1;
                }
            }
        } else if list.nn() {
            n_arg = p.l(list).n_expr();
            reg_agg = sqlite3_get_temp_range(p, n_arg);
            reg_distinct = reg_agg;
            sqlite3_expr_code_expr_list(p, list, reg_agg, 0, SQLITE_ECEL_DUP as u8);
        } else {
            n_arg = 0;
            reg_agg = 0;
        }
        if f.i_distinct >= 0 && list.nn() {
            if addr_next == 0 {
                addr_next = p.v.make_label();
            }
            let d = code_distinct(p, e_distinct_type, f.i_distinct, addr_next, list, reg_distinct);
            p.aggm(agg).a_func[i].i_distinct = d;
        }
        if f.i_ob_tab >= 0 {
            // Insert a new record into the ORDER BY table
            p.v.add_op3(OP_MakeRecord, reg_agg, reg_agg_sz - 1, reg_agg + reg_agg_sz - 1);
            p.v.add_op4_int(OP_IdxInsert, f.i_ob_tab, reg_agg + reg_agg_sz - 1, reg_agg, reg_agg_sz - 1);
            sqlite3_release_temp_range(p, reg_agg, reg_agg_sz);
        } else {
            // Invoke the AggStep function
            let func = f.p_func.clone().expect("aggregate function");
            if func.func_flags & SQLITE_FUNC_NEEDCOLL != 0 {
                let mut coll = None;
                let mut j = 0;
                while coll.is_none() && j < n_arg {
                    let e = p.l(list).a[j as usize].p_expr;
                    coll = sqlite3_expr_coll_seq(p, e);
                    j += 1;
                }
                let coll = match coll {
                    Some(c) => c,
                    None => dflt_coll(p),
                };
                if reg_hit == 0 && p.agg(agg).n_accumulator != 0 {
                    p.n_mem += 1;
                    reg_hit = p.n_mem;
                }
                p.v.add_op4(OP_CollSeq, reg_hit, 0, 0, P4::CollSeq(coll));
            }
            let reg = p.agg(agg).func_reg(i as i32);
            p.v.add_op3(OP_AggStep, 0, reg_agg, reg);
            p.v.append_p4(P4::FuncDef(func));
            p.v.change_p5(n_arg as u16);
            sqlite3_release_temp_range(p, reg_agg, n_arg);
        }
        if addr_next != 0 {
            p.v.resolve_label(addr_next);
        }
        if p.n_err != 0 {
            return;
        }
    }
    if reg_hit == 0 && p.agg(agg).n_accumulator != 0 {
        reg_hit = reg_acc;
    }
    if reg_hit != 0 {
        addr_hit_test = p.v.add_op1(OP_If, reg_hit);
    }
    for i in 0..p.agg(agg).n_accumulator {
        let e = p.agg(agg).a_col[i as usize].p_c_expr;
        let reg = p.agg(agg).column_reg(i);
        sqlite3_expr_code(p, e, reg);
        if p.n_err != 0 {
            return;
        }
    }
    p.aggm(agg).direct_mode = 0;
    if addr_hit_test != 0 {
        p.v.jump_here_or_pop_inst(addr_hit_test);
    }
}

/// `havingToWhere(pParse, p)`: transfer eligible terms from the HAVING clause of a query,
/// which is assumed to be an aggregate with a GROUP BY, to the WHERE clause. A term of the
/// HAVING expression is eligible for transfer if it consists entirely of constants and
/// expressions that are also GROUP BY terms that use the "BINARY" collation sequence. Each
/// transferred term is replaced in the HAVING clause by the constant 1.
fn having_to_where(p: &mut Parse, sel: SelectId) {
    fn having_to_where_expr_cb(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
        if p.e(e).op != TK_AND {
            let s = w.p_select;
            // This routine is called before the HAVING clause of the current SELECT is
            // analyzed for aggregates. So if pExpr->pAggInfo is set here, it must be an
            // aggregate that belongs to an outer query.
            let gb = p.s(s).p_group_by;
            if sqlite3_expr_is_constant_or_group_by(p, e, gb) != 0 && !p.e(e).always_false() && p.e(e).p_agg_info.is_null() {
                let new = sqlite3_expr_int32(p, 1);
                let p_where = p.s(s).p_where;
                // SWAP(Expr, *pNew, *pExpr)
                p.a_expr.swap(new.ix(), e.ix());
                let n = sqlite3_expr_and(p, p_where, new);
                p.sm(s).p_where = n;
                w.e_code = 1;
            }
            return WRC_Prune;
        }
        WRC_Continue
    }
    let mut w = Walker { x_expr_callback: Some(having_to_where_expr_cb), p_select: sel, ..Walker::default() };
    let h = p.s(sel).p_having;
    sqlite3_walk_expr(p, &mut w, h);
}

/// `isSimpleCount(p, pAggInfo)`: the select statement passed as the first argument is an
/// aggregate query. The second argument is the associated aggregate-info object. This
/// function tests if the SELECT is of the form:
///
///   SELECT count(*) FROM <tbl>
///
/// where table is a database table, not a sub-select or view. If the query does match this
/// pattern, then a pointer to the Table object representing <tbl> is returned. Otherwise,
/// NULL is returned.
///
/// This routine checks to see if it is safe to use the count optimization. A correct answer
/// is still obtained (though perhaps more slowly) if this routine returns NULL when it could
/// have returned a table pointer. But returning the pointer when NULL should have been
/// returned can result in incorrect answers and/or crashes. So, when in doubt, return NULL.
fn is_simple_count(p: &Parse, sel: SelectId, agg: AggInfoId) -> TableId {
    let s = p.s(sel);
    if s.p_where.nn()
        || p.l(s.p_e_list).n_expr() != 1
        || p.src(s.p_src).a.len() != 1
        || p.src(s.p_src).a[0].is_subquery
        || p.agg(agg).n_func() != 1
        || s.p_having.nn()
    {
        return TableId::NULL;
    }
    let tab = p.src(s.p_src).a[0].p_s_tab;
    if !p.tab(tab).is_ordinary() {
        return TableId::NULL;
    }
    let e = p.l(s.p_e_list).a[0].p_expr;
    if p.e(e).op != TK_AGG_FUNCTION {
        return TableId::NULL;
    }
    if p.e(e).p_agg_info != agg {
        return TableId::NULL;
    }
    let f = p.agg(agg).a_func[0].p_func.as_ref().expect("aggregate function");
    if (f.func_flags & SQLITE_FUNC_COUNT) == 0 {
        return TableId::NULL;
    }
    if p.e(e).has(EP_Distinct | EP_WinFunc) {
        return TableId::NULL;
    }
    tab
}

/// `explainSimpleCount(pParse, pTab, pIdx)`: generate code that will tell the user about
/// the simple count(*) optimization: "SCAN <tbl> [USING COVERING INDEX <idx>]".
fn explain_simple_count(p: &mut Parse, tab: TableId, idx: IndexId) {
    if p.explain == 2 {
        let b_cover = idx.nn() && (p.tab(tab).has_rowid() || !p.idx(idx).is_primary_key());
        let msg = format!(
            "SCAN {}{}{}",
            p.tab(tab).z_name,
            if b_cover { " USING COVERING INDEX " } else { "" },
            if b_cover { p.idx(idx).z_name.as_str() } else { "" }
        );
        crate::build::sqlite3_vdbe_explain(p, false, msg);
    }
}

/// `isSelfJoinView(pTabList, pThis, iFirst, iEnd)`: check to see if the pThis entry of
/// pTabList is a self-join of another view. Search FROM-clause entries in the range of
/// iFirst..iEnd, including iFirst but excluding iEnd.
///
/// If pThis is a self-join, then return the SrcItem for the first other instance of that
/// view found. If pThis is not a self-join then return 0.
fn is_self_join_view(p: &Parse, tab_list: SrcListId, i_this: usize, mut i_first: usize, i_end: usize) -> Option<usize> {
    let this = &p.src(tab_list).a[i_this];
    let sel = this.p_subq.as_ref().expect("subquery").p_select;
    if p.s(sel).sel_flags & SF_PushDown != 0 {
        return None;
    }
    while i_first < i_end {
        let k = i_first;
        i_first += 1;
        let item = &p.src(tab_list).a[k];
        if !item.is_subquery {
            continue;
        }
        if item.via_coroutine {
            continue;
        }
        if item.z_name.is_none() {
            continue;
        }
        if p.tab(item.p_s_tab).i_db != p.tab(this.p_s_tab).i_db {
            continue;
        }
        if !item.z_name.as_deref().unwrap_or("").eq_ignore_ascii_case(this.z_name.as_deref().unwrap_or("")) {
            continue;
        }
        let s1 = item.p_subq.as_ref().expect("subquery").p_select;
        if p.tab(item.p_s_tab).i_db < 0 && p.s(sel).sel_id != p.s(s1).sel_id {
            // The query flattener left two different CTE tables with identical names in the
            // same FROM clause.
            continue;
        }
        if p.s(s1).sel_flags & SF_PushDown != 0 {
            // The view was modified by some other optimization such as pushDownWhereTerms()
            continue;
        }
        return Some(k);
    }
    None
}

/// `fromClauseTermCanBeCoroutine(pParse, pTabList, i, selFlags)`: return TRUE (non-zero) if
/// the i-th entry in the pTabList SrcList can be implemented as a co-routine. The i-th entry
/// is guaranteed to be a subquery.
///
/// The subquery is implemented as a co-routine if all of the following are true:
///
///    (1)  The subquery will likely be implemented in the outer loop of the query. This is
///         the case if any one of the following conditions hold:
///         (a)  The subquery is the only term in the FROM clause
///         (b)  The subquery is the left-most term and a CROSS JOIN or similar requires it
///              to be the outer loop
///         (c)  All of the following are true:
///                (i) The subquery is the left-most subquery in the FROM clause
///               (ii) There is nothing that would prevent the subquery from being used as
///                    the outer loop if the sqlite3WhereBegin() routine implements the query
///                    plan that is optimal.
///              (iii) The query is not a UPDATE ... FROM
///    (2)  The subquery is not a CTE that should be materialized because
///         (a) the AS MATERIALIZED keyword is used, or
///         (b) the CTE is used multiple times and does not have the NOT MATERIALIZED keyword
///    (3)  The subquery is not part of a left operand for a RIGHT JOIN
///    (4)  The SQLITE_Coroutine optimization disable flag is not set
///    (5)  The subquery is not self-joined
fn from_clause_term_can_be_coroutine(p: &Parse, tab_list: SrcListId, i: usize, sel_flags: u32) -> bool {
    let item = &p.src(tab_list).a[i];
    if item.is_cte {
        let cte_use = &p.a_cte_use[item.p_cte_use as usize - 1];
        if cte_use.e_m10d as i32 == M10d_Yes {
            return false; // (2a)
        }
        if cte_use.n_use >= 2 && cte_use.e_m10d as i32 != M10d_No {
            return false; // (2b)
        }
    }
    if p.src(tab_list).a[0].jointype as i32 & JT_LTORJ != 0 {
        return false; // (3)
    }
    if p.db.optimization_disabled(SQLITE_Coroutines) {
        return false; // (4)
    }
    if is_self_join_view(p, tab_list, i, i + 1, p.src(tab_list).a.len()).is_some() {
        return false; // (5)
    }
    if i == 0 {
        if p.src(tab_list).a.len() == 1 {
            return true; // (1a)
        }
        if p.src(tab_list).a[1].jointype as i32 & JT_CROSS != 0 {
            return true; // (1b)
        }
        if sel_flags & SF_UpdateFrom != 0 {
            return false; // (1c-iii)
        }
        return true;
    }
    if sel_flags & SF_UpdateFrom != 0 {
        return false; // (1c-iii)
    }
    let mut k = i;
    loop {
        if p.src(tab_list).a[k].jointype as i32 & (JT_OUTER | JT_CROSS) != 0 {
            return false; // (1c-ii)
        }
        if k == 0 {
            break;
        }
        k -= 1;
        if p.src(tab_list).a[k].is_subquery {
            return false; // (1c-i)
        }
    }
    true
}

/// `sqlite3Select(pParse, p, pDest)`: generate byte-code for the SELECT statement given in
/// the p argument.
///
/// The results are returned according to the SelectDest structure. See comments in
/// sqliteInt.h for additional information.
///
/// This routine returns the number of errors. If any errors are encountered, then an
/// appropriate error message is left in pParse->zErrMsg.
///
/// This routine does NOT free the Select structure passed in. The calling function needs to
/// do that.
///
/// This routine is called by the parser and by other parts of the code generator. Window
/// functions, GROUP BY over a FROM clause and recursive common table expressions are not
/// ported yet and are reported as unsupported.
pub fn sqlite3_select(p: &mut Parse, sel: SelectId, dest: &mut SelectDest) -> i32 {
    sqlite3_get_vdbe(p);
    if sel.is_null() || p.n_err != 0 {
        return 1;
    }
    // (sqlite3AuthCheck(pParse, SQLITE_SELECT, 0, 0, 0): the engine authorizes)
    let mut b_pop = true;
    let rc = sqlite3_select_body(p, sel, dest, &mut b_pop);
    // select_end:
    if b_pop {
        crate::build::sqlite3_vdbe_explain_pop(p);
    }
    rc
}

/// The body of `sqlite3Select()` between its entry checks and the `select_end:` label
/// (`ExplainQueryPlanPop(pParse)`); the compound case returns before the label.
fn sqlite3_select_body(p: &mut Parse, sel: SelectId, dest: &mut SelectDest, b_pop: &mut bool) -> i32 {
    let mut rc = 1;
    // tag-select-0100
    // IgnorableDistinct(pDest)
    if dest.e_dest as i32 <= SRT_DistQueue {
        // All of these destinations are also able to ignore the ORDER BY clause
        if p.s(sel).p_order_by.nn() {
            p.sm(sel).p_order_by = ExprListId::NULL;
        }
        p.sm(sel).sel_flags &= !SF_Distinct;
    }
    sqlite3_select_prep(p, sel, NcId::NULL);
    if p.n_err != 0 {
        return rc;
    }
    if dest.e_dest as i32 == SRT_Output {
        crate::treedump::v1_dump_select(p, "R", sel);
    }

    // If the SF_UFSrcCheck flag is set, then this function is being called as part of
    // populating the temp table for an UPDATE...FROM statement.
    if p.s(sel).sel_flags & SF_UFSrcCheck != 0 {
        p.not_supported("UPDATE ... FROM");
        return rc;
    }

    if dest.e_dest as i32 == SRT_Output {
        sqlite3_generate_column_names(p, sel);
    }

    if p.s(sel).p_win.nn() {
        // sqlite3WindowRewrite(pParse, p)
        p.not_supported("window function");
        return rc;
    }
    let mut tab_list = p.s(sel).p_src;
    let mut is_agg = (p.s(sel).sel_flags & SF_Aggregate) != 0;
    let mut s_sort = SortCtx { p_order_by: p.s(sel).p_order_by, ..SortCtx::default() };

    // Try to do various optimizations (flattening subqueries, and strength reduction of join
    // operators) in the FROM clause up into the main query. tag-select-0200
    let mut i: i32 = 0;
    while p.s(sel).p_prior.is_null() && (i as usize) < p.src(tab_list).a.len() {
        let iu = i as usize;
        let (sub, tab, jointype, i_cursor) = {
            let it = &p.src(tab_list).a[iu];
            (if it.is_subquery { it.p_subq.as_ref().map(|q| q.p_select) } else { None }, it.p_s_tab, it.jointype as i32, it.i_cursor)
        };

        // Try to simplify joins:
        //
        //      LEFT JOIN  ->  JOIN
        //     RIGHT JOIN  ->  JOIN
        //      FULL JOIN  ->  RIGHT JOIN
        //
        // If terms of the i-th table are used in the WHERE clause in such a way that the
        // i-th table cannot be the NULL row of a join, then perform the appropriate
        // simplification. This is called "OUTER JOIN strength reduction" in the SQLite
        // documentation. tag-select-0220
        if (jointype & (JT_LEFT | JT_LTORJ)) != 0
            && {
                let wh = p.s(sel).p_where;
                sqlite3_expr_implies_non_null_row(p, wh, i_cursor, (jointype & JT_LTORJ) != 0)
            }
            && p.db.optimization_enabled(SQLITE_SimplifyJoin)
        {
            if jointype & JT_LEFT != 0 {
                if jointype & JT_RIGHT != 0 {
                    // FULL-JOIN simplifies to RIGHT-JOIN
                    p.srcm(tab_list).a[iu].jointype &= !(JT_LEFT as u8);
                } else {
                    // LEFT-JOIN simplifies to JOIN
                    p.srcm(tab_list).a[iu].jointype &= !((JT_LEFT | JT_OUTER) as u8);
                    let wh = p.s(sel).p_where;
                    unset_join_expr(p, wh, i_cursor, false);
                }
            }
            if jointype & JT_LTORJ != 0 {
                for j in iu + 1..p.src(tab_list).a.len() {
                    let (jt2, cur2) = (p.src(tab_list).a[j].jointype as i32, p.src(tab_list).a[j].i_cursor);
                    if jt2 & JT_RIGHT != 0 {
                        if jt2 & JT_LEFT != 0 {
                            // FULL-JOIN simplifies to LEFT-JOIN
                            p.srcm(tab_list).a[j].jointype &= !(JT_RIGHT as u8);
                        } else {
                            // RIGHT-JOIN simplifies to JOIN
                            p.srcm(tab_list).a[j].jointype &= !((JT_RIGHT | JT_OUTER) as u8);
                            let wh = p.s(sel).p_where;
                            unset_join_expr(p, wh, cur2, true);
                        }
                    }
                }
                for j in (0..p.src(tab_list).a.len()).rev() {
                    p.srcm(tab_list).a[j].jointype &= !(JT_LTORJ as u8);
                    if p.src(tab_list).a[j].jointype as i32 & JT_RIGHT != 0 {
                        break;
                    }
                }
            }
        }

        // No further action if this term of the FROM clause is not a subquery
        let Some(sub) = sub else {
            i += 1;
            continue;
        };

        // Catch mismatch in the declared columns of a view and the number of columns in the
        // SELECT on the RHS
        if p.tab(tab).a_col.len() != p.l(p.s(sub).p_e_list).a.len() {
            let msg = format!(
                "expected {} columns for '{}' but got {}",
                p.tab(tab).a_col.len(),
                p.tab(tab).z_name,
                p.l(p.s(sub).p_e_list).a.len()
            );
            p.error_msg(msg);
            return rc;
        }

        // Do not attempt the usual optimizations (flattening and ORDER BY elimination) on a
        // MATERIALIZED common table expression because a MATERIALIZED common table
        // expression is an optimization fence.
        if p.src(tab_list).a[iu].is_cte && p.a_cte_use[p.src(tab_list).a[iu].p_cte_use as usize - 1].e_m10d as i32 == M10d_Yes {
            i += 1;
            continue;
        }

        // Do not try to flatten an aggregate subquery.
        //
        // Flattening an aggregate subquery is only possible if the outer query is not a
        // join. But if the outer query is not a join, then the subquery will be implemented
        // as a co-routine and there is no advantage to flattening in that case.
        if (p.s(sub).sel_flags & SF_Aggregate) != 0 {
            i += 1;
            continue;
        }

        // tag-select-0230: If a FROM-clause subquery has an ORDER BY clause that is not
        // really doing anything, then delete it now so that it does not interfere with query
        // flattening. See the discussion at
        // https://sqlite.org/forum/forumpost/2d76f2bcf65d256a
        //
        // Beware of these cases where the ORDER BY clause may not be safely omitted:
        //
        //    (1)   There is also a LIMIT clause
        //    (2)   The subquery was added to help with window-function processing
        //    (3)   The subquery is in the FROM clause of an UPDATE
        //    (4)   The outer query uses an aggregate function other than the built-in
        //          count(), min(), or max().
        //    (5)   The ORDER BY isn't going to accomplish anything because one of:
        //            (a)  The outer query has a different ORDER BY clause
        //            (b)  The subquery is part of a join
        //          See forum post 062d576715d277c8
        //    (6)   The subquery is not a recursive CTE. ORDER BY has a different meaning for
        //          recursive CTEs and this optimization does not apply.
        //
        // Also retain the ORDER BY if the OmitOrderBy optimization is disabled.
        if p.s(sub).p_order_by.nn()
            && (p.s(sel).p_order_by.nn() || p.src(tab_list).a.len() > 1) // Condition (5)
            && p.s(sub).p_limit.is_null() // Condition (1)
            && (p.s(sub).sel_flags & (SF_OrderByReqd | SF_Recursive)) == 0 // (2) and (6)
            && (p.s(sel).sel_flags & SF_OrderByReqd) == 0 // Condition (3) and (4)
            && p.db.optimization_enabled(SQLITE_OmitOrderBy)
        {
            p.sm(sub).p_order_by = ExprListId::NULL;
        }

        // If the outer query contains a "complex" result set (that is, if the result set of
        // the outer query uses functions or subqueries) and if the subquery contains an
        // ORDER BY clause and if it will be implemented as a co-routine, then do not
        // flatten. This restriction allows SQL constructs like this:
        //
        //  SELECT expensive_function(x)
        //    FROM (SELECT x FROM tab ORDER BY y LIMIT 10);
        //
        // The expensive_function() is only computed on the 10 rows that are output, rather
        // than every row of the table.
        //
        // The requirement that the outer query have a complex result set means that
        // flattening does occur on simpler SQL constraints without the expensive_function()
        // like:
        //
        //  SELECT x FROM (SELECT x FROM tab ORDER BY y LIMIT 10);
        if p.s(sub).p_order_by.nn()
            && i == 0
            && (p.s(sel).sel_flags & SF_ComplexResult) != 0
            && (p.src(tab_list).a.len() == 1 || (p.src(tab_list).a[1].jointype as i32 & (JT_OUTER | JT_CROSS)) != 0)
        {
            i += 1;
            continue;
        }

        // tag-select-0240
        if crate::flatten::flatten_subquery(p, sel, iu, is_agg) {
            if p.n_err != 0 {
                return rc;
            }
            // This subquery can be absorbed into its parent.
            i = -1;
        }
        if p.n_err != 0 {
            return rc;
        }
        tab_list = p.s(sel).p_src;
        // if( !IgnorableOrderby(pDest) )
        if dest.e_dest as i32 > SRT_DistQueue {
            s_sort.p_order_by = p.s(sel).p_order_by;
        }
        i += 1;
    }

    // Handle compound SELECT statements using the separate multiSelect() procedure.
    // tag-select-0300
    if p.s(sel).p_prior.nn() {
        rc = multi_select(p, sel, dest);
        if p.s(sel).p_next.is_null() {
            crate::build::sqlite3_vdbe_explain_pop(p);
        }
        // (the compound case returns without passing through select_end)
        *b_pop = false;
        return rc;
    }

    // If there may be an "EXISTS (SELECT ...)" in the WHERE clause, attempt to change it
    // into a join.
    if p.b_has_exists && p.db.optimization_enabled(SQLITE_ExistsToJoin) {
        let wh = p.s(sel).p_where;
        crate::flatten::exists_to_join(p, sel, wh);
        tab_list = p.s(sel).p_src;
    }

    // Do the WHERE-clause constant propagation optimization if this is a join. No need to
    // spend time on this operation for non-join queries as the equivalent optimization will
    // be handled by query planner in sqlite3WhereBegin(). tag-select-0330
    if p.s(sel).p_where.nn() && p.e(p.s(sel).p_where).op == TK_AND && p.db.optimization_enabled(SQLITE_PropagateConst) {
        crate::flatten::propagate_constants(p, sel);
    }
    if p.n_err != 0 {
        return rc;
    }

    // tag-select-0350
    if p.db.optimization_enabled(SQLITE_QueryFlattener | SQLITE_CountOfView) && crate::flatten::count_of_view_optimization(p, sel) {
        tab_list = p.s(sel).p_src;
    }
    if p.n_err != 0 {
        return rc;
    }

    // Loop over all terms in the FROM clause and do two things for each term:
    //
    //   (1) Authorize unreferenced tables
    //   (2) Generate code for all sub-queries
    //
    // tag-select-0400
    for i in 0..p.src(tab_list).a.len() {
        // (Authorize unreferenced tables: sqlite3AuthCheck(pParse, SQLITE_READ, ...): the
        // engine authorizes. tag-select-0410)

        // Generate code for all sub-queries in the FROM clause
        if !p.src(tab_list).a[i].is_subquery {
            continue;
        }
        let (sub, addr_fill_sub) = {
            let q = p.src(tab_list).a[i].p_subq.as_ref().expect("subquery");
            (q.p_select, q.addr_fill_sub)
        };

        // The code for a subquery should only be generated once.
        if addr_fill_sub != 0 {
            continue;
        }

        // Increment Parse.nHeight by the height of the largest expression tree referred to
        // by this, the parent select. The child select may contain expression trees of at
        // most (SQLITE_MAX_EXPR_DEPTH-Parse.nHeight) height. This is a bit more
        // conservative than necessary, but much easier than enforcing an exact limit.
        p.n_height += sqlite3_select_expr_height(p, sel);

        // Make copies of constant WHERE-clause terms in the outer query down inside the
        // subquery. This can help the subquery to run more efficiently. This is the
        // "predicate push-down optimization". tag-select-0420
        if p.db.optimization_enabled(SQLITE_PushDown) && {
            let it = &p.src(tab_list).a[i];
            !it.is_cte || {
                let u = &p.a_cte_use[it.p_cte_use as usize - 1];
                u.e_m10d as i32 != M10d_Yes && u.n_use < 2
            }
        } {
            let wh = p.s(sel).p_where;
            crate::flatten::push_down_where_terms(p, sub, wh, tab_list, i);
        }
        if p.n_err != 0 {
            return rc;
        }

        // Convert unused result columns of the subquery into simple NULL expressions, to
        // avoid unneeded searching and computation. tag-select-0440
        if p.db.optimization_enabled(SQLITE_NullUnusedCols) {
            crate::flatten::disable_unused_subquery_result_columns(p, tab_list, i);
        }
        if p.n_err != 0 {
            return rc;
        }

        // Generate byte-code to implement the subquery tag-select-0480
        if from_clause_term_can_be_coroutine(p, tab_list, i, p.s(sel).sel_flags) {
            // Implement a co-routine that will return a single row of the result set on each
            // invocation. tag-select-0482
            let addr_top = p.v.current_addr() + 1;
            p.n_mem += 1;
            let reg_return = p.n_mem;
            p.v.add_op3(OP_InitCoroutine, reg_return, 0, addr_top);
            {
                let q = p.srcm(tab_list).a[i].p_subq.as_mut().expect("subquery");
                q.reg_return = reg_return;
                q.addr_fill_sub = addr_top;
            }
            let mut d = sqlite3_select_dest_init(SRT_Coroutine, reg_return);
            let name = crate::wherecode::src_item_name(p, tab_list, i, true);
            crate::build::sqlite3_vdbe_explain(p, true, format!("CO-ROUTINE {name}"));
            sqlite3_select(p, sub, &mut d);
            let n_row = p.s(sub).n_select_row;
            let stab = p.src(tab_list).a[i].p_s_tab;
            p.tabm(stab).n_row_log_est = n_row;
            {
                let it = &mut p.srcm(tab_list).a[i];
                it.via_coroutine = true;
                it.p_subq.as_mut().expect("subquery").reg_result = d.i_sdst;
            }
            p.v.end_coroutine(reg_return);
            p.v.jump_here(addr_top - 1);
            sqlite3_clear_temp_reg_cache(p);
        } else if p.src(tab_list).a[i].is_cte && p.a_cte_use[p.src(tab_list).a[i].p_cte_use as usize - 1].addr_m9e > 0 {
            // This is a CTE for which materialization code has already been generated.
            // Invoke the subroutine to compute the materialization, then make the
            // pItem->iCursor be a copy of the ephemeral table that holds the result of the
            // materialization. tag-select-0484
            let cu = p.a_cte_use[p.src(tab_list).a[i].p_cte_use as usize - 1].clone();
            let i_cursor = p.src(tab_list).a[i].i_cursor;
            p.v.add_op2(OP_Gosub, cu.reg_rtn, cu.addr_m9e);
            if i_cursor != cu.i_cur {
                p.v.add_op2(OP_OpenDup, i_cursor, cu.i_cur);
            }
            p.sm(sub).n_select_row = cu.n_row_est;
        } else if let Some(prior) = is_self_join_view(p, tab_list, i, 0, i) {
            // This view has already been materialized by a prior entry in this same FROM
            // clause. Reuse it. tag-select-0486
            let (pr_reg_return, pr_addr_fill_sub, pr_select, pr_cursor) = {
                let it = &p.src(tab_list).a[prior];
                let q = it.p_subq.as_ref().expect("subquery");
                (q.reg_return, q.addr_fill_sub, q.p_select, it.i_cursor)
            };
            if pr_addr_fill_sub != 0 {
                p.v.add_op2(OP_Gosub, pr_reg_return, pr_addr_fill_sub);
            }
            let i_cursor = p.src(tab_list).a[i].i_cursor;
            p.v.add_op2(OP_OpenDup, i_cursor, pr_cursor);
            let n = p.s(pr_select).n_select_row;
            p.sm(sub).n_select_row = n;
        } else {
            // Materialize the view. If the view is not correlated, generate a subroutine to
            // do the materialization so that subsequent uses of the same view can reuse the
            // materialization. tag-select-0488
            let mut once_addr = 0;
            p.n_mem += 1;
            let reg_return = p.n_mem;
            let top_addr = p.v.add_op0(OP_Goto);
            {
                let it = &mut p.srcm(tab_list).a[i];
                let q = it.p_subq.as_mut().expect("subquery");
                q.reg_return = reg_return;
                q.addr_fill_sub = top_addr + 1;
                it.is_materialized = true;
            }
            if !p.src(tab_list).a[i].is_correlated {
                // If the subquery is not correlated and if we are not inside of a trigger,
                // then we only need to compute the value of the subquery once.
                once_addr = p.v.add_op0(OP_Once);
            }
            let i_cursor = p.src(tab_list).a[i].i_cursor;
            let mut d = sqlite3_select_dest_init(SRT_EphemTab, i_cursor);
            let name = crate::wherecode::src_item_name(p, tab_list, i, true);
            crate::build::sqlite3_vdbe_explain(p, true, format!("MATERIALIZE {name}"));
            sqlite3_select(p, sub, &mut d);
            let n_row = p.s(sub).n_select_row;
            let stab = p.src(tab_list).a[i].p_s_tab;
            p.tabm(stab).n_row_log_est = n_row;
            if once_addr != 0 {
                p.v.jump_here(once_addr);
            }
            p.v.add_op2(OP_Return, reg_return, top_addr + 1);
            p.v.jump_here(top_addr);
            sqlite3_clear_temp_reg_cache(p);
            if p.src(tab_list).a[i].is_cte && !p.src(tab_list).a[i].is_correlated {
                let k = p.src(tab_list).a[i].p_cte_use as usize - 1;
                let cu = &mut p.a_cte_use[k];
                cu.addr_m9e = top_addr + 1;
                cu.reg_rtn = reg_return;
                cu.i_cur = i_cursor;
                cu.n_row_est = n_row;
            }
        }
        if p.n_err != 0 {
            return rc;
        }
        p.n_height -= sqlite3_select_expr_height(p, sel);
    }

    // Various elements of the SELECT copied into local variables for convenience
    let e_list = p.s(sel).p_e_list;
    let mut p_where = p.s(sel).p_where;
    let mut group_by = p.s(sel).p_group_by;
    let having = p.s(sel).p_having;
    let mut s_distinct = DistinctCtx { is_tnct: ((p.s(sel).sel_flags & SF_Distinct) != 0) as u8, ..DistinctCtx::default() };

    // tag-select-0500
    //
    // If the query is DISTINCT with an ORDER BY but is not an aggregate, and if the
    // select-list is the same as the ORDER BY list, then this query can be rewritten as a
    // GROUP BY. In other words, this:
    //
    //     SELECT DISTINCT xyz FROM ... ORDER BY xyz
    //
    // is transformed to:
    //
    //     SELECT xyz FROM ... GROUP BY xyz ORDER BY xyz
    //
    // The second form is preferred as a single index (or temp-table) may be used for both the
    // ORDER BY and DISTINCT processing. As originally written the query must use a
    // temp-table for at least one of the ORDER BY and DISTINCT, and an index or separate
    // temp-table for the other.
    if (p.s(sel).sel_flags & (SF_Distinct | SF_Aggregate)) == SF_Distinct
        && sqlite3_copy_sort_order(p, e_list, s_sort.p_order_by)
        && sqlite3_expr_list_compare(p, e_list, s_sort.p_order_by, -1) == 0
        && p.db.optimization_enabled(SQLITE_GroupByOrder)
        && p.s(sel).p_win.is_null()
    {
        p.sm(sel).sel_flags &= !SF_Distinct;
        group_by = sqlite3_expr_list_dup(p, e_list, 0);
        p.sm(sel).p_group_by = group_by;
        for i in 0..p.l(group_by).a.len() {
            p.lm(group_by).a[i].i_order_by_col = i as u16 + 1;
        }
        p.sm(sel).sel_flags |= SF_Aggregate;
        // Notice that even thought SF_Distinct has been cleared from p->selFlags, the
        // sDistinct.isTnct is still set. Hence, isTnct represents the original setting of
        // the SF_Distinct flag, not the current setting
        s_distinct.is_tnct = 2;
        is_agg = true;
    }

    // If there is an ORDER BY clause, then create an ephemeral index to do the sorting. But
    // this sorting ephemeral index might end up being unused if the data can be extracted in
    // pre-sorted order. If that is the case, then the OP_OpenEphemeral instruction will be
    // changed to an OP_Noop once we figure out that the sorting index is not needed. The
    // sSort.addrSortIndex variable is used to facilitate that change. tag-select-0600
    if s_sort.p_order_by.nn() {
        let n = p.l(e_list).n_expr();
        let ki = sqlite3_key_info_from_expr_list(p, s_sort.p_order_by, 0, n);
        s_sort.i_e_cursor = p.n_tab;
        p.n_tab += 1;
        let n_ob = p.l(s_sort.p_order_by).n_expr();
        s_sort.addr_sort_index = p.v.add_op4(OP_OpenEphemeral, s_sort.i_e_cursor, n_ob + 1 + n, 0, P4::KeyInfo(Arc::new(ki)));
    } else {
        s_sort.addr_sort_index = -1;
    }

    // If the output is destined for a temporary table, open that table. tag-select-0630
    if dest.e_dest as i32 == SRT_EphemTab {
        let n = p.l(e_list).n_expr();
        p.v.add_op2(OP_OpenEphemeral, dest.i_sd_parm, n);
        if p.s(sel).sel_flags & SF_NestedFrom != 0 {
            // Delete or NULL-out result columns that will never be used
            let mut ii = p.l(e_list).a.len() as i32 - 1;
            while ii > 0 && !p.l(e_list).a[ii as usize].b_used {
                p.lm(e_list).a.pop();
                ii -= 1;
            }
            for ii in 0..p.l(e_list).a.len() {
                if !p.l(e_list).a[ii].b_used {
                    let e = p.l(e_list).a[ii].p_expr;
                    p.em(e).op = TK_NULL;
                }
            }
        }
    }

    // Set the limiter. tag-select-0650
    let i_end = p.v.make_label();
    if (p.s(sel).sel_flags & SF_FixedLimit) == 0 {
        p.sm(sel).n_select_row = 320; // 4 billion rows
    }
    if p.s(sel).p_limit.nn() {
        compute_limit_registers(p, sel, i_end);
    }
    if p.s(sel).i_limit == 0 && s_sort.addr_sort_index >= 0 {
        p.v.change_opcode(s_sort.addr_sort_index, OP_SorterOpen);
        s_sort.sort_flags |= SORTFLAG_USE_SORTER;
    }

    // Open an ephemeral index to use for the distinct set. tag-select-0680
    if p.s(sel).sel_flags & SF_Distinct != 0 {
        s_distinct.tab_tnct = p.n_tab;
        p.n_tab += 1;
        let el = p.s(sel).p_e_list;
        let ki = sqlite3_key_info_from_expr_list(p, el, 0, 0);
        s_distinct.addr_tnct = p.v.add_op4(OP_OpenEphemeral, s_distinct.tab_tnct, 0, 0, P4::KeyInfo(Arc::new(ki)));
        p.v.change_p5(BTREE_UNORDERED);
        s_distinct.e_tnct_type = WHERE_DISTINCT_UNORDERED as u8;
    } else {
        s_distinct.e_tnct_type = WHERE_DISTINCT_NOOP as u8;
    }

    if !is_agg && group_by.is_null() {
        // No aggregate functions and no GROUP BY clause. tag-select-0700
        let wctrl_flags: u16 =
            (if s_distinct.is_tnct != 0 { WHERE_WANT_DISTINCT as u16 } else { 0 }) | (p.s(sel).sel_flags & SF_FixedLimit) as u16;

        // Begin the database scan.
        let el = p.s(sel).p_e_list;
        let n_select_row = p.s(sel).n_select_row;
        let Some(winfo) = sqlite3_where_begin(p, tab_list, p_where, s_sort.p_order_by, el, sel, wctrl_flags, n_select_row as i32) else {
            return rc;
        };
        if winfo.output_row_count() < p.s(sel).n_select_row {
            p.sm(sel).n_select_row = winfo.output_row_count();
            if dest.e_dest as i32 <= SRT_DistQueue && dest.e_dest as i32 >= SRT_DistFifo {
                // TUNING: For a UNION CTE, because UNION is implies DISTINCT, reduce the
                // estimated output row count by 8 (LogEst 30). Search for tag-20250414a to
                // see other cases
                p.sm(sel).n_select_row -= 30;
            }
        }
        if s_distinct.is_tnct != 0 && winfo.is_distinct() != 0 {
            s_distinct.e_tnct_type = winfo.is_distinct() as u8;
        }
        if s_sort.p_order_by.nn() {
            s_sort.n_ob_sat = winfo.is_ordered();
            s_sort.label_ob_lopt = sqlite3_where_order_by_limit_opt_label(&winfo);
            if s_sort.n_ob_sat == p.l(s_sort.p_order_by).n_expr() {
                s_sort.p_order_by = ExprListId::NULL;
            }
        }

        // If sorting index that was created by a prior OP_OpenEphemeral instruction ended up
        // not being needed, then change the OP_OpenEphemeral into an OP_Noop.
        if s_sort.addr_sort_index >= 0 && s_sort.p_order_by.is_null() {
            p.v.change_to_noop(s_sort.addr_sort_index);
        }

        // Use the standard inner loop.
        let (i_cont, i_brk) = (winfo.continue_label(), winfo.break_label());
        select_inner_loop(p, sel, -1, Some(&mut s_sort), Some(&s_distinct), dest, i_cont, i_brk);

        // End the database scan loop.
        sqlite3_where_end(p, winfo);
    } else {
        // This case is for when there exist aggregate functions or a GROUP BY clause or both.
        // tag-select-0800
        let mut min_max_order_by = ExprListId::NULL;
        let mut order_by_grp = false; // True if the GROUP BY and ORDER BY are the same

        // Remove any and all aliases between the result set and the GROUP BY clause.
        if group_by.nn() {
            let el = p.s(sel).p_e_list;
            for it in p.lm(el).a.iter_mut() {
                it.i_alias = 0;
            }
            for it in p.lm(group_by).a.iter_mut() {
                it.i_alias = 0;
            }
            if p.s(sel).n_select_row > 66 {
                p.sm(sel).n_select_row = 66;
            }

            // If there is both a GROUP BY and an ORDER BY clause and they are identical, then
            // it may be possible to disable the ORDER BY clause on the grounds that the GROUP
            // BY will cause elements to come out in the correct order. It also may not - the
            // GROUP BY might use a database index that causes rows to be grouped together as
            // required but not actually sorted. Either way, record the fact that the ORDER
            // BY and GROUP BY clauses are the same by setting the orderByGrp variable.
            if sqlite3_copy_sort_order(p, group_by, s_sort.p_order_by)
                && sqlite3_expr_list_compare(p, group_by, s_sort.p_order_by, -1) == 0
            {
                order_by_grp = true;
            }
        } else {
            p.sm(sel).n_select_row = 0;
        }

        // Create a label to jump to when we want to abort the query
        let addr_end = p.v.make_label();

        // Convert TK_COLUMN nodes into TK_AGG_COLUMN and make entries in sAggInfo for all
        // TK_AGG_FUNCTION nodes in expressions of the SELECT statement.
        let agg = p.new_agg(AggInfo { sel_id: p.s(sel).sel_id, ..AggInfo::default() });
        let nc = p.new_nc(NameContext { p_src_list: tab_list, p_agg_info: agg, ..NameContext::default() });
        p.aggm(agg).n_sorting_column = if group_by.nn() { p.l(group_by).n_expr() as u32 } else { 0 };
        p.aggm(agg).p_group_by = group_by;
        sqlite3_expr_analyze_agg_list(p, nc, e_list);
        sqlite3_expr_analyze_agg_list(p, nc, s_sort.p_order_by);
        if having.nn() {
            if group_by.nn() {
                having_to_where(p, sel);
                p_where = p.s(sel).p_where;
            }
            sqlite3_expr_analyze_aggregates(p, nc, having);
        }
        let n_col = p.agg(agg).n_column();
        p.aggm(agg).n_accumulator = n_col;
        let min_max_flag = if p.s(sel).p_group_by.is_null() && p.s(sel).p_having.is_null() && p.agg(agg).n_func() == 1 {
            let fe = p.agg(agg).a_func[0].p_f_expr;
            min_max_query(p, fe, &mut min_max_order_by)
        } else {
            WHERE_ORDERBY_NORMAL as u8
        };
        analyze_agg_func_args(p, agg, nc);
        p.a_nc.truncate(nc.ix());
        if p.n_err != 0 {
            return rc;
        }

        // Processing for aggregates with GROUP BY is very different and much more complex
        // than aggregates without a GROUP BY. tag-select-0810
        if group_by.nn() {
            let mut distinct = ExprListId::NULL;
            let mut dist_flag: u16 = 0;
            let n_group_by = p.l(group_by).n_expr();

            if p.agg(agg).n_func() == 1 && p.agg(agg).a_func[0].i_distinct >= 0 && {
                let fe = p.agg(agg).a_func[0].p_f_expr;
                p.e(fe).x_list().nn()
            } {
                let fe = p.agg(agg).a_func[0].p_f_expr;
                let arg0 = p.l(p.e(fe).x_list()).a[0].p_expr;
                let arg0 = sqlite3_expr_dup(p, arg0, 0);
                distinct = sqlite3_expr_list_dup(p, group_by, 0);
                distinct = sqlite3_expr_list_append(p, distinct, arg0);
                dist_flag = (WHERE_WANT_DISTINCT | WHERE_AGG_DISTINCT) as u16;
            }

            // If there is a GROUP BY clause we might need a sorting index to implement it.
            // Allocate that sorting index now. If it turns out that we do not need it after
            // all, the OP_SorterOpen instruction will be converted into a Noop.
            p.aggm(agg).sorting_idx = p.n_tab;
            p.n_tab += 1;
            let n_column = p.agg(agg).n_column();
            let key_info = Arc::new(sqlite3_key_info_from_expr_list(p, group_by, 0, n_column));
            let (sorting_idx, n_sorting_column) = (p.agg(agg).sorting_idx, p.agg(agg).n_sorting_column as i32);
            let addr_sorting_idx = p.v.add_op4(OP_SorterOpen, sorting_idx, n_sorting_column, 0, P4::KeyInfo(key_info.clone()));

            // Initialize memory locations used by GROUP BY aggregate processing
            p.n_mem += 1;
            let i_use_flag = p.n_mem;
            p.n_mem += 1;
            let i_abort_flag = p.n_mem;
            p.n_mem += 1;
            let reg_output_row = p.n_mem;
            let addr_output_row_label = p.v.make_label();
            p.n_mem += 1;
            let reg_reset = p.n_mem;
            let addr_reset = p.v.make_label();
            let i_a_mem = p.n_mem + 1;
            p.n_mem += n_group_by;
            let i_b_mem = p.n_mem + 1;
            p.n_mem += n_group_by;
            p.v.add_op2(OP_Integer, 0, i_abort_flag);
            p.v.add_op3(OP_Null, 0, i_a_mem, i_a_mem + n_group_by - 1);
            sqlite3_expr_null_register_range(p, i_a_mem, n_group_by);

            // Begin a loop that will extract all source rows in GROUP BY order. This might
            // involve two separate loops with an OP_Sort in between, or it might be a single
            // loop that uses an index to extract information in the right order to begin
            // with.
            p.v.add_op2(OP_Gosub, reg_reset, addr_reset);
            let flags = (if s_distinct.is_tnct == 2 { WHERE_DISTINCTBY } else { WHERE_GROUPBY }) as u16
                | (if order_by_grp { WHERE_SORTBYGROUP as u16 } else { 0 })
                | dist_flag;
            let Some(winfo) = sqlite3_where_begin(p, tab_list, p_where, group_by, distinct, sel, flags, 0) else {
                return rc;
            };
            if !p.p_idx_epr.is_empty() {
                let nc = p.new_nc(NameContext { p_src_list: tab_list, p_agg_info: agg, ..NameContext::default() });
                optimize_aggregate_use_of_indexed_expr(p, sel, agg, nc);
                p.a_nc.truncate(nc.ix());
            }
            assign_aggregate_registers(p, agg);
            let e_dist = winfo.is_distinct();
            let group_by_sort;
            let mut sort_p_tab = 0;
            let mut sort_out = 0;
            let mut winfo = Some(winfo);
            if winfo.as_ref().is_some_and(|w| w.is_ordered() == n_group_by) {
                // The optimizer is able to deliver rows in group by order so we do not have
                // to sort. The OP_OpenEphemeral table will be cancelled later because we
                // still need to use the pKeyInfo
                group_by_sort = false;
            } else {
                // Rows are coming out in undetermined order. We have to push each row into a
                // sorting index, terminate the first loop, then loop over the sorting index
                // in order to get the output in sorted order
                let usage = if s_distinct.is_tnct != 0 && (p.s(sel).sel_flags & SF_Distinct) == 0 { "DISTINCT" } else { "GROUP BY" };
                explain_temp_table(p, usage);

                group_by_sort = true;
                let mut n_col = n_group_by;
                let mut j = n_group_by;
                for i in 0..p.agg(agg).a_col.len() {
                    if p.agg(agg).a_col[i].i_sorter_column >= j {
                        n_col += 1;
                        j += 1;
                    }
                }
                let reg_base = sqlite3_get_temp_range(p, n_col);
                sqlite3_expr_code_expr_list(p, group_by, reg_base, 0, 0);
                j = n_group_by;
                p.aggm(agg).direct_mode = 1;
                for i in 0..p.agg(agg).a_col.len() {
                    let (i_sorter_column, c_expr) = {
                        let c = &p.agg(agg).a_col[i];
                        (c.i_sorter_column, c.p_c_expr)
                    };
                    if i_sorter_column >= j {
                        sqlite3_expr_code(p, c_expr, j + reg_base);
                        j += 1;
                    }
                }
                p.aggm(agg).direct_mode = 0;
                let reg_record = sqlite3_get_temp_reg(p);
                p.v.add_op3(OP_MakeRecord, reg_base, n_col, reg_record);
                p.v.add_op2(OP_SorterInsert, sorting_idx, reg_record);
                sqlite3_release_temp_reg(p, reg_record);
                sqlite3_release_temp_range(p, reg_base, n_col);
                sqlite3_where_end(p, winfo.take().expect("where info"));
                sort_p_tab = p.n_tab;
                p.n_tab += 1;
                p.aggm(agg).sorting_idx_p_tab = sort_p_tab;
                sort_out = sqlite3_get_temp_reg(p);
                p.v.add_op3(OP_OpenPseudo, sort_p_tab, sort_out, n_col);
                p.v.add_op2(OP_SorterSort, sorting_idx, addr_end);
                p.aggm(agg).use_sorting_idx = 1;
            }

            // If there are entries in pAgggInfo->aFunc[] that contain subexpressions that are
            // indexed (and that were previously identified and tagged in
            // optimizeAggregateUseOfIndexedExpr()) then those subexpressions must now be
            // converted into a TK_AGG_COLUMN node so that the value is correctly pulled from
            // the index rather than being recomputed.
            if !p.p_idx_epr.is_empty() {
                aggregate_convert_indexed_expr_ref_to_column(p, agg);
            }

            // If the index or temporary table used by the GROUP BY sort will naturally
            // deliver rows in the order required by the ORDER BY clause, cancel the
            // ephemeral table open coded earlier.
            //
            // This is an optimization - the correct answer should result regardless. Use the
            // SQLITE_GroupByOrder flag with SQLITE_TESTCTRL_OPTIMIZER to disable this
            // optimization for testing purposes.
            dump_agg(p, sel, agg, p_where, "groupby", [group_by_sort as i32, order_by_grp as i32, e_dist, dist_flag as i32]);
            if order_by_grp
                && p.db.optimization_enabled(SQLITE_GroupByOrder)
                && (group_by_sort || winfo.as_ref().is_some_and(|w| w.is_sorted()))
            {
                s_sort.p_order_by = ExprListId::NULL;
                p.v.change_to_noop(s_sort.addr_sort_index);
            }

            // Evaluate the current GROUP BY terms and store in b0, b1, b2... (b0 is memory
            // location iBMem+0, b1 is iBMem+1, and so forth) Then compare the current GROUP
            // BY terms against the GROUP BY terms from the previous row currently stored in
            // a0, a1, a2...
            let addr_top_of_loop = p.v.current_addr();
            if group_by_sort {
                p.v.add_op3(OP_SorterData, sorting_idx, sort_out, sort_p_tab);
            }
            for j in 0..n_group_by {
                let (i_order_by_col, ge) = {
                    let it = &p.l(group_by).a[j as usize];
                    (it.i_order_by_col as i32, it.p_expr)
                };
                if group_by_sort {
                    p.v.add_op3(OP_Column, sort_p_tab, j, i_b_mem + j);
                } else {
                    p.aggm(agg).direct_mode = 1;
                    sqlite3_expr_code(p, ge, i_b_mem + j);
                }
                if i_order_by_col != 0 {
                    let el = p.s(sel).p_e_list;
                    let mut px = p.l(el).a[(i_order_by_col - 1) as usize].p_expr;
                    let mut base = sqlite3_expr_skip_collate_and_likely(p, px);
                    while base.nn() && p.e(base).op == TK_IF_NULL_ROW {
                        px = p.e(base).p_left;
                        base = sqlite3_expr_skip_collate_and_likely(p, px);
                    }
                    if base.nn() && p.e(base).op != TK_AGG_COLUMN && p.e(base).op != TK_REGISTER {
                        sqlite3_expr_to_register(p, px, i_a_mem + j);
                    }
                }
            }
            p.v.add_op4(OP_Compare, i_a_mem, i_b_mem, n_group_by, P4::KeyInfo(key_info));
            let addr1 = p.v.current_addr();
            p.v.add_op3(OP_Jump, addr1 + 1, 0, addr1 + 1);

            // Generate code that runs whenever the GROUP BY changes. Changes in the GROUP BY
            // are detected by the previous code block. If there were no changes, this block
            // is skipped.
            //
            // This code copies current group by terms in b0,b1,b2,... over to a0,a1,a2. It
            // then calls the output subroutine and resets the aggregate accumulator registers
            // in preparation for the next GROUP BY batch.
            p.v.add_op2(OP_Gosub, reg_output_row, addr_output_row_label);
            sqlite3_expr_code_move(p, i_b_mem, i_a_mem, n_group_by);
            p.v.add_op2(OP_IfPos, i_abort_flag, addr_end);
            p.v.add_op2(OP_Gosub, reg_reset, addr_reset);

            // Update the aggregate accumulators based on the content of the current row
            p.v.jump_here(addr1);
            update_accumulator(p, i_use_flag, agg, e_dist);
            p.v.add_op2(OP_Integer, 1, i_use_flag);

            // End of the loop
            if group_by_sort {
                p.v.add_op2(OP_SorterNext, sorting_idx, addr_top_of_loop);
            } else {
                sqlite3_where_end(p, winfo.take().expect("where info"));
                p.v.change_to_noop(addr_sorting_idx);
            }

            // Output the final row of result
            p.v.add_op2(OP_Gosub, reg_output_row, addr_output_row_label);

            // Jump over the subroutines
            p.v.goto(addr_end);

            // Generate a subroutine that outputs a single row of the result set. This
            // subroutine first looks at the iUseFlag. If iUseFlag is less than or equal to
            // zero, the subroutine is a no-op. If the processing calls for the query to
            // abort, this subroutine increments the iAbortFlag memory location before
            // returning in order to signal the caller to abort.
            let addr_set_abort = p.v.current_addr();
            p.v.add_op2(OP_Integer, 1, i_abort_flag);
            p.v.add_op1(OP_Return, reg_output_row);
            p.v.resolve_label(addr_output_row_label);
            let addr_output_row = p.v.current_addr();
            p.v.add_op2(OP_IfPos, i_use_flag, addr_output_row + 2);
            p.v.add_op1(OP_Return, reg_output_row);
            finalize_agg_functions(p, agg);
            sqlite3_expr_if_false(p, having, addr_output_row + 1, SQLITE_JUMPIFNULL);
            select_inner_loop(p, sel, -1, Some(&mut s_sort), Some(&s_distinct), dest, addr_output_row + 1, addr_set_abort);
            p.v.add_op1(OP_Return, reg_output_row);

            // Generate a subroutine that will reset the group-by accumulator
            p.v.resolve_label(addr_reset);
            reset_accumulator(p, agg);
            p.v.add_op2(OP_Integer, 0, i_use_flag);
            p.v.add_op1(OP_Return, reg_reset);

            if dist_flag != 0 && e_dist != WHERE_DISTINCT_NOOP {
                let (i_distinct, i_dist_addr) = (p.agg(agg).a_func[0].i_distinct, p.agg(agg).a_func[0].i_dist_addr);
                fix_distinct_open_eph(p, e_dist, i_distinct, i_dist_addr);
            }
        } else {
            // Aggregate functions without GROUP BY. tag-select-0820
            let tab = is_simple_count(p, sel, agg);
            if tab.nn() {
                // tag-select-0821
                //
                // If isSimpleCount() returns a pointer to a Table structure, then the SQL
                // statement is of the form:
                //
                //   SELECT count(*) FROM <tbl>
                //
                // where the Table structure returned represents table <tbl>.
                //
                // This statement is so common that it is optimized specially. The OP_Count
                // instruction is executed either on the intkey table that contains the data
                // for table <tbl> or on one of its indexes. It is better to execute the op
                // on an index, as indexes are almost always spread across less pages than
                // their corresponding tables.
                let i_db = p.tab(tab).i_db;
                let i_csr = p.n_tab; // Cursor to scan b-tree
                p.n_tab += 1;
                let mut best = IndexId::NULL; // Best index found so far
                let mut i_root = p.tab(tab).tnum; // Root page of scanned b-tree

                crate::build::sqlite3_code_verify_schema(p, i_db);
                let (tnum, name) = (p.tab(tab).tnum, p.tab(tab).z_name.clone());
                crate::schema::sqlite3_table_lock(p, i_db, tnum, 0, &name);

                // Search for the index that has the lowest scan cost.
                //
                // (2011-04-15) Do not do a full scan of an unordered index.
                //
                // (2013-10-03) Do not count the entries in a partial index.
                //
                // In practice the KeyInfo structure will not be used. It is only passed to
                // keep OP_OpenRead happy.
                if !p.tab(tab).has_rowid() {
                    best = crate::schema::sqlite3_primary_key_index(p, tab);
                }
                if !p.src(p.s(sel).p_src).a[0].not_indexed {
                    for &ix in &p.tab(tab).p_index {
                        let x = p.idx(ix);
                        if !x.b_unordered
                            && x.sz_idx_row < p.tab(tab).sz_tab_row
                            && x.p_part_idx_where.is_null()
                            && (best.is_null() || x.sz_idx_row < p.idx(best).sz_idx_row)
                        {
                            best = ix;
                        }
                    }
                }
                let mut key_info = None; // Keyinfo for scanned index
                if best.nn() {
                    i_root = p.idx(best).tnum;
                    key_info = crate::schema::sqlite3_key_info_of_index(p, best);
                }

                // Open a read-only cursor, execute the OP_Count, close the cursor.
                p.v.add_op4_int(OP_OpenRead, i_csr, i_root as i32, i_db, 1);
                if let Some(k) = key_info {
                    p.v.change_p4(-1, P4::KeyInfo(Arc::new(k)));
                }
                assign_aggregate_registers(p, agg);
                let reg = p.agg(agg).func_reg(0);
                dump_agg(p, sel, agg, p_where, "count", [if best.nn() { p.idx(best).tnum as i32 } else { 0 }, i_csr, 0, 0]);
                p.v.add_op2(OP_Count, i_csr, reg);
                p.v.add_op1(OP_Close, i_csr);
                explain_simple_count(p, tab, best);
            } else {
                // The general case of an aggregate query without GROUP BY tag-select-0822
                let mut reg_acc = 0; // "populate accumulators" flag
                let mut distinct = ExprListId::NULL;
                let mut dist_flag: u16 = 0;

                // If there are accumulator registers but no min() or max() functions without
                // FILTER clauses, allocate register regAcc. Register regAcc will contain 0
                // the first time the inner loop runs, and 1 thereafter. The code generated
                // by updateAccumulator() uses this to ensure that the accumulator registers
                // are (a) updated only once if there are no min() or max functions or (b)
                // always updated for the first row visited by the aggregate, so that they
                // are updated at least once even if the FILTER clause means the min() or
                // max() function visits zero rows.
                if p.agg(agg).n_accumulator != 0 {
                    let n = p.agg(agg).a_func.len();
                    let mut i = 0;
                    while i < n {
                        if p.e(p.agg(agg).a_func[i].p_f_expr).has(EP_WinFunc) {
                            i += 1;
                            continue;
                        }
                        let f = p.agg(agg).a_func[i].p_func.clone().expect("aggregate function");
                        if f.func_flags & SQLITE_FUNC_NEEDCOLL != 0 {
                            break;
                        }
                        i += 1;
                    }
                    if i == n {
                        p.n_mem += 1;
                        reg_acc = p.n_mem;
                        p.v.add_op2(OP_Integer, 0, reg_acc);
                    }
                } else if p.agg(agg).n_func() == 1 && p.agg(agg).a_func[0].i_distinct >= 0 {
                    distinct = p.e(p.agg(agg).a_func[0].p_f_expr).x_list();
                    dist_flag = if distinct.nn() { (WHERE_WANT_DISTINCT | WHERE_AGG_DISTINCT) as u16 } else { 0 };
                }
                assign_aggregate_registers(p, agg);

                // This case runs if the aggregate has no GROUP BY clause. The processing is
                // much simpler since there is only a single row of output.
                reset_accumulator(p, agg);

                // If this query is a candidate for the min/max optimization, then minMaxFlag
                // will have been previously set to either WHERE_ORDERBY_MIN or
                // WHERE_ORDERBY_MAX and pMinMaxOrderBy will be an ORDER BY clause for the
                // where.c to use.
                let Some(winfo) =
                    sqlite3_where_begin(p, tab_list, p_where, min_max_order_by, distinct, sel, min_max_flag as u16 | dist_flag, 0)
                else {
                    return rc;
                };
                let e_dist = winfo.is_distinct();
                dump_agg(p, sel, agg, p_where, "plain", [min_max_flag as i32, reg_acc, e_dist, dist_flag as i32]);
                update_accumulator(p, reg_acc, agg, e_dist);
                if e_dist != WHERE_DISTINCT_NOOP && !p.agg(agg).a_func.is_empty() {
                    let (i_distinct, i_dist_addr) = (p.agg(agg).a_func[0].i_distinct, p.agg(agg).a_func[0].i_dist_addr);
                    fix_distinct_open_eph(p, e_dist, i_distinct, i_dist_addr);
                }
                if reg_acc != 0 {
                    p.v.add_op2(OP_Integer, 1, reg_acc);
                }
                if min_max_flag != 0 {
                    sqlite3_where_min_max_opt_early_out(p, &winfo);
                }
                sqlite3_where_end(p, winfo);
                finalize_agg_functions(p, agg);
            }

            s_sort.p_order_by = ExprListId::NULL;
            sqlite3_expr_if_false(p, having, addr_end, SQLITE_JUMPIFNULL);
            select_inner_loop(p, sel, -1, None, None, dest, addr_end, addr_end);
        }
        p.v.resolve_label(addr_end);
    } // endif aggregate query

    if s_distinct.e_tnct_type as i32 == WHERE_DISTINCT_UNORDERED {
        explain_temp_table(p, "DISTINCT");
    }

    // If there is an ORDER BY clause, then we need to sort the results and send them to the
    // callback one by one. tag-select-0900
    if s_sort.p_order_by.nn() {
        let n = p.l(e_list).n_expr();
        generate_sort_tail(p, sel, &mut s_sort, n, dest);
    }

    // Jump here to skip this query
    p.v.resolve_label(i_end);

    // The SELECT has been coded. If there is an error in the Parse structure, set the return
    // code to 1. Otherwise 0.
    rc = (p.n_err > 0) as i32;
    rc
}

/// `sqlite3MultiValuesEnd(pParse, pVal)`: the co-routine of a multi-row VALUES clause ends
/// here (called when the last row of the VALUES clause has been parsed).
pub fn sqlite3_multi_values_end(p: &mut Parse, val: SelectId) {
    if val.nn() && !p.src(p.s(val).p_src).a.is_empty() {
        let (is_subq, reg_return, addr_fill_sub) = {
            let it = &p.src(p.s(val).p_src).a[0];
            let q = it.p_subq.as_ref();
            (it.is_subquery, q.map_or(0, |q| q.reg_return), q.map_or(0, |q| q.addr_fill_sub))
        };
        if is_subq {
            p.v.end_coroutine(reg_return);
            // sqlite3VdbeEndCoroutine(): v->pParse->nTempReg = 0; nRangeReg = 0
            p.n_temp_reg = 0;
            p.n_range_reg = 0;
            p.v.jump_here(addr_fill_sub - 1);
        }
    }
}

/// `exprListIsNoAffinity(pParse, pRow)`: return true if all expressions in the
/// expression-list passed as the only argument are constant and have no affinity.
fn expr_list_is_no_affinity(p: &mut Parse, row: ExprListId) -> bool {
    if !sqlite3_expr_list_is_constant(p, row, false) {
        return false;
    }
    for ii in 0..p.l(row).a.len() {
        let e = p.l(row).a[ii].p_expr;
        if sqlite3_expr_affinity(p, e) != 0 {
            return false;
        }
    }
    true
}

/// `sqlite3MultiValues(pParse, pLeft, pRow)`: this function is called by the parser for the
/// second and subsequent rows of a multi-row VALUES clause. Argument `left` is the part of
/// the VALUES clause already parsed, argument `row` is the vector of values for the new row.
/// The Select object returned represents the complete VALUES clause, including the new row.
///
/// There are two ways in which this may be achieved - by incremental coding of a co-routine
/// (the "co-routine" method) or by returning a Select object equivalent to the following
/// (the "UNION ALL" method):
///
///        "pLeft UNION ALL SELECT pRow"
///
/// If the VALUES clause contains a lot of rows, this compound Select object may consume a
/// lot of memory.
///
/// When the co-routine method is used, each row that will be returned by the VALUES clause
/// is coded into part of a co-routine as it is passed to this function. The returned Select
/// object is equivalent to:
///
///     SELECT * FROM (
///       Select object to read co-routine
///     )
///
/// The co-routine method is used in most cases. Exceptions are:
///
///    a) If the current statement has a WITH clause. This is to avoid statements like:
///
///            WITH cte AS ( VALUES('x'), ('y') ... )
///            SELECT * FROM cte AS a, cte AS b;
///
///       This will not work, as the co-routine uses a hard-coded register for its OP_Yield
///       instructions, and so it is not possible for two cursors to iterate through it
///       concurrently.
///
///    b) The schema is currently being parsed (i.e. the VALUES clause is part of a schema
///       item like a VIEW or TRIGGER). In this case there is no VM being generated when
///       parsing is taking place, and so generating a co-routine is not possible.
///
///    c) There are non-constant expressions in the VALUES clause (e.g. the VALUES clause is
///       part of a correlated sub-query).
///
///    d) One or more of the values in the first row of the VALUES clause has an affinity
///       (i.e. is a CAST expression). This causes problems because the complex rules SQLite
///       uses (see function sqlite3SubqueryColumnTypes() in select.c) to determine the
///       effective affinity of result columns for sub-queries within FROM clauses and for
///       CTEs.
pub fn sqlite3_multi_values(p: &mut Parse, left: SelectId, row: ExprListId) -> SelectId {
    let mut left = left;
    if left.is_null() {
        return left;
    }
    let left_n_src = p.src(p.s(left).p_src).a.len();
    if p.b_has_with
        || p.db.init_busy
        || !sqlite3_expr_list_is_constant(p, row, true)
        || (left_n_src == 0 && !{
            let el = p.s(left).p_e_list;
            expr_list_is_no_affinity(p, el)
        })
        || p.e_parse_mode != PARSE_MODE_NORMAL as u8
    {
        // The co-routine method cannot be used. Fall back to UNION ALL.
        let mut f = SF_Values | SF_MultiValue;
        if left_n_src != 0 {
            sqlite3_multi_values_end(p, left);
            f = SF_Values;
        } else if p.s(left).p_prior.nn() {
            // In this case set the SF_MultiValue flag only if it was set on pLeft
            f &= p.s(left).sel_flags;
        }
        let select = sqlite3_select_new(
            p,
            row,
            SrcListId::NULL,
            ExprId::NULL,
            ExprListId::NULL,
            ExprId::NULL,
            ExprListId::NULL,
            f,
            ExprId::NULL,
        );
        p.sm(left).sel_flags &= !SF_MultiValue;
        p.sm(select).op = TK_ALL;
        p.sm(select).p_prior = left;
        left = select;
    } else {
        let item_sel: SelectId; // the Select whose pSrc->a[0] is the co-routine
        if left_n_src == 0 {
            // Co-routine has not yet been started and the special Select object that accesses
            // the co-routine has not yet been created. This block does both those things.
            sqlite3_get_vdbe(p);
            let ret = sqlite3_select_new(
                p,
                ExprListId::NULL,
                SrcListId::NULL,
                ExprId::NULL,
                ExprListId::NULL,
                ExprId::NULL,
                ExprListId::NULL,
                0,
                ExprId::NULL,
            );
            // (sqlite3ReadSchema(pParse): the engine's schema is always loaded)
            {
                let (prior, op) = (p.s(left).p_prior, p.s(left).op);
                let r = p.sm(ret);
                r.p_prior = prior;
                r.op = op;
                if prior.nn() {
                    r.sel_flags |= SF_Values;
                }
            }
            p.sm(left).p_prior = SelectId::NULL;
            p.sm(left).op = TK_SELECT;
            let src = p.s(ret).p_src;
            // pRet->pSrc->nSrc = 1
            p.srcm(src).a.push(SrcItem { via_coroutine: true, i_cursor: -1, n_row: 2, ..SrcItem::default() });
            // sqlite3SrcItemAttachSubquery(pParse, p, pLeft, 0)
            let mut subq = Subquery { p_select: left, ..Subquery::default() };
            subq.addr_fill_sub = p.v.current_addr() + 1;
            p.n_mem += 1;
            subq.reg_return = p.n_mem;
            p.v.add_op3(OP_InitCoroutine, subq.reg_return, 0, subq.addr_fill_sub);
            let mut dest = sqlite3_select_dest_init(SRT_Coroutine, subq.reg_return);

            // Allocate registers for the output of the co-routine. Do so so that there are
            // two unused registers immediately before those used by the co-routine. This
            // allows the code in sqlite3Insert() to use these registers directly, instead of
            // copying the output of the co-routine to a separate array for processing.
            dest.i_sdst = p.n_mem + 3;
            dest.n_sdst = p.l(p.s(left).p_e_list).n_expr();
            p.n_mem += 2 + dest.n_sdst;

            p.sm(left).sel_flags |= SF_MultiValue;
            {
                let it = &mut p.srcm(src).a[0];
                it.is_subquery = true;
                it.p_subq = Some(Box::new(subq));
            }
            sqlite3_select(p, left, &mut dest);
            if let Some(q) = p.srcm(src).a[0].p_subq.as_mut() {
                q.reg_result = dest.i_sdst;
            }
            left = ret;
            item_sel = ret;
        } else {
            item_sel = left;
            let src = p.s(left).p_src;
            p.srcm(src).a[0].n_row += 1;
        }
        if p.n_err == 0 {
            let src = p.s(item_sel).p_src;
            let (sub_sel, reg_result, reg_return) = {
                let q = p.src(src).a[0].p_subq.as_ref().expect("co-routine subquery");
                (q.p_select, q.reg_result, q.reg_return)
            };
            if p.l(p.s(sub_sel).p_e_list).n_expr() != p.l(row).n_expr() {
                sqlite3_select_wrong_num_terms_error(p, sub_sel);
            } else {
                sqlite3_expr_code_expr_list(p, row, reg_result, 0, 0);
                p.v.add_op1(OP_Yield, reg_return);
            }
        }
    }
    left
}
