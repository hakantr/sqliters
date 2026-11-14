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

/// `convertCompoundSelectToSubquery(pWalker, p)`: a compound SELECT whose ORDER BY clause
/// uses a COLLATE operator and that is not made only of UNION ALL is rewritten as
/// `SELECT * FROM (compound) ORDER BY …`. The rewrite needs a FROM-clause subquery, which
/// step V1 does not generate code for: the statement is reported as unsupported.
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
    // If iOrderByCol is already non-zero, then it has already been matched by a prior call
    // to this routine
    if p.l(ob).a[0].i_order_by_col != 0 {
        return WRC_Continue;
    }
    let has_collate = p.l(ob).a.iter().any(|it| p.e(it.p_expr).flags & EP_Collate != 0);
    if !has_collate {
        return WRC_Continue;
    }
    p.not_supported("compound SELECT with COLLATE in ORDER BY (rewritten as a FROM subquery)");
    WRC_Abort
}

/// `selectExpander(pWalker, p)`: this routine is a Walker callback for "expanding" a SELECT
/// statement. "Expanding" means to do the following:
///
///    (1)  Make sure VDBE cursor numbers have been assigned to every element of the FROM
///         clause.
///    (2)  Fill in the pTabList->a[].pTab fields in the SrcList that defines FROM clause.
///    (3)  Add terms to the WHERE clause to accommodate the NATURAL keyword on joins and the
///         ON and USING clause of joins.
///    (4)  Scan the list of columns in the result set (pEList) looking for instances of the
///         "*" operator or "TABLE.*" operator. If found, expand each "*" to be every column
///         in every table and TABLE.* to be every column in TABLE.
///
/// Step V1: the only FROM-clause term that can exist is the co-routine built by
/// `sqlite3MultiValues()`, which arrives with its table and cursor already filled in.
fn select_expander(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let sel_flags = p.s(sel).sel_flags;
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
    // (pParse->pWith && SF_View: views are not ported)
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
            // A CTE or an ordinary table or view name in the FROM clause
            p.not_supported("FROM clause: table, view or CTE reference");
            return WRC_Abort;
        }
    }

    // Process NATURAL keywords, and ON and USING clauses of joins.
    if p.n_err != 0 {
        return WRC_Abort;
    }
    if p.src(tab_list).a.len() > 1 {
        p.not_supported("joins (sqlite3ProcessJoin)");
        return WRC_Abort;
    }

    // For every "*" that occurs in the column list, insert the names of all columns in all
    // tables. And for every TABLE.* insert the names of all columns in TABLE. The parser
    // inserted a special expression with the TK_ASTERISK operator for each "*" that it found
    // in the column list. The following code just has to locate the TK_ASTERISK expressions
    // and expand each one to the list of all columns in all tables.
    //
    // The first loop just checks to see if there are any "*" operators that need expanding.
    let mut elist_flags: u32 = 0;
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
                let z_t_name = if p.e(pe).op == TK_DOT { Some(p.e(p.e(pe).p_left).tok().to_owned()) } else { None };
                for i in 0..p.src(tab_list).a.len() {
                    let (tab, alias, i_cursor) = {
                        let it = &p.src(tab_list).a[i];
                        (it.p_s_tab, it.z_alias.clone(), it.i_cursor)
                    };
                    let z_tab_name = alias.unwrap_or_else(|| p.tab(tab).z_name.clone());
                    if let Some(zt) = z_t_name.as_deref()
                        && !zt.eq_ignore_ascii_case(&z_tab_name)
                    {
                        continue;
                    }
                    // (zSchemaName = "*" for a subquery; longNames / nested FROM / USING /
                    // hidden columns do not apply to the co-routine of a VALUES clause)
                    // (nested FROM, USING, hidden / NOEXPAND columns and the rowid of a nested
                    // FROM do not apply to the co-routine of a VALUES clause)
                    let long_names = (p.db.flags & SQLITE_FullColNames) != 0 && (p.db.flags & SQLITE_ShortColNames) == 0;
                    let i_err_ofst = if p.e(pe).op == TK_DOT { p.e(p.e(pe).p_right).w } else { p.e(pe).w };
                    let n_col = p.tab(tab).a_col.len();
                    for j in 0..n_col {
                        let z_name = p.tab(tab).a_col[j].z_cn_name.clone();
                        table_seen = true;
                        // With a single table in the FROM clause the expression is the bare
                        // column name (pTabList->nSrc>1 adds the "TABLE." / "SCHEMA.TABLE."
                        // qualifiers: joins are not ported).
                        let px = sqlite3_expr(p, TK_ID, Some(&z_name));
                        sqlite3_expr_set_error_offset(p, px, i_err_ofst);
                        new_list = sqlite3_expr_list_append(p, new_list, px);
                        let it = p.lm(new_list).a.last_mut().expect("item");
                        if long_names {
                            it.z_e_name = Some(format!("{z_tab_name}.{z_name}"));
                        } else {
                            it.z_e_name = Some(z_name);
                        }
                        it.e_e_name = ENAME_NAME as u8;
                        let _ = i_cursor;
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

/// `sqlite3ExpandSubquery(pParse, pFrom)`: the SrcItem structure passed as the second
/// argument represents a sub-query in the FROM clause of a SELECT statement. This function
/// allocates and populates the SrcItem.pTab object. If successful, SQLITE_OK is returned.
/// Otherwise, if an OOM error is encountered, SQLITE_NOMEM.
fn sqlite3_expand_subquery(p: &mut Parse, src: SrcListId, i_from: usize) -> i32 {
    let (mut sel, alias) = {
        let it = &p.src(src).a[i_from];
        (it.p_subq.as_ref().map(|q| q.p_select).unwrap_or(SelectId::NULL), it.z_alias.clone())
    };
    // pTab->zName = sqlite3MPrintf(pParse->db, "%!S", pFrom): "(subquery-%u)" with the
    // selId of the subquery when there is no alias
    let z_name = match alias {
        Some(a) => a,
        None => format!("(subquery-{})", p.s(sel).sel_id),
    };
    while p.s(sel).p_prior.nn() {
        sel = p.s(sel).p_prior;
    }
    let e_list = p.s(sel).p_e_list;
    let a_col = sqlite3_columns_from_expr_list(p, e_list);
    p.a_tab.push(Table {
        z_name,
        a_col,
        n_tab_ref: 1,
        i_p_key: -1,
        e_tab_type: TABTYP_VIEW as u8,
        n_row_log_est: 200,
        tab_flags: TF_Ephemeral | TF_NoVisibleRowid,
        i_db: -1,
        ..Table::default()
    });
    let tab = TableId(p.a_tab.len() as u32 - 1);
    p.srcm(src).a[i_from].p_s_tab = tab;
    if p.n_err != 0 { SQLITE_ERROR } else { SQLITE_OK }
}

/// `sqlite3ColumnsFromExprList(pParse, pEList, pnCol, paCol)`: given an expression list
/// (which is really the list of expressions that form the result set of a SELECT statement)
/// compute appropriate column names for a table that would hold the expression list. All
/// column names will be unique.
///
/// Step V1 needs this for the result set of a VALUES clause only, whose columns are named
/// "column1", "column2", …; other shapes are reported as unsupported.
fn sqlite3_columns_from_expr_list(p: &mut Parse, e_list: ExprListId) -> Vec<Column> {
    let mut a_col = Vec::new();
    let n = p.l(e_list).a.len();
    for i in 0..n {
        let (z_e_name, e_e_name, pe) = {
            let it = &p.l(e_list).a[i];
            (it.z_e_name.clone(), it.e_e_name, it.p_expr)
        };
        // Get an appropriate name for the column
        let z_name = match z_e_name {
            // If the column contains an "AS <name>" phrase, use <name> as the name
            Some(z) if e_e_name == ENAME_NAME as u8 => z,
            _ => {
                let mut c = sqlite3_expr_skip_collate_and_likely(p, pe);
                while c.nn() && p.e(c).op == TK_DOT {
                    c = p.e(c).p_right;
                }
                if c.nn() && (p.e(c).op == TK_COLUMN || p.e(c).op == TK_ID) {
                    p.not_supported("column names of a subquery with column references");
                    return a_col;
                }
                // Use the original text of the column expression as its name: for a VALUES
                // clause there is no span, so the name is "column%d".
                format!("column{}", i + 1)
            }
        };
        // Make sure the column name is unique. If the name is not unique, append an integer
        // to the name so that it becomes unique.
        if a_col.iter().any(|c: &Column| c.z_cn_name.eq_ignore_ascii_case(&z_name)) {
            p.not_supported("duplicate column names in a subquery");
            return a_col;
        }
        a_col.push(Column { z_cn_name: z_name, ..Column::default() });
    }
    a_col
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

/// `sqlite3SelectAddTypeInfo(pParse, pSelect)`: this routine adds datatype and collating
/// sequence information to the Table structures of all FROM-clause subqueries in a SELECT
/// statement (`selectAddSubqueryTypeInfo` / `sqlite3SubqueryColumnTypes`).
///
/// The only subquery of step V1 is the co-routine of a VALUES clause, whose columns have no
/// declared type and affinity SQLITE_AFF_NONE; its rows are copied register to register by
/// `sqlite3WhereEnd()`, so only the SF_HasTypeInfo flag is observable.
fn sqlite3_select_add_type_info(p: &mut Parse, sel: SelectId) {
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
                // sqlite3SubqueryColumnTypes(pParse, pTab, pSel, SQLITE_AFF_NONE)
                let mut s = sub;
                while p.s(s).p_prior.nn() {
                    s = p.s(s).p_prior;
                }
                let el = p.s(s).p_e_list;
                for j in 0..p.tab(tab).a_col.len().min(p.l(el).a.len()) {
                    let e = p.l(el).a[j].p_expr;
                    let mut aff = sqlite3_expr_affinity(p, e);
                    if aff <= SQLITE_AFF_NONE {
                        aff = SQLITE_AFF_NONE;
                    }
                    p.a_tab[tab.ix()].a_col[j].affinity = aff;
                }
            }
        }
    }
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

/// `selectInnerLoop(pParse, p, srcTab, pSort, pDistinct, pDest, iContinue, iBreak)`: this
/// routine generates the code for the inside of the inner loop of a SELECT.
///
/// If `src_tab` is negative, then the p->pEList expressions are evaluated in order to get the
/// data for this row. If `src_tab` is zero or more, then data is pulled from src_tab and
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
    let e_dest = dest.e_dest as i32;
    let i_parm = dest.i_sd_parm;
    let has_distinct = distinct.map_or(WHERE_DISTINCT_NOOP, |d| d.e_tnct_type as i32);
    let sort = match sort {
        Some(s) if s.p_order_by.nn() => Some(s),
        _ => None,
    };
    if sort.is_some() {
        p.not_supported("ORDER BY that needs a sorter (pushOntoSorter)");
        return;
    }
    if has_distinct == 0 {
        let off = p.s(sel).i_offset;
        code_offset(p, off, i_continue);
    }

    // Pull the requested columns.
    let mut n_result_col = p.l(p.s(sel).p_e_list).n_expr();
    if dest.i_sdst == 0 {
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
    let reg_result = dest.i_sdst;
    if src_tab >= 0 {
        for i in 0..n_result_col {
            p.v.add_op3(OP_Column, src_tab, i, reg_result + i);
        }
    } else if e_dest != SRT_Exists {
        // If the destination is an EXISTS(...) expression, the actual values returned by the
        // SELECT are not required.
        let ecel_flags = if e_dest == SRT_Mem || e_dest == SRT_Output || e_dest == SRT_Coroutine {
            SQLITE_ECEL_DUP as u8
        } else {
            0
        };
        // innerLoopLoadRow()
        let el = p.s(sel).p_e_list;
        sqlite3_expr_code_expr_list(p, el, reg_result, 0, ecel_flags);
    }

    // If the DISTINCT keyword was present on the SELECT statement and this row has been seen
    // before, then do not make this row part of the result.
    if has_distinct != 0 {
        let d = distinct.expect("distinct context");
        let e_type = d.e_tnct_type as i32;
        let el = p.s(sel).p_e_list;
        let i_tab = code_distinct(p, e_type, d.tab_tnct, i_continue, el, reg_result);
        fix_distinct_open_eph(p, e_type, i_tab, d.addr_tnct);
        let off = p.s(sel).i_offset;
        code_offset(p, off, i_continue);
    }

    match e_dest {
        // Store the result as data using a unique key.
        SRT_Fifo | SRT_DistFifo | SRT_Table | SRT_EphemTab => {
            let r1 = sqlite3_get_temp_range(p, 1);
            p.v.add_op3(OP_MakeRecord, reg_result, n_result_col, r1);
            if e_dest == SRT_DistFifo {
                // If the destination is DistFifo, then cursor (iParm+1) is open on an
                // ephemeral index. If the current row is already present in the index, do not
                // write it to the output. If not, add the current row to the index and
                // proceed with writing it to the output table.
                let addr = p.v.current_addr() + 4;
                p.v.add_op4_int(OP_Found, i_parm + 1, addr, r1, 0);
                p.v.add_op4_int(OP_IdxInsert, i_parm + 1, r1, reg_result, n_result_col);
            }
            let r2 = sqlite3_get_temp_reg(p);
            p.v.add_op2(OP_NewRowid, i_parm, r2);
            p.v.add_op3(OP_Insert, i_parm, r1, r2);
            p.v.change_p5(OPFLAG_APPEND as u16);
            sqlite3_release_temp_reg(p, r2);
            sqlite3_release_temp_range(p, r1, 1);
        }
        // If we are creating a set for an "expr IN (SELECT ...)" construct, then there should
        // be a single item on the stack. Write this item into the set table with bogus data.
        SRT_Set => {
            let r1 = sqlite3_get_temp_reg(p);
            let aff = dest.z_aff_sdst.clone().unwrap_or_default();
            let mut aff = aff;
            aff.truncate(n_result_col as usize);
            p.v.add_op4(OP_MakeRecord, reg_result, n_result_col, r1, P4::Dynamic(aff));
            p.v.add_op4_int(OP_IdxInsert, i_parm, r1, reg_result, n_result_col);
            if dest.i_sd_parm2 != 0 {
                p.v.add_op4_int(OP_FilterAdd, dest.i_sd_parm2, 0, reg_result, n_result_col);
            }
            sqlite3_release_temp_reg(p, r1);
        }
        // If any row exist in the result set, record that fact and abort.
        SRT_Exists => {
            p.v.add_op2(OP_Integer, 1, i_parm);
            // The LIMIT clause will terminate the loop for us
        }
        // If this is a scalar select that is part of an expression, then store the results in
        // the appropriate memory cell or array of memory cells and break out of the scan loop.
        SRT_Mem => {
            if reg_result != i_parm {
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
            if e_dest == SRT_Coroutine {
                p.v.add_op1(OP_Yield, dest.i_sd_parm);
            } else {
                p.v.add_op2(OP_ResultRow, reg_result, n_result_col);
            }
        }
        SRT_Upfrom | SRT_DistQueue | SRT_Queue => {
            p.not_supported("SELECT destination of UPDATE-FROM or a recursive CTE");
        }
        // Discard the results. This is used for SELECT statements inside the body of a
        // TRIGGER. The purpose of such selects is to call user-defined functions that have
        // side effects. We do not care about the actual results of the select.
        _ => {
            n_result_col = 0;
            let _ = n_result_col;
        }
    }

    // Jump to the end of the loop if the LIMIT is reached. Except, if there is a sorter, in
    // which case the sorter has already limited the output for us.
    let lim = p.s(sel).i_limit;
    if lim != 0 {
        p.v.add_op2(OP_DecrJumpZero, lim, i_break);
    }
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
        if (p.s(sel).sel_flags & SF_Recursive) != 0 {
            p.not_supported("recursive common table expression");
            return 1;
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
    }
    p.ncm(nc).nc_flags &= !NC_InAggFunc;
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
        // (EP_WinFunc / FILTER: not ported in step V1)
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

/// `sqlite3Select(pParse, p, pDest)`: generate byte-code for the SELECT statement given in
/// the `sel` argument.
///
/// The results are returned according to the SelectDest structure. See comments in
/// sqliteInt.h for additional information.
///
/// This routine returns the number of errors. If any errors are encountered, then an
/// appropriate error message is left in pParse->zErrMsg.
///
/// This routine does NOT free the Select structure passed in. The calling function needs to
/// do that.
pub fn sqlite3_select(p: &mut Parse, sel: SelectId, dest: &mut SelectDest) -> i32 {
    let mut rc = 1;
    sqlite3_get_vdbe(p);
    if sel.is_null() || p.n_err != 0 {
        return 1;
    }
    // (sqlite3AuthCheck(pParse, SQLITE_SELECT, 0, 0, 0): the engine authorizes)

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
    // (SF_UFSrcCheck: UPDATE ... FROM is not ported)

    if dest.e_dest as i32 == SRT_Output {
        // sqlite3GenerateColumnNames(pParse, p): the names and declared types of the result
        // columns. TODO(Track M): they are not visible in EXPLAIN (whose columns are the
        // fixed eight) and are filled in when the machine executes the program.
    }
    // (sqlite3WindowRewrite: window functions are not ported)

    let tab_list = p.s(sel).p_src;
    let mut is_agg = (p.s(sel).sel_flags & SF_Aggregate) != 0;
    let mut s_sort = SortCtx { p_order_by: p.s(sel).p_order_by, ..SortCtx::default() };

    // Try to do various optimizations (flattening subqueries, and strength reduction of join
    // operators) in the FROM clause up into the main query.
    if p.s(sel).p_prior.is_null() {
        for i in 0..p.src(tab_list).a.len() {
            let (is_subq, via_co) = {
                let it = &p.src(tab_list).a[i];
                (it.is_subquery, it.via_coroutine)
            };
            // The co-routine of a multi-row VALUES clause has already been coded
            // (pSubq->addrFillSub!=0): flattenSubquery() refuses it because of restriction
            // (25) ... any other FROM term is unsupported.
            if !(is_subq && via_co) {
                p.not_supported("FROM clause term (flattener / planner)");
                return 1;
            }
        }
    }

    // Handle compound SELECT statements using the separate multiSelect() procedure.
    if p.s(sel).p_prior.nn() {
        return multi_select(p, sel, dest);
    }

    // (existsToJoin, propagateConstants, countOfViewOptimization: need a FROM clause with
    // tables; the co-routine of VALUES has no WHERE clause and is not an aggregate candidate)
    if !p.src(tab_list).a.is_empty() && (p.s(sel).p_where.nn() || is_agg || p.s(sel).p_group_by.nn()) {
        p.not_supported("WHERE / aggregate over a FROM clause term");
        return 1;
    }

    // For each term in the FROM clause, do two things: (1) Authorized unreferenced tables
    // (2) Generate code for all sub-queries. The co-routine of a VALUES clause was coded by
    // sqlite3MultiValues() (pSubq->addrFillSub!=0).

    // Various elements of the SELECT copied into local variables for convenience
    let e_list = p.s(sel).p_e_list;
    let mut p_where = p.s(sel).p_where;
    let mut group_by = p.s(sel).p_group_by;
    let having = p.s(sel).p_having;
    let mut s_distinct = DistinctCtx { is_tnct: ((p.s(sel).sel_flags & SF_Distinct) != 0) as u8, ..DistinctCtx::default() };

    // If the query is DISTINCT with an ORDER BY but is not an aggregate, and if the select-list
    // is the same as the ORDER BY list, then this query can be rewritten as a GROUP BY. In
    // other words, this:
    //
    //     SELECT DISTINCT xyz FROM ... ORDER BY xyz
    //
    // is transformed to:
    //
    //     SELECT xyz FROM ... GROUP BY xyz ORDER BY xyz
    //
    // The second form is preferred as a single index (or temp-table) may be used for both the
    // ORDER BY and DISTINCT processing. As originally written, the query must use a temp-table
    // for at least one of the ORDER BY or DISTINCT.
    if (p.s(sel).sel_flags & (SF_Distinct | SF_Aggregate)) == SF_Distinct
        && sqlite3_copy_sort_order(p, e_list, s_sort.p_order_by)
        && sqlite3_expr_list_compare(p, e_list, s_sort.p_order_by, -1) == 0
        && p.db.optimization_enabled(SQLITE_GroupByOrder)
    {
        p.sm(sel).sel_flags &= !SF_Distinct;
        group_by = sqlite3_expr_list_dup(p, e_list, 0);
        p.sm(sel).p_group_by = group_by;
        for i in 0..p.l(group_by).a.len() {
            p.lm(group_by).a[i].i_order_by_col = i as u16 + 1;
        }
        p.sm(sel).sel_flags |= SF_Aggregate;
        // Notice that even thought SF_Distinct has been cleared from p->selFlags, the
        // sDistinct.isTnct is still set. Hence, isTnct represents the original setting of the
        // DISTINCT keyword, not the setting after the rewrite.
        s_distinct.is_tnct = 2;
        is_agg = true;
    }

    // If there is an ORDER BY clause, then create an ephemeral index to do the sorting. But
    // this sorting ephemeral index might end up being unused if the data can be extracted in
    // pre-sorted order. If that is the case, then the OP_OpenEphemeral instruction will be
    // changed to an OP_Noop once we figure out that the sorting index is not needed. The
    // sSort.addrSortIndex variable is used to facilitate that change.
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

    // If the output is destined for a temporary table, open that table.
    if dest.e_dest as i32 == SRT_EphemTab {
        let n = p.l(e_list).n_expr();
        p.v.add_op2(OP_OpenEphemeral, dest.i_sd_parm, n);
        // (SF_NestedFrom: not ported)
    }

    // Set the limiter.
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

    // Open an ephemeral index to use for the distinct set.
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
        // No aggregate functions and no GROUP BY clause
        let wctrl_flags: u16 =
            (if s_distinct.is_tnct != 0 { WHERE_WANT_DISTINCT as u16 } else { 0 }) | (p.s(sel).sel_flags & SF_FixedLimit) as u16;

        // Begin the database scan.
        let el = p.s(sel).p_e_list;
        let n_select_row = p.s(sel).n_select_row;
        let Some(winfo) = sqlite3_where_begin(p, tab_list, p_where, s_sort.p_order_by, el, sel, wctrl_flags, n_select_row as i32) else {
            return (p.n_err > 0) as i32;
        };
        if winfo.n_row_out < p.s(sel).n_select_row {
            p.sm(sel).n_select_row = winfo.n_row_out;
            if dest.e_dest as i32 <= SRT_DistQueue && dest.e_dest as i32 >= SRT_DistFifo {
                // TUNING: For a UNION CTE, because UNION is implies DISTINCT, reduce the
                // estimated number of rows by 3x.
                p.sm(sel).n_select_row -= 30;
            }
        }
        if s_distinct.is_tnct != 0 && winfo.e_distinct != 0 {
            s_distinct.e_tnct_type = winfo.e_distinct;
        }
        if s_sort.p_order_by.nn() {
            s_sort.n_ob_sat = winfo.is_ordered();
            s_sort.label_ob_lopt = winfo.order_by_limit_opt_label();
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
        select_inner_loop(p, sel, -1, Some(&mut s_sort), Some(&s_distinct), dest, winfo.i_continue, winfo.i_break);

        // End the database scan loop.
        sqlite3_where_end(p, winfo);
    } else {
        // This case when there exist aggregate functions or a GROUP BY clause or both
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
            // we may be able to use the same index (or sorter) for both. Copy the sort order
            // from the ORDER BY to the GROUP BY so that the sorter of the GROUP BY delivers
            // the rows in the order the ORDER BY wants.
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

        // Processing for aggregates with GROUP BY is very different and much more complex
        // than aggregates without a GROUP BY.
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
            // all, the OP_SorterOpen instruction will be converted into an OP_Noop.
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
            // loop that uses an index to extract information in group order without sorting.
            p.v.add_op2(OP_Gosub, reg_reset, addr_reset);
            let flags = (if s_distinct.is_tnct == 2 { WHERE_DISTINCTBY } else { WHERE_GROUPBY }) as u16
                | (if order_by_grp { WHERE_SORTBYGROUP as u16 } else { 0 })
                | dist_flag;
            let Some(winfo) = sqlite3_where_begin(p, tab_list, p_where, group_by, distinct, sel, flags, 0) else {
                return (p.n_err > 0) as i32;
            };
            assign_aggregate_registers(p, agg);
            let e_dist = winfo.e_distinct as i32;
            if winfo.is_ordered() != n_group_by {
                // Rows are coming out in undetermined order. We have to push each row into a
                // sorting index, terminate the first loop, then loop over the sorting index in
                // order to get the output in sorted order.
                p.not_supported("GROUP BY that needs a sorter (rows of a FROM clause)");
                return 1;
            }
            // The optimizer is able to deliver rows in group by order so we do not have to
            // sort. The OP_OpenEphemeral table will be cancelled later because we still need
            // to use the pKeyInfo.
            // (orderByGrp && (groupBySort || sqlite3WhereIsSorted(pWInfo)): never "sorted"
            // without a table)

            // Evaluate the current GROUP BY terms and store in b0, b1, b2... (b0 is memory
            // location iBMem+0, b1 is iBMem+1, and so forth). Then compare the current GROUP
            // BY terms against the GROUP BY terms from the previous row currently stored in
            // a0, a1, a2...
            for j in 0..n_group_by {
                let (i_order_by_col, ge) = {
                    let it = &p.l(group_by).a[j as usize];
                    (it.i_order_by_col as i32, it.p_expr)
                };
                p.aggm(agg).direct_mode = 1;
                sqlite3_expr_code(p, ge, i_b_mem + j);
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
            sqlite3_where_end(p, winfo);
            p.v.change_to_noop(addr_sorting_idx);

            // Output the final row of result
            p.v.add_op2(OP_Gosub, reg_output_row, addr_output_row_label);

            // Jump over the subroutines
            p.v.goto(addr_end);

            // Generate a subroutine that outputs a single row of the result set. This
            // subroutine first looks at the iUseFlag. If iUseFlag is zero, the subroutine is a
            // no-op. If iUseFlag is non-zero, then it means that the accumulator registers
            // have been filled and the subroutine should output a row.
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
            // Aggregate functions without GROUP BY.
            // (isSimpleCount(): needs exactly one table in the FROM clause)
            let mut reg_acc = 0; // "populate accumulators" flag
            let mut distinct = ExprListId::NULL;
            let mut dist_flag: u16 = 0;

            // If there are accumulator registers but no min() or max() functions without
            // FILTER clauses, allocate register regAcc. Register regAcc will contain 0 the
            // first time the inner loop runs, and 1 thereafter. The code generated by
            // updateAccumulator() uses this to ensure that the accumulator registers are
            // only populated for the first row visited by the aggregate, so that they are
            // updated atomically.
            if p.agg(agg).n_accumulator != 0 {
                let n = p.agg(agg).a_func.len();
                let mut i = 0;
                while i < n {
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

            // This case runs if the aggregate has no GROUP BY clause. The processing is much
            // simpler since there is only a single row of output.
            reset_accumulator(p, agg);

            // If this query is a candidate for the min/max optimization, then minMaxFlag will
            // have been previously set to either WHERE_ORDERBY_MIN or WHERE_ORDERBY_MAX and
            // pMinMaxOrderBy will be an ORDER BY clause for the where.c to use.
            let Some(winfo) = sqlite3_where_begin(p, tab_list, p_where, min_max_order_by, distinct, sel, min_max_flag as u16 | dist_flag, 0) else {
                return (p.n_err > 0) as i32;
            };
            let e_dist = winfo.e_distinct as i32;
            update_accumulator(p, reg_acc, agg, e_dist);
            if e_dist != WHERE_DISTINCT_NOOP && !p.agg(agg).a_func.is_empty() {
                let (i_distinct, i_dist_addr) = (p.agg(agg).a_func[0].i_distinct, p.agg(agg).a_func[0].i_dist_addr);
                fix_distinct_open_eph(p, e_dist, i_distinct, i_dist_addr);
            }
            if reg_acc != 0 {
                p.v.add_op2(OP_Integer, 1, reg_acc);
            }
            // (sqlite3WhereMinMaxOptEarlyOut: needs an ordered inner loop, i.e. a table)
            sqlite3_where_end(p, winfo);
            finalize_agg_functions(p, agg);

            s_sort.p_order_by = ExprListId::NULL;
            sqlite3_expr_if_false(p, having, addr_end, SQLITE_JUMPIFNULL);
            select_inner_loop(p, sel, -1, None, None, dest, addr_end, addr_end);
        }
        p.v.resolve_label(addr_end);
    } // endif aggregate query

    // If there is an ORDER BY clause, then we need to sort the results and send them to the
    // callback one by one.
    if s_sort.p_order_by.nn() {
        p.not_supported("ORDER BY that needs a sorter (generateSortTail)");
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
