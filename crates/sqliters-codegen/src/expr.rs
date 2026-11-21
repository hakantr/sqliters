//! `expr.c`, first part: construction, duplication and analysis of expression trees
//! (everything that does not emit VDBE code; the code generator is in `exprcode.rs`).
//!
//! Functions appear in the order of `expr.c` and keep its names.

use std::sync::Arc;

use sqliters_types::Value;
use sqliters_vdbe::vdbeaux::{CollSeq, FuncDef};
use sqliters_vdbe::vdbemem::{Mem, sqlite3_mem_compare};

use crate::callback::*;
use crate::consts::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::util::*;
use crate::walker::*;

/// `sqlite3TableColumnAffinity(pTab, iCol)`
pub fn sqlite3_table_column_affinity(p: &Parse, tab: TableId, i_col: i32) -> u8 {
    if i_col < 0 || i_col >= p.tab(tab).n_col() as i32 {
        return SQLITE_AFF_INTEGER;
    }
    p.tab(tab).a_col[i_col as usize].affinity
}

/// `sqlite3ExprAffinity(pExpr)`: the 'affinity' of the expression.
pub fn sqlite3_expr_affinity(p: &Parse, mut e: ExprId) -> u8 {
    let mut op = p.e(e).op;
    loop {
        let x = p.e(e);
        if op == TK_COLUMN || (op == TK_AGG_COLUMN && x.y_tab.nn()) {
            return sqlite3_table_column_affinity(p, x.y_tab, x.i_column as i32);
        }
        if op == TK_SELECT {
            let el = p.s(x.x_select()).p_e_list;
            return sqlite3_expr_affinity(p, p.l(el).a[0].p_expr);
        }
        if op == TK_CAST {
            return sqlite3_affinity_type(x.tok());
        }
        if op == TK_SELECT_COLUMN {
            let el = p.s(p.e(x.p_left).x_select()).p_e_list;
            return sqlite3_expr_affinity(p, p.l(el).a[x.i_column as usize].p_expr);
        }
        if op == TK_VECTOR || (op == TK_FUNCTION && x.aff_expr == SQLITE_AFF_DEFER) {
            return sqlite3_expr_affinity(p, p.l(x.x_list()).a[0].p_expr);
        }
        if x.has(EP_Skip | EP_IfNullRow) {
            e = x.p_left;
            op = p.e(e).op;
            continue;
        }
        if op != TK_REGISTER {
            break;
        }
        op = x.op2;
        if op == TK_REGISTER {
            break;
        }
    }
    p.e(e).aff_expr
}

/// `sqlite3ExprDataType(pExpr)`: a mask of the datatypes the expression might return
/// (0x01 numeric, 0x02 text, 0x04 blob).
pub fn sqlite3_expr_data_type(p: &Parse, mut e: ExprId) -> i32 {
    while e.nn() {
        let x = p.e(e);
        match x.op {
            TK_COLLATE | TK_IF_NULL_ROW | TK_UPLUS => e = x.p_left,
            TK_NULL => e = ExprId::NULL,
            TK_STRING => return 0x02,
            TK_BLOB => return 0x04,
            TK_CONCAT => return 0x06,
            TK_VARIABLE | TK_AGG_FUNCTION | TK_FUNCTION => return 0x07,
            TK_COLUMN | TK_AGG_COLUMN | TK_SELECT | TK_CAST | TK_SELECT_COLUMN | TK_VECTOR => {
                let aff = sqlite3_expr_affinity(p, e);
                if aff >= SQLITE_AFF_NUMERIC {
                    return 0x05;
                }
                if aff == SQLITE_AFF_TEXT {
                    return 0x06;
                }
                return 0x07;
            }
            TK_CASE => {
                let mut res = 0;
                let list = p.l(x.x_list());
                let n = list.a.len();
                let mut ii = 1;
                while ii < n {
                    res |= sqlite3_expr_data_type(p, list.a[ii].p_expr);
                    ii += 2;
                }
                if n % 2 != 0 {
                    res |= sqlite3_expr_data_type(p, list.a[n - 1].p_expr);
                }
                return res;
            }
            _ => return 0x01,
        }
    }
    0x00
}

/// `sqlite3ExprAddCollateToken(pParse, pExpr, pCollName, dequote)`: set the collating sequence
/// for expression `e` to be the collating sequence named by `coll_name`.
pub fn sqlite3_expr_add_collate_token(p: &mut Parse, e: ExprId, coll_name: &Token, dequote: i32) -> ExprId {
    if coll_name.n > 0 && p.n_err == 0 {
        let z = p.tok_str(coll_name).to_owned();
        return expr_add_collate(p, e, &z, dequote);
    }
    e
}
fn expr_add_collate(p: &mut Parse, e: ExprId, z: &str, dequote: i32) -> ExprId {
    let p_new = sqlite3_expr_alloc_str(p, TK_COLLATE, Some(z), dequote);
    p.em(p_new).p_left = e;
    p.em(p_new).flags |= EP_Collate | EP_Skip;
    let h = p.e(e).n_height + 1;
    p.em(p_new).n_height = h;
    sqlite3_expr_check_height(p, h);
    p_new
}
/// `sqlite3ExprAddCollateString(pParse, pExpr, zC)`
pub fn sqlite3_expr_add_collate_string(p: &mut Parse, e: ExprId, z_c: &str) -> ExprId {
    if !z_c.is_empty() && p.n_err == 0 {
        return expr_add_collate(p, e, z_c, 0);
    }
    e
}

/// `sqlite3ExprSkipCollate(pExpr)`: skip over any TK_COLLATE operators.
pub fn sqlite3_expr_skip_collate(p: &Parse, mut e: ExprId) -> ExprId {
    while e.nn() && p.e(e).has(EP_Skip) {
        e = p.e(e).p_left;
    }
    e
}

/// `sqlite3ExprSkipCollateAndLikely(pExpr)`: skip over any TK_COLLATE operators and/or any
/// unlikely() or likelihood() or likely() functions at the root of an expression.
pub fn sqlite3_expr_skip_collate_and_likely(p: &Parse, mut e: ExprId) -> ExprId {
    while e.nn() && p.e(e).has(EP_Skip | EP_Unlikely) {
        if p.e(e).has(EP_Unlikely) {
            e = p.l(p.e(e).x_list()).a[0].p_expr;
        } else if p.e(e).op == TK_COLLATE {
            e = p.e(e).p_left;
        } else {
            break;
        }
    }
    e
}

/// `sqlite3ExprCollSeq(pParse, pExpr)`: the collation sequence for the expression, or the
/// default collation sequence (NULL here) if there is none.
pub fn sqlite3_expr_coll_seq(p: &mut Parse, e: ExprId) -> Option<Arc<CollSeq>> {
    let mut p_coll: Option<Arc<CollSeq>> = None;
    let mut x = e;
    while x.nn() {
        let ex = p.e(x);
        let mut op = ex.op;
        if op == TK_REGISTER {
            op = ex.op2;
        }
        if (op == TK_AGG_COLUMN && ex.y_tab.nn()) || op == TK_COLUMN || op == TK_TRIGGER {
            let j = ex.i_column;
            if j >= 0 {
                let z_coll = p.tab(ex.y_tab).a_col[j as usize].z_coll.clone();
                // sqlite3ColumnColl() / sqlite3FindCollSeq(db, ENC(db), zColl, 0)
                p_coll = match z_coll {
                    Some(z) => sqlite3_find_coll_seq(p, &z),
                    // sqlite3FindCollSeq(db, enc, 0, 0) is db->pDfltColl
                    None => Some(dflt_coll(p)),
                };
            }
            break;
        }
        if op == TK_CAST || op == TK_UPLUS {
            x = ex.p_left;
            continue;
        }
        if op == TK_VECTOR || (op == TK_FUNCTION && ex.aff_expr == SQLITE_AFF_DEFER) {
            x = p.l(ex.x_list()).a[0].p_expr;
            continue;
        }
        if op == TK_COLLATE {
            let z = ex.tok().to_owned();
            p_coll = sqlite3_get_coll_seq(p, &z);
            break;
        }
        if ex.flags & EP_Collate != 0 {
            if ex.p_left.nn() && (p.e(ex.p_left).flags & EP_Collate) != 0 {
                x = ex.p_left;
            } else {
                let mut p_next = ex.p_right;
                if ex.use_x_list() && ex.x_list().nn() {
                    let list = p.l(ex.x_list());
                    for it in &list.a {
                        if p.e(it.p_expr).has(EP_Collate) {
                            p_next = it.p_expr;
                            break;
                        }
                    }
                }
                x = p_next;
            }
        } else {
            break;
        }
    }
    // sqlite3CheckCollSeq(): a collation found through sqlite3FindCollSeq always has a
    // comparison function here.
    p_coll
}

/// `sqlite3ExprNNCollSeq(pParse, pExpr)`: like `sqlite3ExprCollSeq` but never NULL — the
/// default collation (BINARY) is returned instead.
pub fn sqlite3_expr_nn_coll_seq(p: &mut Parse, e: ExprId) -> Arc<CollSeq> {
    match sqlite3_expr_coll_seq(p, e) {
        Some(c) => c,
        None => dflt_coll(p),
    }
}

/// `sqlite3ExprCollSeqMatch(pParse, pE1, pE2)`
pub fn sqlite3_expr_coll_seq_match(p: &mut Parse, e1: ExprId, e2: ExprId) -> bool {
    let c1 = sqlite3_expr_nn_coll_seq(p, e1);
    let c2 = sqlite3_expr_nn_coll_seq(p, e2);
    c1.name.eq_ignore_ascii_case(&c2.name)
}

/// `sqlite3CompareAffinity(pExpr, aff2)`: the type affinity that should be used for the
/// comparison operator when `e` is one operand and the other operand has affinity `aff2`.
pub fn sqlite3_compare_affinity(p: &Parse, e: ExprId, aff2: u8) -> u8 {
    let aff1 = sqlite3_expr_affinity(p, e);
    if aff1 > SQLITE_AFF_NONE && aff2 > SQLITE_AFF_NONE {
        // Both sides of the comparison are columns. If one has numeric affinity, use that.
        // Otherwise use no affinity.
        if is_numeric_affinity(aff1) || is_numeric_affinity(aff2) {
            SQLITE_AFF_NUMERIC
        } else {
            SQLITE_AFF_BLOB
        }
    } else {
        // One side is a column, the other is not. Use the column's affinity.
        (if aff1 <= SQLITE_AFF_NONE { aff2 } else { aff1 }) | SQLITE_AFF_NONE
    }
}

/// `comparisonAffinity(pExpr)`: the type affinity to apply to the operands of a comparison
/// operator (or the left operand and the sub-select result of IN) before comparing.
pub fn comparison_affinity(p: &Parse, e: ExprId) -> u8 {
    let x = p.e(e);
    let mut aff = sqlite3_expr_affinity(p, x.p_left);
    if x.p_right.nn() {
        aff = sqlite3_compare_affinity(p, x.p_right, aff);
    } else if x.use_x_select() {
        let el = p.s(x.x_select()).p_e_list;
        aff = sqlite3_compare_affinity(p, p.l(el).a[0].p_expr, aff);
    } else if aff == 0 {
        aff = SQLITE_AFF_BLOB;
    }
    aff
}

/// `sqlite3IndexAffinityOk(pExpr, idx_affinity)`
pub fn sqlite3_index_affinity_ok(p: &Parse, e: ExprId, idx_affinity: u8) -> bool {
    let aff = comparison_affinity(p, e);
    if aff < SQLITE_AFF_TEXT {
        return true;
    }
    if aff == SQLITE_AFF_TEXT {
        return idx_affinity == SQLITE_AFF_TEXT;
    }
    is_numeric_affinity(idx_affinity)
}

/// `binaryCompareP5(pExpr1, pExpr2, jumpIfNull)`: the P5 value that should be used for a
/// binary comparison opcode (OP_Eq, OP_Ge etc.) used to compare `e1` and `e2`.
pub fn binary_compare_p5(p: &Parse, e1: ExprId, e2: ExprId, jump_if_null: i32) -> u8 {
    let aff = sqlite3_expr_affinity(p, e2);
    sqlite3_compare_affinity(p, e1, aff) | (jump_if_null as u8)
}

/// `sqlite3BinaryCompareCollSeq(pParse, pLeft, pRight)`: the collation sequence that should be
/// used by a binary comparison operator comparing `left` and `right`. An explicit collating
/// sequence on either operand wins (left first); otherwise the left operand's collation is
/// used if it has one, else the right operand's.
pub fn sqlite3_binary_compare_coll_seq(p: &mut Parse, left: ExprId, right: ExprId) -> Option<Arc<CollSeq>> {
    if p.e(left).flags & EP_Collate != 0 {
        sqlite3_expr_coll_seq(p, left)
    } else if right.nn() && (p.e(right).flags & EP_Collate) != 0 {
        sqlite3_expr_coll_seq(p, right)
    } else {
        let c = sqlite3_expr_coll_seq(p, left);
        if c.is_none() { sqlite3_expr_coll_seq(p, right) } else { c }
    }
}

/// `sqlite3ExprCompareCollSeq(pParse, p)`: expression `e` is a comparison operator; return the
/// collation sequence that should be used, taking EP_Commuted into account.
pub fn sqlite3_expr_compare_coll_seq(p: &mut Parse, e: ExprId) -> Option<Arc<CollSeq>> {
    let (l, r) = (p.e(e).p_left, p.e(e).p_right);
    if p.e(e).has(EP_Commuted) {
        sqlite3_binary_compare_coll_seq(p, r, l)
    } else {
        sqlite3_binary_compare_coll_seq(p, l, r)
    }
}

/// `sqlite3ExprIsVector(pExpr)`: true if the expression is a vector of two or more fields.
pub fn sqlite3_expr_is_vector(p: &Parse, e: ExprId) -> bool {
    sqlite3_expr_vector_size(p, e) > 1
}

/// `sqlite3ExprVectorSize(pExpr)`: the number of result values of the expression (1 for a
/// scalar, the number of columns for a vector or a sub-select).
pub fn sqlite3_expr_vector_size(p: &Parse, e: ExprId) -> i32 {
    let x = p.e(e);
    let mut op = x.op;
    if op == TK_REGISTER {
        op = x.op2;
    }
    if op == TK_VECTOR {
        p.l(x.x_list()).n_expr()
    } else if op == TK_SELECT {
        p.l(p.s(x.x_select()).p_e_list).n_expr()
    } else {
        1
    }
}

/// `sqlite3VectorFieldSubexpr(pVector, i)`: the subexpression of `vector` that is field `i`.
pub fn sqlite3_vector_field_subexpr(p: &Parse, vector: ExprId, i: i32) -> ExprId {
    if sqlite3_expr_is_vector(p, vector) {
        let x = p.e(vector);
        if x.op == TK_SELECT || x.op2 == TK_SELECT {
            p.l(p.s(x.x_select()).p_e_list).a[i as usize].p_expr
        } else {
            p.l(x.x_list()).a[i as usize].p_expr
        }
    } else {
        vector
    }
}

/// `sqlite3ExprForVectorField(pParse, pVector, iField, nField)`: compute and return a new
/// Expr object which, when passed to sqlite3ExprCode(), will generate all necessary code to
/// compute the `i_field`-th column of the vector expression.
pub fn sqlite3_expr_for_vector_field(p: &mut Parse, mut vector: ExprId, i_field: i32, n_field: i32) -> ExprId {
    if p.e(vector).op == TK_SELECT {
        let ret = sqlite3_p_expr(p, TK_SELECT_COLUMN, ExprId::NULL, ExprId::NULL);
        let r = p.em(ret);
        r.flags |= EP_FullSize;
        r.i_table = n_field;
        r.i_column = i_field as i16;
        r.p_left = vector;
        ret
    } else {
        if p.e(vector).op == TK_VECTOR {
            vector = p.l(p.e(vector).x_list()).a[i_field as usize].p_expr;
        }
        sqlite3_expr_dup(p, vector, 0)
    }
}

/// `sqlite3ExprCheckHeight(pParse, nHeight)`: check that `n_height` does not exceed the
/// maximum permitted depth of an expression tree.
pub fn sqlite3_expr_check_height(p: &mut Parse, n_height: i32) -> i32 {
    let mx_height = p.db.a_limit[SQLITE_LIMIT_EXPR_DEPTH as usize];
    if n_height > mx_height {
        p.error_msg(format!("Expression tree is too large (maximum depth {mx_height})"));
        return SQLITE_ERROR;
    }
    SQLITE_OK
}

fn height_of_expr(p: &Parse, e: ExprId, pn_height: &mut i32) {
    if e.nn() && p.e(e).n_height > *pn_height {
        *pn_height = p.e(e).n_height;
    }
}
fn height_of_expr_list(p: &Parse, l: ExprListId, pn_height: &mut i32) {
    if l.nn() {
        for it in &p.l(l).a {
            height_of_expr(p, it.p_expr, pn_height);
        }
    }
}
fn height_of_select(p: &Parse, sel: SelectId, pn_height: &mut i32) {
    let mut s = sel;
    while s.nn() {
        let x = p.s(s);
        height_of_expr(p, x.p_where, pn_height);
        height_of_expr(p, x.p_having, pn_height);
        height_of_expr(p, x.p_limit, pn_height);
        height_of_expr_list(p, x.p_e_list, pn_height);
        height_of_expr_list(p, x.p_group_by, pn_height);
        height_of_expr_list(p, x.p_order_by, pn_height);
        s = x.p_prior;
    }
}

/// `exprSetHeight(p)`: set the Expr.nHeight variable using the sub-expressions, and propagate
/// EP_Propagate flags from the expression list.
fn expr_set_height(p: &mut Parse, e: ExprId) {
    let x = p.e(e);
    let mut n_height = if x.p_left.nn() { p.e(x.p_left).n_height } else { 0 };
    if x.p_right.nn() && p.e(x.p_right).n_height > n_height {
        n_height = p.e(x.p_right).n_height;
    }
    if x.use_x_select() {
        height_of_select(p, x.x_select(), &mut n_height);
    } else if x.x_list().nn() {
        let l = x.x_list();
        height_of_expr_list(p, l, &mut n_height);
        let f = EP_Propagate & sqlite3_expr_list_flags(p, l);
        p.em(e).flags |= f;
    }
    p.em(e).n_height = n_height + 1;
}

/// `sqlite3ExprSetHeightAndFlags(pParse, p)`: set Expr.nHeight and propagate the EP_Propagate
/// flags; record an error if the tree is too deep.
pub fn sqlite3_expr_set_height_and_flags(p: &mut Parse, e: ExprId) {
    if p.n_err != 0 {
        return;
    }
    expr_set_height(p, e);
    let h = p.e(e).n_height;
    sqlite3_expr_check_height(p, h);
}

/// `sqlite3SelectExprHeight(p)`: the maximum height of any expression tree referenced by the
/// select statement.
pub fn sqlite3_select_expr_height(p: &Parse, sel: SelectId) -> i32 {
    let mut n_height = 0;
    height_of_select(p, sel, &mut n_height);
    n_height
}

/// `sqlite3ExprSetErrorOffset(pExpr, iOfst)`
pub fn sqlite3_expr_set_error_offset(p: &mut Parse, e: ExprId, i_ofst: i32) {
    if e.is_null() {
        return;
    }
    p.em(e).w = i_ofst;
}

/// `sqlite3ExprAlloc(db, op, pToken, dequote)`: allocate a new expression node from a token.
pub fn sqlite3_expr_alloc(p: &mut Parse, op: u8, token: Option<&Token>, dequote: i32) -> ExprId {
    let z = token.map(|t| p.tok_str(t).to_owned());
    sqlite3_expr_alloc_str(p, op, z.as_deref(), dequote)
}
/// `sqlite3ExprAlloc()` with the token text given as a string.
pub fn sqlite3_expr_alloc_str(p: &mut Parse, op: u8, z_token: Option<&str>, dequote: i32) -> ExprId {
    let mut e = Expr { op, i_agg: -1, n_height: 1, ..Expr::default() };
    if let Some(z) = z_token {
        if dequote != 0 && !z.is_empty() && is_quote(z.as_bytes()[0]) {
            // sqlite3DequoteExpr(pNew)
            e.flags |= if z.as_bytes()[0] == b'"' { EP_Quoted | EP_DblQuoted } else { EP_Quoted };
            e.z_token = Some(sqlite3_dequote(z));
        } else {
            e.z_token = Some(z.to_owned());
        }
    }
    p.new_expr(e)
}

/// `sqlite3Expr(db, op, zToken)`: allocate a new expression node from a zero-terminated token
/// that has already been dequoted.
pub fn sqlite3_expr(p: &mut Parse, op: u8, z_token: Option<&str>) -> ExprId {
    // C builds a Token {z, strlen(z)}; with zToken==0 the token has n==0 but nExtra==1, so the
    // node gets an empty zToken.
    sqlite3_expr_alloc_str(p, op, Some(z_token.unwrap_or("")), 0)
}

/// `sqlite3ExprInt32(db, iVal)`: construct an expression node for a simple integer value.
pub fn sqlite3_expr_int32(p: &mut Parse, i_val: i32) -> ExprId {
    p.new_expr(Expr {
        op: TK_INTEGER,
        i_agg: -1,
        flags: EP_IntValue | EP_Leaf | if i_val != 0 { EP_IsTrue } else { EP_IsFalse },
        i_value: i_val,
        n_height: 1,
        ..Expr::default()
    })
}

/// `sqlite3ExprAttachSubtrees(db, pRoot, pLeft, pRight)`: attach subtrees `left` and `right`
/// to the Expr node `root`.
pub fn sqlite3_expr_attach_subtrees(p: &mut Parse, root: ExprId, left: ExprId, right: ExprId) {
    if root.is_null() {
        return;
    }
    if right.nn() {
        let (f, h) = (p.e(right).flags, p.e(right).n_height);
        let r = p.em(root);
        r.p_right = right;
        r.flags |= EP_Propagate & f;
        r.n_height = h + 1;
    } else {
        p.em(root).n_height = 1;
    }
    if left.nn() {
        let (f, h) = (p.e(left).flags, p.e(left).n_height);
        let r = p.em(root);
        r.p_left = left;
        r.flags |= EP_Propagate & f;
        if h >= r.n_height {
            r.n_height = h + 1;
        }
    }
}

/// `sqlite3PExpr(pParse, op, pLeft, pRight)`: allocate an Expr node which joins as many as two
/// subtrees.
pub fn sqlite3_p_expr(p: &mut Parse, op: u8, left: ExprId, right: ExprId) -> ExprId {
    let e = p.new_expr(Expr { op, i_agg: -1, ..Expr::default() });
    sqlite3_expr_attach_subtrees(p, e, left, right);
    let h = p.e(e).n_height;
    sqlite3_expr_check_height(p, h);
    e
}

/// `sqlite3PExprAddSelect(pParse, pExpr, pSelect)`: add `sel` to the Expr.x.pSelect field.
pub fn sqlite3_p_expr_add_select(p: &mut Parse, e: ExprId, sel: SelectId) {
    if e.nn() {
        let x = p.em(e);
        x.x = sel.0;
        x.flags |= EP_xIsSelect | EP_Subquery;
        sqlite3_expr_set_height_and_flags(p, e);
    }
}

/// `sqlite3ExprListToValues(pParse, nElem, pEList)`: expression list `e_list` is a list of
/// vector values; convert it into a compound SELECT ("VALUES(...),(...),(...)").
pub fn sqlite3_expr_list_to_values(p: &mut Parse, n_elem: i32, e_list: ExprListId) -> SelectId {
    let mut p_ret = SelectId::NULL;
    for ii in 0..p.l(e_list).a.len() {
        let e = p.l(e_list).a[ii].p_expr;
        let n_expr_elem = if p.e(e).op == TK_VECTOR { p.l(p.e(e).x_list()).n_expr() } else { 1 };
        if n_expr_elem != n_elem {
            p.error_msg(format!(
                "IN(...) element has {} term{} - expected {}",
                n_expr_elem,
                if n_expr_elem > 1 { "s" } else { "" },
                n_elem
            ));
            break;
        }
        let list = p.e(e).x_list();
        let p_sel = crate::select::sqlite3_select_new(
            p,
            list,
            SrcListId::NULL,
            ExprId::NULL,
            ExprListId::NULL,
            ExprId::NULL,
            ExprListId::NULL,
            SF_Values,
            ExprId::NULL,
        );
        p.em(e).x = 0;
        if p_sel.nn() {
            if p_ret.nn() {
                p.sm(p_sel).op = TK_ALL;
                p.sm(p_sel).p_prior = p_ret;
            }
            p_ret = p_sel;
        }
    }
    if p_ret.nn() && p.s(p_ret).p_prior.nn() {
        p.sm(p_ret).sel_flags |= SF_MultiValue;
    }
    p_ret
}

/// `sqlite3ExprAnd(pParse, pLeft, pRight)`: join two expressions using an AND operator. If
/// either expression is NULL, the other is returned; if one is provably FALSE (and neither
/// contains a function or comes from an ON clause) the result is the constant 0.
pub fn sqlite3_expr_and(p: &mut Parse, left: ExprId, right: ExprId) -> ExprId {
    if left.is_null() {
        right
    } else if right.is_null() {
        left
    } else {
        let f = p.e(left).flags | p.e(right).flags;
        if (f & (EP_OuterON | EP_InnerON | EP_IsFalse | EP_HasFunc)) == EP_IsFalse && !p.in_rename_object() {
            sqlite3_expr_int32(p, 0)
        } else {
            sqlite3_p_expr(p, TK_AND, left, right)
        }
    }
}

/// `sqlite3ExprFunction(pParse, pList, pToken, eDistinct)`: construct a new expression node
/// for a function with multiple arguments.
pub fn sqlite3_expr_function(p: &mut Parse, list: ExprListId, token: &Token, e_distinct: i32) -> ExprId {
    let e = sqlite3_expr_alloc(p, TK_FUNCTION, Some(token), 1);
    p.em(e).w = token.z as i32;
    if list.nn() && p.l(list).n_expr() > p.db.a_limit[SQLITE_LIMIT_FUNCTION_ARG as usize] && p.nested == 0 {
        let msg = format!("too many arguments on function {}", p.tok_str(token));
        p.error_msg_tok(msg, token);
    }
    p.em(e).x = list.0;
    p.em(e).flags |= EP_HasFunc;
    sqlite3_expr_set_height_and_flags(p, e);
    if e_distinct == SF_Distinct as i32 {
        p.em(e).flags |= EP_Distinct;
    }
    e
}

/// `sqlite3ExprOrderByAggregateError(pParse, p)`
pub fn sqlite3_expr_order_by_aggregate_error(p: &mut Parse, e: ExprId) {
    let msg = format!("ORDER BY may not be used with non-aggregate {}()", p.e(e).tok());
    p.error_msg_expr(msg, e);
}

/// `sqlite3ExprAddFunctionOrderBy(pParse, pExpr, pOrderBy)`: attach an ORDER BY clause to a
/// function call ("agg(args ORDER BY ...)"): the ORDER BY becomes a TK_ORDER node in pLeft.
pub fn sqlite3_expr_add_function_order_by(p: &mut Parse, e: ExprId, order_by: ExprListId) {
    if order_by.is_null() || e.is_null() {
        return;
    }
    if p.e(e).x_list().is_null() || p.l(p.e(e).x_list()).a.is_empty() {
        // Ignore ORDER BY on zero-argument aggregates
        return;
    }
    if p.e(e).has(EP_WinFunc) && p.w(p.e(e).y_win).e_frm_type != TK_FILTER {
        // IsWindowFunc(pExpr)
        sqlite3_expr_order_by_aggregate_error(p, e);
        return;
    }
    if p.l(order_by).n_expr() > p.db.a_limit[SQLITE_LIMIT_COLUMN as usize] {
        p.error_msg("too many terms in ORDER BY clause");
        return;
    }
    let ob = sqlite3_expr_alloc_str(p, TK_ORDER, None, 0);
    p.em(ob).x = order_by.0;
    p.em(e).p_left = ob;
    p.em(ob).flags |= EP_FullSize;
}

/// `sqlite3ExprFunctionUsable(pParse, pExpr, pDef)`: check whether the function may be used
/// in the current context (it may not if it comes from the schema and is direct-only or the
/// schema is not trusted).
pub fn sqlite3_expr_function_usable(p: &mut Parse, e: ExprId, def: &FuncDef) {
    if p.e(e).has(EP_FromDDL) || (p.prep_flags as i32 & SQLITE_PREPARE_FROM_DDL) != 0 {
        if (def.func_flags & SQLITE_FUNC_DIRECT) != 0 || (p.db.flags & SQLITE_TrustedSchema) == 0 {
            // If SQLITE_DBCONFIG_TRUSTED_SCHEMA is off, then functions used in a VIEW or
            // TRIGGER or within CHECK constraints and DEFAULT clauses, expression indexes,
            // partial indexes, or generated columns must be marked SQLITE_INNOCUOUS.
            let msg = format!("unsafe use of {}()", p.e(e).tok());
            p.error_msg_expr(msg, e);
        }
    }
}

/// `sqlite3ExprAssignVarNumber(pParse, pExpr, n)`: assign a variable number to an expression
/// that encodes a wildcard in the original SQL statement (`?`, `?NNN`, `:AAA`, `@AAA`, `$AAA`).
pub fn sqlite3_expr_assign_var_number(p: &mut Parse, e: ExprId, n: u32) {
    if e.is_null() {
        return;
    }
    let z = p.e(e).tok().to_owned();
    let zb = z.as_bytes();
    let x: i32;
    let mx = p.db.a_limit[SQLITE_LIMIT_VARIABLE_NUMBER as usize];
    if zb.len() == 1 {
        // Wildcard of the form "?". Assign the next variable number
        p.n_var += 1;
        x = p.n_var as i16 as i32;
    } else {
        let mut do_add = false;
        if zb[0] == b'?' {
            // Wildcard of the form "?nnn". Convert "nnn" to an integer and use it as the
            // variable number
            let i: i64;
            let b_ok: bool;
            if n == 2 {
                i = (zb[1] - b'0') as i64;
                b_ok = true;
            } else {
                let (v, rc) = sqliters_types::fp::sqlite_atoi64(&zb[1..n as usize]);
                i = v;
                b_ok = rc == 0;
            }
            if !b_ok || i < 1 || i > mx as i64 {
                p.error_msg(format!("variable number must be between ?1 and ?{mx}"));
                p.record_error_offset_of_expr(e);
                return;
            }
            x = i as i16 as i32;
            if x > p.n_var {
                p.n_var = x;
                do_add = true;
            } else if x < SQLITE_VNBMC * 64 {
                do_add = (p.a_vnbmc[(x >> 6) as usize] & (1u64 << (x & 63))) == 0;
            } else if p.p_v_list.num_to_name(x).is_none() {
                do_add = true;
            }
        } else {
            // Wildcards like ":aaa", "$aaa" or "@aaa". Reuse the same variable number as the
            // prior appearance of the same name, or if the name has never appeared before,
            // reuse the same variable number
            let mut y = p.p_v_list.name_to_num(&z);
            if y == 0 {
                p.n_var += 1;
                y = p.n_var as i16 as i32;
                do_add = true;
            }
            x = y;
        }
        if do_add {
            p.p_v_list.add(&z, x);
            if x < SQLITE_VNBMC * 64 {
                p.a_vnbmc[(x >> 6) as usize] |= 1u64 << (x & 63);
            }
        }
    }
    p.em(e).i_column = x as i16;
    if x > mx {
        p.error_msg("too many SQL variables");
        p.record_error_offset_of_expr(e);
    }
}

/// `dupedExprStructSize(p, flags)`: which EP_Reduced / EP_TokenOnly flag a duplicate of the
/// node gets (0 for a full-size copy).
fn duped_expr_struct_size(x: &Expr, flags: i32) -> u32 {
    if flags == 0 || x.has(EP_FullSize) {
        0
    } else if x.p_left.nn() || x.x != 0 {
        EP_Reduced
    } else {
        EP_TokenOnly
    }
}

/// `exprDup(db, p, dupFlags, pEdupBuf)`: a deep copy of the expression. With
/// `EXPRDUP_REDUCE` the copy keeps only the leading part of each node (C allocates
/// `EXPR_REDUCEDSIZE` / `EXPR_TOKENONLYSIZE` bytes): the fields that C does not copy are zero
/// here and the node is flagged `EP_Reduced` / `EP_TokenOnly`.
fn expr_dup(p: &mut Parse, e: ExprId, dup_flags: i32) -> ExprId {
    let old = p.e(e).clone();
    let n_struct = duped_expr_struct_size(&old, dup_flags);
    let mut new = if dup_flags != 0 {
        match n_struct {
            0 => old.clone(),
            EP_Reduced => Expr {
                // offsetof(Expr,iTable): op, affExpr, op2, flags, u, pLeft, pRight, x, nHeight
                op: old.op,
                aff_expr: old.aff_expr,
                op2: old.op2,
                flags: old.flags,
                z_token: old.z_token.clone(),
                z_token_raw: old.z_token_raw.clone(),
                i_value: old.i_value,
                p_left: old.p_left,
                p_right: old.p_right,
                x: old.x,
                n_height: old.n_height,
                ..Expr::default()
            },
            _ => Expr {
                // offsetof(Expr,pLeft): op, affExpr, op2, flags, u
                op: old.op,
                aff_expr: old.aff_expr,
                op2: old.op2,
                flags: old.flags,
                z_token: old.z_token.clone(),
                z_token_raw: old.z_token_raw.clone(),
                i_value: old.i_value,
                ..Expr::default()
            },
        }
    } else {
        // exprStructSize(p): a reduced node is copied as far as it exists; the rest of the
        // new (full-size) node is zero. Reduced nodes already have zeros there.
        old.clone()
    };
    new.flags &= !(EP_Reduced | EP_TokenOnly | EP_Static);
    new.flags |= n_struct;
    // Fill in pLeft, pRight, x of the copy
    new.p_left = ExprId::NULL;
    new.p_right = ExprId::NULL;
    new.x = 0;
    if old.has(EP_IntValue) {
        new.z_token = None;
    }
    let id = p.new_expr(new);
    if ((old.flags | p.e(id).flags) & (EP_TokenOnly | EP_Leaf)) == 0 {
        // Fill in the pNew->x.pSelect or pNew->x.pList member.
        if old.use_x_select() {
            let s = sqlite3_select_dup(p, old.x_select(), dup_flags);
            p.em(id).x = s.0;
        } else {
            let l = sqlite3_expr_list_dup(p, old.x_list(), if old.op != TK_ORDER { dup_flags } else { 0 });
            p.em(id).x = l.0;
        }
        if old.has(EP_WinFunc) {
            let wn = crate::window::sqlite3_window_dup(p, id, old.y_win);
            p.em(id).y_win = wn;
        }
        if old.op == TK_SELECT_COLUMN {
            p.em(id).p_left = old.p_left;
        } else {
            let l = if old.p_left.nn() { expr_dup(p, old.p_left, dup_flags) } else { ExprId::NULL };
            p.em(id).p_left = l;
        }
        let r = if old.p_right.nn() { expr_dup(p, old.p_right, dup_flags) } else { ExprId::NULL };
        p.em(id).p_right = r;
    }
    id
}

/// `sqlite3ExprDup(db, p, flags)`: a deep copy of the expression tree (`flags` is 0 or
/// `EXPRDUP_REDUCE`).
pub fn sqlite3_expr_dup(p: &mut Parse, e: ExprId, flags: i32) -> ExprId {
    if e.nn() { expr_dup(p, e, flags) } else { ExprId::NULL }
}

/// `sqlite3ExprListDup(db, p, flags)`
pub fn sqlite3_expr_list_dup(p: &mut Parse, list: ExprListId, flags: i32) -> ExprListId {
    if list.is_null() {
        return ExprListId::NULL;
    }
    let old = p.l(list).clone();
    let mut new = ExprList { a: Vec::with_capacity(old.a.len()) };
    let mut prior_select_col_old = ExprId::NULL;
    let mut prior_select_col_new = ExprId::NULL;
    for old_item in &old.a {
        let old_expr = old_item.p_expr;
        let new_expr = sqlite3_expr_dup(p, old_expr, flags);
        if old_expr.nn() && p.e(old_expr).op == TK_SELECT_COLUMN && new_expr.nn() {
            if p.e(new_expr).p_right.nn() {
                prior_select_col_old = p.e(old_expr).p_right;
                prior_select_col_new = p.e(new_expr).p_right;
                let r = p.e(new_expr).p_right;
                p.em(new_expr).p_left = r;
            } else {
                if p.e(old_expr).p_left != prior_select_col_old {
                    prior_select_col_old = p.e(old_expr).p_left;
                    prior_select_col_new = sqlite3_expr_dup(p, prior_select_col_old, flags);
                    p.em(new_expr).p_right = prior_select_col_new;
                }
                p.em(new_expr).p_left = prior_select_col_new;
            }
        }
        let mut it = old_item.clone();
        it.p_expr = new_expr;
        new.a.push(it);
    }
    p.new_list(new)
}

/// `sqlite3SrcListDup(db, p, flags)`
pub fn sqlite3_src_list_dup(p: &mut Parse, src: SrcListId, flags: i32) -> SrcListId {
    if src.is_null() {
        return SrcListId::NULL;
    }
    let old = p.src(src).clone();
    let mut new = SrcList { a: Vec::with_capacity(old.a.len()) };
    for old_item in &old.a {
        let mut it = old_item.clone();
        if let Some(sq) = &old_item.p_subq {
            let mut nsq = sq.clone();
            nsq.p_select = sqlite3_select_dup(p, sq.p_select, flags);
            it.p_subq = Some(nsq);
        }
        if it.is_tab_func {
            it.p_func_arg = sqlite3_expr_list_dup(p, old_item.p_func_arg, flags);
        }
        if it.is_cte && it.p_cte_use != 0 {
            // pNewItem->u2.pCteUse->nUse++
            p.a_cte_use[it.p_cte_use as usize - 1].n_use += 1;
        }
        if old_item.p_s_tab.nn() {
            p.a_tab[old_item.p_s_tab.ix()].n_tab_ref += 1;
        }
        if !old_item.is_using {
            it.p_on = sqlite3_expr_dup(p, old_item.p_on, flags);
        }
        new.a.push(it);
    }
    p.new_src(new)
}

/// `sqlite3SelectDup(db, pDup, flags)`
pub fn sqlite3_select_dup(p: &mut Parse, dup: SelectId, flags: i32) -> SelectId {
    let mut p_ret = SelectId::NULL;
    let mut p_next = SelectId::NULL;
    let mut prev_new = SelectId::NULL;
    let mut s = dup;
    while s.nn() {
        let old = p.s(s).clone();
        let n = Select {
            p_e_list: sqlite3_expr_list_dup(p, old.p_e_list, flags),
            p_src: sqlite3_src_list_dup(p, old.p_src, flags),
            p_where: sqlite3_expr_dup(p, old.p_where, flags),
            p_group_by: sqlite3_expr_list_dup(p, old.p_group_by, flags),
            p_having: sqlite3_expr_dup(p, old.p_having, flags),
            p_order_by: sqlite3_expr_list_dup(p, old.p_order_by, flags),
            op: old.op,
            p_next,
            p_prior: SelectId::NULL,
            p_limit: sqlite3_expr_dup(p, old.p_limit, flags),
            i_limit: 0,
            i_offset: 0,
            sel_flags: old.sel_flags,
            n_select_row: old.n_select_row,
            p_with: sqlite3_with_dup(p, old.p_with),
            p_win: WindowId::NULL,
            p_win_defn: WindowId::NULL,
            sel_id: old.sel_id,
        };
        let id = p.new_select(n);
        let defn = crate::window::sqlite3_window_list_dup(p, old.p_win_defn);
        p.sm(id).p_win_defn = defn;
        if old.p_win.nn() {
            gather_select_windows(p, id);
        }
        if prev_new.nn() {
            p.sm(prev_new).p_prior = id;
        } else {
            p_ret = id;
        }
        prev_new = id;
        p_next = id;
        s = old.p_prior;
    }
    p_ret
}

/// `gatherSelectWindowsCallback(pWalker, pExpr)`
fn gather_select_windows_callback(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).op == TK_FUNCTION && p.e(e).has(EP_WinFunc) {
        let win = p.e(e).y_win;
        debug_assert!(p.w(win).pp_this == WinPpThis::None);
        crate::window::sqlite3_window_link(p, w.p_select, win);
    }
    WRC_Continue
}
/// `gatherSelectWindowsSelectCallback(pWalker, p)`
fn gather_select_windows_select_callback(_p: &mut Parse, w: &mut Walker, s: SelectId) -> i32 {
    if s == w.p_select { WRC_Continue } else { WRC_Prune }
}
/// `gatherSelectWindows(p)`: scan all the expressions of a newly duplicated SELECT statement
/// and gather all of the Window objects found there, assembling them onto the linked list at
/// Select->pWin.
fn gather_select_windows(p: &mut Parse, sel: SelectId) {
    let mut w = Walker {
        x_expr_callback: Some(gather_select_windows_callback),
        x_select_callback: Some(gather_select_windows_select_callback),
        has_parse: false,
        p_select: sel,
        ..Walker::default()
    };
    sqlite3_walk_select(p, &mut w, sel);
}

/// `sqlite3WithDup(db, p)`
pub fn sqlite3_with_dup(p: &mut Parse, with: WithId) -> WithId {
    if with.is_null() {
        return WithId::NULL;
    }
    let old = p.a_with[with.ix()].clone();
    let mut new = With { b_view: 0, p_outer: WithId::NULL, a: Vec::with_capacity(old.a.len()) };
    for c in &old.a {
        new.a.push(Cte {
            p_select: sqlite3_select_dup(p, c.p_select, 0),
            p_cols: sqlite3_expr_list_dup(p, c.p_cols, 0),
            z_name: c.z_name.clone(),
            e_m10d: c.e_m10d,
            z_cte_err: None,
            p_use: 0,
        });
    }
    p.a_with.push(new);
    WithId(p.a_with.len() as u32 - 1)
}

/// `sqlite3ExprListAppend(pParse, pList, pExpr)`: add a new element to the end of an
/// expression list; a NULL list is created.
pub fn sqlite3_expr_list_append(p: &mut Parse, list: ExprListId, e: ExprId) -> ExprListId {
    let list = if list.is_null() { p.new_list(ExprList::default()) } else { list };
    p.lm(list).a.push(ExprListItem { p_expr: e, ..ExprListItem::default() });
    list
}

/// `sqlite3ExprListSetSortOrder(p, iSortOrder, eNulls)`: set the sort order for the last
/// element on the given ExprList.
pub fn sqlite3_expr_list_set_sort_order(p: &mut Parse, list: ExprListId, mut i_sort_order: i32, e_nulls: i32) {
    if list.is_null() {
        return;
    }
    let item = p.lm(list).a.last_mut().expect("non-empty list");
    if i_sort_order == SQLITE_SO_UNDEFINED {
        i_sort_order = SQLITE_SO_ASC;
    }
    item.sort_flags = i_sort_order as u8;
    if e_nulls != SQLITE_SO_UNDEFINED {
        item.b_nulls = true;
        if i_sort_order != e_nulls {
            item.sort_flags |= sqliters_vdbe::vdbeaux::KEYINFO_ORDER_BIGNULL;
        }
    }
}

/// `sqlite3ExprListSetName(pParse, pList, pName, dequote)`: set the ExprList.a[].zEName
/// element of the most recently added item on the list to `name`.
pub fn sqlite3_expr_list_set_name(p: &mut Parse, list: ExprListId, name: &Token, dequote: i32) {
    if list.nn() {
        let mut z = p.tok_str(name).to_owned();
        if dequote != 0 {
            // If dequote==0, then pName->z does not point to part of a DDL statement, so
            // there is no need to (and it is not possible to) update the rename-token map.
            z = sqlite3_dequote(&z);
        }
        let item = p.lm(list).a.last_mut().expect("non-empty list");
        item.z_e_name = Some(z);
    }
}

/// `sqlite3ExprListSetSpan(pParse, pList, zStart, zEnd)`: set the ExprList.a[].zEName element
/// of the most recently added item to the text between offsets `z_start` and `z_end` of the
/// statement, with leading and trailing space removed (`sqlite3DbSpanDup`).
pub fn sqlite3_expr_list_set_span(p: &mut Parse, list: ExprListId, z_start: i32, z_end: i32) {
    if list.nn() {
        if p.l(list).a.last().expect("non-empty list").z_e_name.is_some() {
            return;
        }
        let b = p.z_sql.as_bytes();
        let mut a = (z_start.max(0) as usize).min(b.len());
        let mut e = (z_end.max(0) as usize).min(b.len());
        while a < e && is_space(b[a]) {
            a += 1;
        }
        while e > a && is_space(b[e - 1]) {
            e -= 1;
        }
        let z = String::from_utf8_lossy(&b[a..e]).into_owned();
        let item = p.lm(list).a.last_mut().expect("non-empty list");
        item.z_e_name = Some(z);
        item.e_e_name = ENAME_SPAN as u8;
    }
}

/// `sqlite3ExprListCheckLength(pParse, pEList, zObject)`: if the expression list contains more
/// than SQLITE_LIMIT_COLUMN entries, leave an error message.
pub fn sqlite3_expr_list_check_length(p: &mut Parse, e_list: ExprListId, z_object: &str) {
    let mx = p.db.a_limit[SQLITE_LIMIT_COLUMN as usize];
    if e_list.nn() && p.l(e_list).n_expr() > mx {
        p.error_msg(format!("too many columns in {z_object}"));
    }
}

/// `sqlite3ExprListFlags(pList)`: the bitwise-OR of all Expr.flags fields in the list.
pub fn sqlite3_expr_list_flags(p: &Parse, list: ExprListId) -> u32 {
    let mut m = 0;
    for it in &p.l(list).a {
        m |= p.e(it.p_expr).flags;
    }
    m
}

/// `sqlite3SelectWalkFail(pWalker, NotUsed)`: a select callback that always aborts the walk.
pub fn sqlite3_select_walk_fail(_p: &mut Parse, w: &mut Walker, _s: SelectId) -> i32 {
    w.e_code = 0;
    WRC_Abort
}

/// `sqlite3IsTrueOrFalse(zIn)`: EP_IsTrue for "true", EP_IsFalse for "false", else 0.
pub fn sqlite3_is_true_or_false(z_in: &str) -> u32 {
    if str_ieq(z_in, "true") {
        return EP_IsTrue;
    }
    if str_ieq(z_in, "false") {
        return EP_IsFalse;
    }
    0
}

/// `sqlite3ExprIdToTrueFalse(pExpr)`: if the input expression is an ID with the name "true" or
/// "false" then convert it into an appropriate TK_TRUEFALSE term. Returns true on conversion.
pub fn sqlite3_expr_id_to_true_false(p: &mut Parse, e: ExprId) -> bool {
    let x = p.e(e);
    if !x.has(EP_Quoted | EP_IntValue) {
        let v = sqlite3_is_true_or_false(x.tok());
        if v != 0 {
            let x = p.em(e);
            x.op = TK_TRUEFALSE;
            x.flags |= v;
            return true;
        }
    }
    false
}

/// `sqlite3ExprTruthValue(pExpr)`: the input expression must be a TK_TRUEFALSE; return 1 if it
/// is TRUE and 0 if it is FALSE.
pub fn sqlite3_expr_truth_value(p: &Parse, e: ExprId) -> i32 {
    let e = sqlite3_expr_skip_collate_and_likely(p, e);
    (p.e(e).tok().len() == 4) as i32
}

/// `sqlite3ExprSimplifiedAndOr(pExpr)`: if `e` is an AND or OR expression, try to simplify it
/// by eliminating terms that are always true or false. The result is either the original
/// expression or one of its subexpressions.
pub fn sqlite3_expr_simplified_and_or(p: &Parse, e: ExprId) -> ExprId {
    let x = p.e(e);
    if x.op == TK_AND || x.op == TK_OR {
        let right = sqlite3_expr_simplified_and_or(p, x.p_right);
        let left = sqlite3_expr_simplified_and_or(p, x.p_left);
        if p.e(left).always_true() || p.e(right).always_false() {
            return if x.op == TK_AND { right } else { left };
        } else if p.e(right).always_true() || p.e(left).always_false() {
            return if x.op == TK_AND { left } else { right };
        }
    }
    e
}

/// `exprNodeIsConstantFunction(pWalker, pExpr)`: `e` is a TK_FUNCTION node; return WRC_Prune
/// if the function is pure with constant arguments, else clear `eCode` and abort.
fn expr_node_is_constant_function(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    let n;
    let x = p.e(e);
    if x.has(EP_TokenOnly) || x.x_list().is_null() {
        n = 0;
    } else {
        let list = x.x_list();
        n = p.l(list).n_expr();
        sqlite3_walk_expr_list(p, w, list);
        if w.e_code == 0 {
            return WRC_Abort;
        }
    }
    let name = p.e(e).tok().to_owned();
    let def = sqlite3_find_function(p, &name, n, 0);
    match def {
        Some(d) if !is_aggregate_def(&d) && (d.func_flags & (SQLITE_FUNC_CONSTANT | SQLITE_FUNC_SLOCHNG)) != 0 => {
            WRC_Prune
        }
        _ => {
            w.e_code = 0;
            WRC_Abort
        }
    }
}

/// `exprNodeIsConstant(pWalker, pExpr)`: the walker callback behind `sqlite3ExprIsConstant()`
/// and friends. `pWalker->eCode` is the kind of constancy being tested (1..6), cleared to 0
/// when the expression turns out not to be constant.
fn expr_node_is_constant(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    // If pWalker->eCode is 2 then any term of the expression that comes from the ON or USING
    // clauses of an outer join disqualifies the expression from being considered constant.
    if w.e_code == 2 && p.e(e).has(EP_OuterON) {
        w.e_code = 0;
        return WRC_Abort;
    }
    let op = p.e(e).op;
    // The C switch falls through from TK_ID to the column cases and from those to the
    // "never constant" cases.
    let mut stage = match op {
        TK_FUNCTION => {
            if p.e(e).has(EP_WinFunc) {
                w.e_code = 0;
                return WRC_Abort;
            } else if w.e_code >= 4 && w.e_code <= 5 {
                if w.e_code == 5 {
                    p.em(e).flags |= EP_FromDDL;
                }
                return WRC_Continue;
            } else if p.e(e).has(EP_ConstFunc) {
                return WRC_Continue;
            } else if w.has_parse {
                return expr_node_is_constant_function(p, w, e);
            } else {
                w.e_code = 0;
                return WRC_Abort;
            }
        }
        TK_ID => 0,
        TK_COLUMN | TK_AGG_FUNCTION | TK_AGG_COLUMN => 1,
        TK_IF_NULL_ROW | TK_REGISTER | TK_DOT | TK_RAISE => 2,
        TK_IS | TK_ISNOT => {
            if w.e_code == 6 {
                w.e_code = 0;
                return WRC_Abort;
            }
            return WRC_Continue;
        }
        TK_VARIABLE => {
            if w.e_code == 5 {
                // Silently convert bound parameters that appear inside of CREATE statements
                // into a NULL when parsing the CREATE statement text out of the
                // sqlite_schema table
                p.em(e).op = TK_NULL;
            } else if w.e_code == 4 {
                // A bound parameter in a CREATE statement that originates from
                // sqlite3_prepare() causes an error
                w.e_code = 0;
                return WRC_Abort;
            }
            return WRC_Continue;
        }
        _ => return WRC_Continue,
    };
    if stage == 0 {
        // Convert "true" or "false" in a DEFAULT clause into the appropriate TK_TRUEFALSE node
        if sqlite3_expr_id_to_true_false(p, e) {
            return WRC_Prune;
        }
        stage = 1;
    }
    if stage == 1 {
        if p.e(e).has(EP_FixedCol) && w.e_code != 2 {
            return WRC_Continue;
        }
        if w.e_code == 3 && p.e(e).i_table == w.n {
            return WRC_Continue;
        }
    }
    w.e_code = 0;
    WRC_Abort
}

/// `exprIsConst(pParse, p, initFlag)`
fn expr_is_const(p: &mut Parse, e: ExprId, init_flag: i32, has_parse: bool, i_cur: i32) -> i32 {
    let mut w = Walker {
        e_code: init_flag as u16,
        has_parse,
        x_expr_callback: Some(expr_node_is_constant),
        x_select_callback: Some(sqlite3_select_walk_fail),
        n: i_cur,
        ..Walker::default()
    };
    sqlite3_walk_expr(p, &mut w, e);
    w.e_code as i32
}

/// `sqlite3ExprIsConstant(pParse, p)`: walk an expression tree; return non-zero if the
/// expression is constant and 0 if it involves variables or function calls.
///
/// For the purposes of this function, a double-quoted string (ex: "abc") is considered a
/// variable but a single-quoted string (ex: 'abc') is a constant.
pub fn sqlite3_expr_is_constant(p: &mut Parse, e: ExprId) -> i32 {
    expr_is_const(p, e, 1, true, 0)
}
/// `sqlite3ExprIsConstant(0, p)`: the same test without access to the function table (any
/// function that is not already marked EP_ConstFunc makes the expression non-constant).
pub fn sqlite3_expr_is_constant_no_parse(p: &mut Parse, e: ExprId) -> i32 {
    expr_is_const(p, e, 1, false, 0)
}

/// `sqlite3ExprListIsConstant(pParse, pList, bNoIs)`
pub fn sqlite3_expr_list_is_constant(p: &mut Parse, list: ExprListId, b_no_is: bool) -> bool {
    let i_init = if b_no_is { 6 } else { 1 };
    for ii in 0..p.l(list).a.len() {
        let e = p.l(list).a[ii].p_expr;
        if expr_is_const(p, e, i_init, true, 0) == 0 {
            return false;
        }
    }
    true
}

/// `sqlite3ExprIsConstantNotJoin(pParse, p)`: like `sqlite3ExprIsConstant` but also return 0 if
/// any subexpression comes from the ON or USING clauses of an outer join.
pub fn sqlite3_expr_is_constant_not_join(p: &mut Parse, e: ExprId) -> i32 {
    expr_is_const(p, e, 2, true, 0)
}

/// `sqlite3ExprIsTableConstant(p, iCur, 0)`: return non-zero if the expression is constant
/// for every single row of the table with cursor `i_cur`.
pub fn sqlite3_expr_is_table_constant(p: &mut Parse, e: ExprId, i_cur: i32) -> i32 {
    sqlite3_expr_is_table_constant_subq(p, e, i_cur, false)
}

/// `exprSelectWalkTableConstant(pWalker, pSelect)`: Walker callback of
/// sqlite3ExprIsTableConstant() for sub-queries: an uncorrelated sub-query is constant.
fn expr_select_walk_table_constant(p: &mut Parse, w: &mut Walker, s: SelectId) -> i32 {
    if (p.s(s).sel_flags & SF_Correlated) != 0 {
        w.e_code = 0;
        return WRC_Abort;
    }
    WRC_Prune
}

/// `sqlite3ExprIsTableConstant(p, iCur, bAllowSubq)`: walk an expression tree. Return
/// non-zero if the expression is constant for any single row of the table with cursor iCur.
/// In other words, the expression must not refer to any non-deterministic function nor any
/// table other than iCur.
///
/// Consider uncorrelated subqueries to be constants if the bAllowSubq parameter is true.
pub fn sqlite3_expr_is_table_constant_subq(p: &mut Parse, e: ExprId, i_cur: i32, b_allow_subq: bool) -> i32 {
    let mut w = Walker {
        e_code: 3,
        has_parse: false,
        x_expr_callback: Some(expr_node_is_constant),
        x_select_callback: Some(if b_allow_subq { expr_select_walk_table_constant } else { sqlite3_select_walk_fail }),
        n: i_cur,
        ..Walker::default()
    };
    sqlite3_walk_expr(p, &mut w, e);
    w.e_code as i32
}

/// `sqlite3ExprIsSingleTableConstraint(pExpr, pSrcList, iSrc, bAllowSubq)`: check pExpr to
/// see if it is an constraint on the single data source pSrc = &pSrcList->a[iSrc]. In other
/// words, check to see if pExpr constrains pSrc but does not depend on any other tables or
/// data sources anywhere else in the query. Return true (non-zero) if pExpr is a constraint
/// on pSrc only.
///
/// This is an optimization. False negatives will perhaps cause slower queries, but false
/// positives will yield incorrect answers. So when in doubt, return 0.
///
/// To be an single-source constraint, the following must be true:
///
///   (1)  pExpr cannot refer to any table other than pSrc->iCursor.
///
///   (2a) pExpr cannot use subqueries unless the bAllowSubq parameter is true and the
///        subquery is non-correlated
///
///   (2b) pExpr cannot use non-deterministic functions.
///
///   (3)  pSrc cannot be part of the left operand for a RIGHT JOIN. (Is there some way to
///        relax this constraint?)
///
///   (4)  If pSrc is the right operand of a LEFT JOIN, then...
///         (4a)  pExpr must come from an ON clause..
///         (4b)  and specifically the ON clause associated with the LEFT JOIN.
///
///   (5)  If pSrc is the right operand of a LEFT JOIN or the left operand of a RIGHT JOIN,
///        then pExpr must be from the WHERE clause, not an ON clause.
///
///   (6) Either:
///
///       (6a) pExpr does not originate in an ON or USING clause, or
///
///       (6b) The ON or USING clause from which pExpr is derived is not to the left of a
///            RIGHT JOIN (or FULL JOIN).
///
/// Without this restriction, accepting pExpr as a single-table constraint might move the
/// the ON/USING filter expression from the left side of a RIGHT JOIN over to the right
/// side, which leads to incorrect answers. See also restriction (9) on push-down.
pub fn sqlite3_expr_is_single_table_constraint(p: &mut Parse, e: ExprId, src_list: SrcListId, i_src: usize, b_allow_subq: bool) -> bool {
    let (jointype, i_cursor) = {
        let it = &p.src(src_list).a[i_src];
        (it.jointype as i32, it.i_cursor)
    };
    if jointype & JT_LTORJ != 0 {
        return false; // rule (3)
    }
    if jointype & JT_LEFT != 0 {
        if !p.e(e).has(EP_OuterON) {
            return false; // rule (4a)
        }
        if p.e(e).w != i_cursor {
            return false; // rule (4b)
        }
    } else if p.e(e).has(EP_OuterON) {
        return false; // rule (5)
    }
    if p.e(e).has(EP_OuterON | EP_InnerON) // (6a)
        && (p.src(src_list).a[0].jointype as i32 & JT_LTORJ) != 0
    // Fast pre-test of (6b)
    {
        for jj in 0..i_src {
            if p.e(e).w == p.src(src_list).a[jj].i_cursor {
                if (p.src(src_list).a[jj].jointype as i32 & JT_LTORJ) != 0 {
                    return false; // restriction (6)
                }
                break;
            }
        }
    }
    // Rules (1), (2a), and (2b) handled by the following:
    sqlite3_expr_is_table_constant_subq(p, e, i_cursor, b_allow_subq) != 0
}

/// `exprImpliesNotNull(pParse, p, pNN, iTab, seenNot)`: return non-zero if Expr p can only
/// be true if pNN is not NULL.
///
/// Or if seenNot is true, return non-zero if Expr p can only be non-NULL if pNN is not NULL
fn expr_implies_not_null(p: &Parse, e: ExprId, nn: ExprId, i_tab: i32, mut seen_not: bool) -> bool {
    if sqlite3_expr_compare(p, true, e, nn, i_tab) == 0 {
        return p.e(nn).op != TK_NULL;
    }
    let x = p.e(e);
    match x.op {
        TK_IN => {
            if seen_not && x.has(EP_xIsSelect) {
                return false;
            }
            expr_implies_not_null(p, x.p_left, nn, i_tab, true)
        }
        TK_BETWEEN => {
            let list = x.x_list();
            if seen_not {
                return false;
            }
            if expr_implies_not_null(p, p.l(list).a[0].p_expr, nn, i_tab, true)
                || expr_implies_not_null(p, p.l(list).a[1].p_expr, nn, i_tab, true)
            {
                return true;
            }
            expr_implies_not_null(p, x.p_left, nn, i_tab, true)
        }
        TK_EQ | TK_NE | TK_LT | TK_LE | TK_GT | TK_GE | TK_PLUS | TK_MINUS | TK_BITOR | TK_LSHIFT | TK_RSHIFT | TK_CONCAT
        | TK_STAR | TK_REM | TK_BITAND | TK_SLASH => {
            if !matches!(x.op, TK_STAR | TK_REM | TK_BITAND | TK_SLASH) {
                seen_not = true;
            }
            if expr_implies_not_null(p, x.p_right, nn, i_tab, seen_not) {
                return true;
            }
            expr_implies_not_null(p, x.p_left, nn, i_tab, seen_not)
        }
        TK_SPAN | TK_COLLATE | TK_UPLUS | TK_UMINUS => expr_implies_not_null(p, x.p_left, nn, i_tab, seen_not),
        TK_TRUTH => {
            if seen_not {
                return false;
            }
            if x.op2 != TK_IS {
                return false;
            }
            expr_implies_not_null(p, x.p_left, nn, i_tab, true)
        }
        TK_BITNOT | TK_NOT => expr_implies_not_null(p, x.p_left, nn, i_tab, true),
        _ => false,
    }
}

/// `sqlite3ExprIsNotTrue(pExpr)`: return true if the boolean value of the expression is
/// always either FALSE or NULL.
fn sqlite3_expr_is_not_true(p: &mut Parse, e: ExprId) -> bool {
    if p.e(e).op == TK_NULL {
        return true;
    }
    if p.e(e).op == TK_TRUEFALSE && sqlite3_expr_truth_value(p, e) == 0 {
        return true;
    }
    matches!(sqlite3_expr_is_integer(p, e, false, false), Some(0))
}

/// `sqlite3ExprIsIIF(db, pExpr)`: return true if the expression is one of the following:
///
/// ```text
///    CASE WHEN x THEN y END
///    CASE WHEN x THEN y ELSE NULL END
///    CASE WHEN x THEN y ELSE false END
///    iif(x,y)
///    iif(x,y,NULL)
///    iif(x,y,false)
/// ```
fn sqlite3_expr_is_iif(p: &mut Parse, e: ExprId) -> bool {
    if p.e(e).op == TK_FUNCTION {
        let z = p.e(e).tok().to_owned();
        if !z.starts_with('i') && !z.starts_with('I') {
            return false;
        }
        let list = p.e(e).x_list();
        if list.is_null() {
            return false;
        }
        let n = p.l(list).n_expr();
        let Some(def) = sqlite3_find_function(p, &z, n, 0) else {
            return false;
        };
        if (def.func_flags & SQLITE_FUNC_INLINE) == 0 {
            return false;
        }
        if def.user_data != INLINEFUNC_iif as i64 {
            return false;
        }
    } else if p.e(e).op == TK_CASE {
        if p.e(e).p_left.nn() {
            return false;
        }
    } else {
        return false;
    }
    let list = p.e(e).x_list();
    if p.l(list).n_expr() == 2 {
        return true;
    }
    if p.l(list).n_expr() == 3 {
        let third = p.l(list).a[2].p_expr;
        if sqlite3_expr_is_not_true(p, third) {
            return true;
        }
    }
    false
}

/// `sqlite3ExprImpliesExpr(pParse, pE1, pE2, iTab)`: return true if we can prove the pE2
/// will always be true if pE1 is true. Return false if we cannot complete the proof or if
/// pE2 might be false. Examples:
///
/// ```text
///     pE1: x==5        pE2: x==5             Result: true
///     pE1: x>0         pE2: x==5             Result: false
///     pE1: x=21        pE2: x=21 OR y=43     Result: true
///     pE1: x!=123      pE2: x IS NOT NULL    Result: true
///     pE1: x!=?1       pE2: x IS NOT NULL    Result: true
///     pE1: x IS NULL   pE2: x IS NOT NULL    Result: false
///     pE1: x IS ?2     pE2: x IS NOT NULL    Result: false
///     pE1: iif(x,y)    pE2: x                Result: true
///     PE1: iif(x,y,0)  pE2: x                Result: true
/// ```
///
/// When comparing TK_COLUMN nodes between pE1 and pE2, if pE2 has Expr.iTable<0 then assume
/// a table number given by iTab.
///
/// If pParse is not NULL, then the values of bound variables in pE1 are compared against
/// literal values in pE2 and pParse->pVdbe->expmask is modified to cause a reprepare if the
/// values change (there are no bound values at the first prepare).
///
/// When in doubt, return false. Returning true might give a performance improvement.
/// Returning false might cause a performance reduction, but it will always give the correct
/// answer and is hence always safe.
pub fn sqlite3_expr_implies_expr(p: &mut Parse, e1: ExprId, e2: ExprId, i_tab: i32) -> bool {
    if sqlite3_expr_compare(p, true, e1, e2, i_tab) == 0 {
        return true;
    }
    if p.e(e2).op == TK_OR {
        let (l, r) = (p.e(e2).p_left, p.e(e2).p_right);
        if sqlite3_expr_implies_expr(p, e1, l, i_tab) || sqlite3_expr_implies_expr(p, e1, r, i_tab) {
            return true;
        }
    }
    if p.e(e2).op == TK_NOTNULL && expr_implies_not_null(p, e1, p.e(e2).p_left, i_tab, false) {
        return true;
    }
    if sqlite3_expr_is_iif(p, e1) {
        let first = p.l(p.e(e1).x_list()).a[0].p_expr;
        return sqlite3_expr_implies_expr(p, first, e2, i_tab);
    }
    false
}

/// `bothImplyNotNullRow(pWalker, pE1, pE2)`: this is a helper function to
/// impliesNotNullRow(). In this routine, set pWalker->eCode to one only if *both* of the
/// input expressions separately have the implies-not-null-row property.
fn both_imply_not_null_row(p: &mut Parse, w: &mut Walker, e1: ExprId, e2: ExprId) {
    if w.e_code == 0 {
        sqlite3_walk_expr(p, w, e1);
        if w.e_code != 0 {
            w.e_code = 0;
            sqlite3_walk_expr(p, w, e2);
        }
    }
}

/// `impliesNotNullRow(pWalker, pExpr)`: this is the Expr node callback for
/// sqlite3ExprImpliesNonNullRow().
///
/// If the expression node requires that the table at pWalker->iCur have one or more non-NULL
/// column, then set pWalker->eCode to 1 and abort. If the expression node will always
/// evaluate to TRUE, then return WRC_Prune. Otherwise, continue.
fn implies_not_null_row(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).has(EP_OuterON) {
        return WRC_Prune;
    }
    if p.e(e).has(EP_InnerON) && w.m_w_flags != 0 {
        // If iCur is used in an inner-join ON clause to the left of a RIGHT JOIN, that does
        // *not* mean that the table must be non-null. But it is difficult to check for that
        // condition precisely. To keep things simple, any use of iCur from any inner-join
        // is ignored while attempting to simplify a RIGHT JOIN.
        return WRC_Prune;
    }
    match p.e(e).op {
        TK_ISNOT | TK_ISNULL | TK_NOTNULL | TK_IS | TK_VECTOR | TK_FUNCTION | TK_TRUTH | TK_CASE => WRC_Prune,
        TK_COLUMN => {
            if w.n == p.e(e).i_table {
                w.e_code = 1;
                return WRC_Abort;
            }
            WRC_Prune
        }
        TK_OR | TK_AND => {
            // Both sides of an AND or OR must separately imply non-null-row. Consider these
            // cases:
            //    1.  NOT (x AND y)
            //    2.  x OR y
            // If only one of x or y is non-null-row, then the overall expression can be true
            // if the other arm is false (case 1) or true (case 2).
            let (l, r) = (p.e(e).p_left, p.e(e).p_right);
            both_imply_not_null_row(p, w, l, r);
            WRC_Prune
        }
        TK_IN => {
            // Beware of "x NOT IN ()" and "x NOT IN (SELECT 1 WHERE false)", both of which
            // can be true. But also be careful about subqueries.
            if p.e(e).use_x_list() && p.l(p.e(e).x_list()).n_expr() > 0 {
                let l = p.e(e).p_left;
                sqlite3_walk_expr(p, w, l);
            }
            WRC_Prune
        }
        TK_BETWEEN => {
            // In "x NOT BETWEEN y AND z" either x must be non-null-row or else both y and z
            // must be non-null row
            let l = p.e(e).p_left;
            sqlite3_walk_expr(p, w, l);
            let list = p.e(e).x_list();
            let (a, b) = (p.l(list).a[0].p_expr, p.l(list).a[1].p_expr);
            both_imply_not_null_row(p, w, a, b);
            WRC_Prune
        }
        // Virtual tables are allowed to use constraints like x=NULL. So a term of the form
        // x=y does not prove that y is not null if x is the column of a virtual table
        TK_EQ | TK_NE | TK_LT | TK_LE | TK_GT | TK_GE => {
            let (l, r) = (p.e(e).p_left, p.e(e).p_right);
            if (p.e(l).op == TK_COLUMN && p.e(l).y_tab.nn() && p.tab(p.e(l).y_tab).is_virtual())
                || (p.e(r).op == TK_COLUMN && p.e(r).y_tab.nn() && p.tab(p.e(r).y_tab).is_virtual())
            {
                return WRC_Prune;
            }
            WRC_Continue
        }
        _ => WRC_Continue,
    }
}

/// `sqlite3ExprImpliesNonNullRow(p, iTab, isRJ)`: return true (non-zero) if expression p can
/// only be true if at least one column of table iTab is non-null. In other words, if all
/// columns of iTab are NULL, then the p expression will be NULL or false.
///
/// This routine is used to check if a LEFT JOIN can be converted into an ordinary JOIN. The
/// p argument is the WHERE clause. If the WHERE clause requires that some column of the
/// right table of the LEFT JOIN be non-NULL, then the LEFT JOIN can be safely converted into
/// an ordinary join.
pub fn sqlite3_expr_implies_non_null_row(p: &mut Parse, e: ExprId, i_tab: i32, is_rj: bool) -> bool {
    let mut e = sqlite3_expr_skip_collate_and_likely(p, e);
    if e.is_null() {
        return false;
    }
    if p.e(e).op == TK_NOTNULL {
        e = p.e(e).p_left;
    } else {
        while p.e(e).op == TK_AND {
            let l = p.e(e).p_left;
            if sqlite3_expr_implies_non_null_row(p, l, i_tab, is_rj) {
                return true;
            }
            e = p.e(e).p_right;
        }
    }
    let mut w = Walker {
        x_expr_callback: Some(implies_not_null_row),
        x_select_callback: None,
        x_select_callback2: None,
        e_code: 0,
        m_w_flags: is_rj as u16,
        n: i_tab,
        ..Walker::default()
    };
    sqlite3_walk_expr(p, &mut w, e);
    w.e_code != 0
}

/// `exprIdxCover(pWalker, pExpr)`: check to see if there are references to columns in table
/// pWalker->u.pIdxCover->iCur can be satisfied using the index pWalker->u.pIdxCover->pIdx.
fn expr_idx_cover(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if p.e(e).op == TK_COLUMN
        && p.e(e).i_table == w.n
        && crate::schema::sqlite3_table_column_to_index(p.idx(w.p_idx), p.e(e).i_column as i32) < 0
    {
        w.e_code = 1;
        return WRC_Abort;
    }
    WRC_Continue
}

/// `sqlite3ExprCoveredByIndex(pExpr, iCur, pIdx)`: determine if an index pIdx on table with
/// cursor iCur contains will the expression pExpr. Return true if the index does cover the
/// expression and false if the pExpr expression references table columns that are not found
/// in the index pIdx.
///
/// An index covering an expression means that the expression can be evaluated using only
/// the index and without having to lookup the corresponding table entry.
pub fn sqlite3_expr_covered_by_index(p: &mut Parse, e: ExprId, i_cur: i32, idx: IndexId) -> bool {
    let mut w = Walker {
        x_expr_callback: Some(expr_idx_cover),
        x_select_callback: None,
        n: i_cur,
        p_idx: idx,
        ..Walker::default()
    };
    sqlite3_walk_expr(p, &mut w, e);
    w.e_code == 0
}

/// `exprNodeIsConstantOrGroupBy(pWalker, pExpr)`: sqlite3WalkExpr() callback used by
/// sqlite3ExprIsConstantOrGroupBy().
fn expr_node_is_constant_or_group_by(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    let group_by = w.p_group_by;
    // Check if pExpr is identical to any GROUP BY term. If so, consider it constant.
    for i in 0..p.l(group_by).a.len() {
        let g = p.l(group_by).a[i].p_expr;
        if sqlite3_expr_compare(p, false, e, g, -1) < 2 {
            let coll = sqlite3_expr_nn_coll_seq(p, g);
            if coll.name.eq_ignore_ascii_case("BINARY") {
                return WRC_Prune;
            }
        }
    }
    // Check if pExpr is a sub-select. If so, consider it variable.
    if p.e(e).use_x_select() {
        w.e_code = 0;
        return WRC_Abort;
    }
    expr_node_is_constant(p, w, e)
}

/// `sqlite3ExprIsConstantOrGroupBy(pParse, p, pGroupBy)`: walk the expression tree passed as
/// the first argument. Return non-zero if the expression consists entirely of constants or
/// copies of terms in `group_by` that sort with the BINARY collation sequence. Otherwise,
/// return 0.
///
/// This routine is used to determine if a term of the HAVING clause can be promoted into the
/// WHERE clause. In order for such a promotion to work, the value of the HAVING clause term
/// must be the same for all members of a "group". The requirement that the GROUP BY term must
/// be BINARY assumes that no other collating sequence will have a finer-grained grouping than
/// BINARY. In other words (A=B COLLATE binary) implies A=B in every other collating sequence.
pub fn sqlite3_expr_is_constant_or_group_by(p: &mut Parse, e: ExprId, group_by: ExprListId) -> i32 {
    let mut w = Walker {
        e_code: 1,
        x_expr_callback: Some(expr_node_is_constant_or_group_by),
        x_select_callback: None,
        p_group_by: group_by,
        ..Walker::default()
    };
    sqlite3_walk_expr(p, &mut w, e);
    w.e_code as i32
}

/// `sqlite3ExprIsConstantOrFunction(p, isInit)`: non-zero if the expression is constant or a
/// function call with constant arguments (used for DEFAULT values and CHECK constraints).
pub fn sqlite3_expr_is_constant_or_function(p: &mut Parse, e: ExprId, is_init: u8) -> i32 {
    expr_is_const(p, e, 4 + is_init as i32, false, 0)
}

/// `sqlite3DequoteNumber(pParse, p)`: `e` is a TK_QNUMBER token (a number containing digit
/// separators): remove the separators and reclassify it as TK_INTEGER or TK_FLOAT.
pub fn sqlite3_dequote_number(p: &mut Parse, e: ExprId) {
    if e.is_null() {
        return;
    }
    let z_in = p.e(e).tok().as_bytes().to_vec();
    let at = |i: isize| -> u8 { if i >= 0 && (i as usize) < z_in.len() { z_in[i as usize] } else { 0 } };
    let b_hex = at(0) == b'0' && (at(1) == b'x' || at(1) == b'X');
    let mut op = TK_INTEGER;
    let mut out: Vec<u8> = Vec::with_capacity(z_in.len());
    let mut bad = false;
    let sep = SQLITE_DIGIT_SEPARATOR as u8;
    for i in 0..z_in.len() as isize {
        let c = at(i);
        if c != sep {
            out.push(c);
            if c == b'e' || c == b'E' || c == b'.' {
                op = TK_FLOAT;
            }
        } else if (!b_hex && (!is_digit(at(i - 1)) || !is_digit(at(i + 1))))
            || (b_hex && (!is_xdigit(at(i - 1)) || !is_xdigit(at(i + 1))))
        {
            bad = true;
        }
    }
    if bad {
        // C reports the token as it stands at the first bad separator, i.e. partially
        // compacted; the text printed is the original zToken up to its NUL.
        let msg = format!("unrecognized token: \"{}\"", dequote_number_partial(&z_in, b_hex));
        p.error_msg(msg);
    }
    if b_hex {
        op = TK_INTEGER;
    }
    let z = String::from_utf8_lossy(&out).into_owned();
    let x = p.em(e);
    x.op = op;
    x.z_token = Some(z.clone());
    if op == TK_INTEGER {
        if let Some(v) = sqlite3_get_int32(&z) {
            x.i_value = v;
            x.flags |= EP_IntValue;
            x.z_token = None;
        }
    }
}

/// The text of `p->u.zToken` at the moment `sqlite3DequoteNumber()` reports a misplaced digit
/// separator: C compacts the token in place (pOut trails pIn), so when the *last* bad
/// separator is reported the buffer holds the compacted prefix followed by the not yet
/// overwritten tail of the original text.
fn dequote_number_partial(z_in: &[u8], b_hex: bool) -> String {
    let sep = SQLITE_DIGIT_SEPARATOR as u8;
    let at = |b: &[u8], i: isize| -> u8 { if i >= 0 && (i as usize) < b.len() { b[i as usize] } else { 0 } };
    let mut buf = z_in.to_vec();
    let mut out = 0usize;
    let mut last: Option<Vec<u8>> = None;
    for i in 0..z_in.len() as isize {
        // (the comparisons read the buffer being rewritten, as C does)
        let c = buf[i as usize];
        if c != sep {
            buf[out] = c;
            out += 1;
        } else if (!b_hex && (!is_digit(at(&buf, i - 1)) || !is_digit(at(&buf, i + 1))))
            || (b_hex && (!is_xdigit(at(&buf, i - 1)) || !is_xdigit(at(&buf, i + 1))))
        {
            last = Some(buf.clone());
        }
    }
    String::from_utf8_lossy(&last.unwrap_or(buf)).into_owned()
}

/// `sqlite3ExprIsInteger(p, &value, pParse, bRSI)`: if the expression is an integer constant
/// that fits in 32 bits, return it. With `use_parse` a bound parameter is looked at as well:
/// on a first prepare there is no bound value, so the result is `None`, but the statement is
/// marked for re-preparation when the parameter is bound (`sqlite3VdbeReprepareOnBind`).
pub fn sqlite3_expr_is_integer(p: &mut Parse, mut e: ExprId, mut use_parse: bool, _b_rsi: bool) -> Option<i32> {
    let mut i_sign = 1;
    loop {
        if e.is_null() {
            return None;
        }
        let x = p.e(e);
        if x.has(EP_IntValue) {
            return Some(x.i_value.wrapping_mul(i_sign));
        }
        if x.op == TK_UPLUS {
            e = x.p_left;
            use_parse = false;
            continue;
        }
        if x.op == TK_UMINUS {
            i_sign = -i_sign;
            e = x.p_left;
            use_parse = false;
            continue;
        }
        if x.op == TK_VARIABLE && use_parse {
            if (p.db.flags & SQLITE_EnableQPSG) != 0 {
                break;
            }
            // pVal = sqlite3VdbeGetBoundValue(pParse->pReprepare, ...) is NULL on a first prepare
            let i_var = x.i_column as i32;
            // sqlite3VdbeReprepareOnBind(v, iVar, bRSI)
            if i_var >= 32 {
                p.v.expmask |= 0x8000_0000;
            } else {
                p.v.expmask |= 1u32 << (i_var - 1);
            }
            return None;
        }
        break;
    }
    None
}

/// `sqlite3ExprCanBeNull(p)`: return FALSE if there is no chance that the expression can be
/// NULL. If the expression might be NULL or if the expression is too complex to tell return
/// TRUE.
pub fn sqlite3_expr_can_be_null(p: &Parse, mut e: ExprId) -> bool {
    while p.e(e).op == TK_UPLUS || p.e(e).op == TK_UMINUS {
        e = p.e(e).p_left;
    }
    let x = p.e(e);
    let mut op = x.op;
    if op == TK_REGISTER {
        op = x.op2;
    }
    match op {
        TK_INTEGER | TK_STRING | TK_FLOAT | TK_BLOB => false,
        TK_COLUMN => {
            x.has(EP_CanBeNull)
                || x.y_tab.is_null()
                || (x.i_column >= 0
                    && !p.tab(x.y_tab).a_col.is_empty()
                    && (x.i_column as usize) < p.tab(x.y_tab).a_col.len()
                    && p.tab(x.y_tab).a_col[x.i_column as usize].not_null == 0)
        }
        _ => true,
    }
}

/// `sqlite3ExprNeedsNoAffinityChange(p, aff)`: return TRUE if the given expression is a
/// constant which would be unchanged by OP_Affinity with the affinity given in the second
/// argument.
pub fn sqlite3_expr_needs_no_affinity_change(p: &Parse, mut e: ExprId, aff: u8) -> bool {
    let mut unary_minus = false;
    if aff == SQLITE_AFF_BLOB {
        return true;
    }
    while p.e(e).op == TK_UPLUS || p.e(e).op == TK_UMINUS {
        if p.e(e).op == TK_UMINUS {
            unary_minus = true;
        }
        e = p.e(e).p_left;
    }
    let x = p.e(e);
    let mut op = x.op;
    if op == TK_REGISTER {
        op = x.op2;
    }
    match op {
        TK_INTEGER => aff >= SQLITE_AFF_NUMERIC,
        TK_FLOAT => aff >= SQLITE_AFF_NUMERIC,
        TK_STRING => !unary_minus && aff == SQLITE_AFF_TEXT,
        TK_BLOB => !unary_minus,
        TK_COLUMN => aff >= SQLITE_AFF_NUMERIC && x.i_column < 0,
        _ => false,
    }
}

/// `sqlite3IsRowid(z)`: return TRUE if the given string is a row-id column name.
pub fn sqlite3_is_rowid(z: &str) -> bool {
    str_ieq(z, "_ROWID_") || str_ieq(z, "ROWID") || str_ieq(z, "OID")
}

/// `sqlite3ExprCompare(pParse, pA, pB, iTab)`: do a deep comparison of two expression trees.
/// Return 0 if the two expressions are identical, 1 if they are certainly different or if only
/// the COLLATE operators differ at the top level, 2 if they differ otherwise. `use_parse`
/// stands for a non-NULL pParse (bound parameters are then compared by value; on a first
/// prepare there are no bound values, so a parameter only equals the same parameter).
pub fn sqlite3_expr_compare(p: &Parse, use_parse: bool, a: ExprId, b: ExprId, i_tab: i32) -> i32 {
    expr_compare(p, use_parse, a, b, i_tab, None)
}

pub(crate) fn sqlite3_partial_bound_eq(
    p: &Parse, term: ExprId, partial: ExprId, i_tab: i32, binding: (usize, &Value),
) -> bool {
    let (term_expr, partial_expr) = (p.e(term), p.e(partial));
    term_expr.op == TK_EQ
        && partial_expr.op == TK_EQ
        && [term_expr.p_left, partial_expr.p_left].iter().all(|&id| id.nn() && p.e(id).op == TK_COLUMN)
        && term_expr.p_right.nn() && p.e(term_expr.p_right).op == TK_VARIABLE
        && p.e(term_expr.p_right).i_column as usize == binding.0
        && partial_expr.p_right.nn() && p.e(partial_expr.p_right).op == TK_INTEGER
        && p.e(partial_expr.p_right).has(EP_IntValue)
        && expr_compare(p, true, term, partial, i_tab, Some(binding)) == 0
}

fn expr_compare(p: &Parse, use_parse: bool, a: ExprId, b: ExprId, i_tab: i32, bound: Option<(usize, &Value)>) -> i32 {
    if a.is_null() || b.is_null() {
        return if a == b { 0 } else { 2 };
    }
    let (pa, pb) = (p.e(a), p.e(b));
    if use_parse && pa.op == TK_VARIABLE {
        // exprCompareVariable()
        if pb.op == TK_VARIABLE && pa.i_column == pb.i_column {
            return 0;
        }
        if (p.db.flags & SQLITE_EnableQPSG) != 0 {
            return 2;
        }
        if let Some(literal) = crate::build_tab::sqlite3_value_from_expr(p, b, SQLITE_AFF_BLOB) {
            // sqlite3VdbeReprepareOnBind(pParse->pVdbe, iVar, 0): binding a new value to the
            // variable may change the plan. Only a scoped reprepare can compare it to
            // the literal at compile time.
            let i_var = pa.i_column as i32;
            let m: u32 = if i_var >= 32 { 0x8000_0000 } else { 1u32 << (i_var - 1) };
            p.pending_expmask.set(p.pending_expmask.get() | m);
            if let Some((slot, value)) = bound && i_var as usize == slot {
                return if sqlite3_mem_compare(&Mem::from_value(value), &Mem::from_value(&literal), None) == 0 {
                    0
                } else {
                    2
                };
            }
        }
        return 2;
    }
    let combined_flags = pa.flags | pb.flags;
    if combined_flags & EP_IntValue != 0 {
        if (pa.flags & pb.flags & EP_IntValue) != 0 && pa.i_value == pb.i_value {
            return 0;
        }
        return 2;
    }
    if pa.op != pb.op || pa.op == TK_RAISE {
        if pa.op == TK_COLLATE && expr_compare(p, use_parse, pa.p_left, b, i_tab, bound) < 2 {
            return 1;
        }
        if pb.op == TK_COLLATE && expr_compare(p, use_parse, a, pb.p_left, i_tab, bound) < 2 {
            return 1;
        }
        if pa.op == TK_AGG_COLUMN && pb.op == TK_COLUMN && pb.i_table < 0 && pa.i_table == i_tab {
            // fall through
        } else {
            return 2;
        }
    }
    if let Some(za) = pa.z_token.as_deref() {
        if pa.op == TK_FUNCTION || pa.op == TK_AGG_FUNCTION {
            if !str_ieq(za, pb.tok()) {
                return 2;
            }
            if pa.has(EP_WinFunc) != pb.has(EP_WinFunc) {
                return 2;
            }
            if pa.has(EP_WinFunc) && crate::window::sqlite3_window_compare(p, use_parse, pa.y_win, pb.y_win, true) != 0 {
                return 2;
            }
        } else if pa.op == TK_NULL {
            return 0;
        } else if pa.op == TK_COLLATE {
            if !str_ieq(za, pb.tok()) {
                return 2;
            }
        } else if pb.z_token.is_some() && pa.op != TK_COLUMN && pa.op != TK_AGG_COLUMN && pa.tok_bytes() != pb.tok_bytes() {
            return 2;
        }
    }
    if (pa.flags & (EP_Distinct | EP_Commuted)) != (pb.flags & (EP_Distinct | EP_Commuted)) {
        return 2;
    }
    if (combined_flags & EP_TokenOnly) == 0 {
        if combined_flags & EP_xIsSelect != 0 {
            return 2;
        }
        if (combined_flags & EP_FixedCol) == 0 && expr_compare(p, use_parse, pa.p_left, pb.p_left, i_tab, bound) != 0 {
            return 2;
        }
        if expr_compare(p, use_parse, pa.p_right, pb.p_right, i_tab, bound) != 0 {
            return 2;
        }
        if sqlite3_expr_list_compare(p, pa.x_list(), pb.x_list(), i_tab) != 0 {
            return 2;
        }
        if pa.op != TK_STRING && pa.op != TK_TRUEFALSE && (combined_flags & EP_Reduced) == 0 {
            if pa.i_column != pb.i_column {
                return 2;
            }
            if pa.op2 != pb.op2 && pa.op == TK_TRUTH {
                return 2;
            }
            if pa.op != TK_IN && pa.i_table != pb.i_table && pa.i_table != i_tab {
                return 2;
            }
        }
    }
    0
}

/// `sqlite3ExprListCompare(pA, pB, iTab)`: compare two ExprList objects. Return 0 if they are
/// identical, 1 if they are certainly different, or 2 if it is not possible to determine if
/// they are identical or not.
pub fn sqlite3_expr_list_compare(p: &Parse, a: ExprListId, b: ExprListId, i_tab: i32) -> i32 {
    if a.is_null() && b.is_null() {
        return 0;
    }
    if a.is_null() || b.is_null() {
        return 1;
    }
    let (la, lb) = (p.l(a), p.l(b));
    if la.a.len() != lb.a.len() {
        return 1;
    }
    for i in 0..la.a.len() {
        if la.a[i].sort_flags != lb.a[i].sort_flags {
            return 1;
        }
        let res = sqlite3_expr_compare(p, false, la.a[i].p_expr, lb.a[i].p_expr, i_tab);
        if res != 0 {
            return res;
        }
    }
    0
}

/// `sqlite3ExprCompareSkip(pA, pB, iTab)`: like sqlite3ExprCompare() except COLLATE operators
/// at the top-level are ignored.
pub fn sqlite3_expr_compare_skip(p: &Parse, a: ExprId, b: ExprId, i_tab: i32) -> i32 {
    sqlite3_expr_compare(p, false, sqlite3_expr_skip_collate(p, a), sqlite3_expr_skip_collate(p, b), i_tab)
}

/// `struct RefSrcList` of `sqlite3ReferencesSrcList()`, kept in thread-local-free form inside
/// the walker: `p_src_list` is `pRef`, the excluded cursors live in this vector.
struct RefSrcList {
    ai_exclude: Vec<i32>,
}

/// `sqlite3ReferencesSrcList(pParse, pExpr, pSrcList)`: `e` is an aggregate function. Determine
/// if the arguments to the function reference `src_list`:
///
/// *  1 — the function references the FROM clause,
/// *  0 — it references only outer FROM clauses (or sub-select ones are excluded),
/// * -1 — it references no columns at all (e.g. `count(*)`).
pub fn sqlite3_references_src_list(p: &mut Parse, e: ExprId, src_list: SrcListId) -> i32 {
    let mut x = RefSrcList { ai_exclude: Vec::new() };
    let mut e_code = 0u16;
    fn walk_expr(p: &Parse, x: &mut RefSrcList, src: SrcListId, e_code: &mut u16, mut e: ExprId) {
        // sqlite3WalkExprNN with exprRefToSrcList / selectRefEnter / selectRefLeave
        while e.nn() {
            let ex = p.e(e);
            if ex.op == TK_COLUMN || ex.op == TK_AGG_COLUMN {
                let mut found = false;
                if src.nn() {
                    for it in &p.src(src).a {
                        if ex.i_table == it.i_cursor {
                            *e_code |= 1;
                            found = true;
                            break;
                        }
                    }
                }
                if !found && !x.ai_exclude.contains(&ex.i_table) {
                    *e_code |= 2;
                }
            }
            if ex.has(EP_TokenOnly | EP_Leaf) {
                break;
            }
            if ex.p_left.nn() {
                walk_expr(p, x, src, e_code, ex.p_left);
            }
            if ex.p_right.nn() {
                e = ex.p_right;
                continue;
            } else if ex.use_x_select() {
                walk_select(p, x, src, e_code, ex.x_select());
            } else {
                if ex.x_list().nn() {
                    walk_list(p, x, src, e_code, ex.x_list());
                }
                if ex.has(EP_WinFunc) {
                    // walkWindowList(pWalker, pExpr->y.pWin, 1)
                    let w = p.w(ex.y_win);
                    walk_list(p, x, src, e_code, w.p_order_by);
                    walk_list(p, x, src, e_code, w.p_partition);
                    walk_expr(p, x, src, e_code, w.p_filter);
                    walk_expr(p, x, src, e_code, w.p_start);
                    walk_expr(p, x, src, e_code, w.p_end);
                }
            }
            break;
        }
    }
    fn walk_list(p: &Parse, x: &mut RefSrcList, src: SrcListId, e_code: &mut u16, l: ExprListId) {
        if l.nn() {
            for it in &p.l(l).a {
                walk_expr(p, x, src, e_code, it.p_expr);
            }
        }
    }
    fn walk_select(p: &Parse, x: &mut RefSrcList, src: SrcListId, e_code: &mut u16, mut s: SelectId) {
        while s.nn() {
            let sel = p.s(s);
            // selectRefEnter: add all FROM clause cursors of the sub-select to the exclude list
            let n_src = p.src(sel.p_src).a.len();
            for it in &p.src(sel.p_src).a {
                x.ai_exclude.push(it.i_cursor);
            }
            walk_list(p, x, src, e_code, sel.p_e_list);
            walk_expr(p, x, src, e_code, sel.p_where);
            walk_list(p, x, src, e_code, sel.p_group_by);
            walk_expr(p, x, src, e_code, sel.p_having);
            walk_list(p, x, src, e_code, sel.p_order_by);
            walk_expr(p, x, src, e_code, sel.p_limit);
            for it in &p.src(sel.p_src).a {
                if let Some(sq) = &it.p_subq
                    && it.is_subquery
                {
                    walk_select(p, x, src, e_code, sq.p_select);
                }
                if it.is_tab_func {
                    walk_list(p, x, src, e_code, it.p_func_arg);
                }
            }
            // selectRefLeave
            let n = x.ai_exclude.len();
            if n > 0 {
                x.ai_exclude.truncate(n - n_src);
            }
            s = sel.p_prior;
        }
    }
    let list = p.e(e).x_list();
    walk_list(p, &mut x, src_list, &mut e_code, list);
    let left = p.e(e).p_left;
    if left.nn() {
        let l = p.e(left).x_list();
        walk_list(p, &mut x, src_list, &mut e_code, l);
    }
    if p.e(e).has(EP_WinFunc) {
        let f = p.w(p.e(e).y_win).p_filter;
        walk_expr(p, &mut x, src_list, &mut e_code, f);
    }
    if e_code & 0x01 != 0 {
        1
    } else if e_code != 0 {
        0
    } else {
        -1
    }
}

/// `findOrCreateAggInfoColumn(pParse, pAggInfo, pExpr)`: search the AggInfo object for an
/// aCol[] entry that has iTable and iColumn; create one if there is none. Convert `e` to be a
/// TK_AGG_COLUMN referring to that entry.
fn find_or_create_agg_info_column(p: &mut Parse, agg: AggInfoId, e: ExprId) {
    let mx_term = p.db.a_limit[SQLITE_LIMIT_COLUMN as usize];
    let (i_table, i_column, op) = (p.e(e).i_table, p.e(e).i_column as i32, p.e(e).op);
    let mut k = 0usize;
    let mut found = false;
    while k < p.agg(agg).a_col.len() {
        let c = &p.agg(agg).a_col[k];
        if c.p_c_expr == e {
            return;
        }
        if c.i_table == i_table && c.i_column == i_column && op != TK_IF_NULL_ROW {
            found = true;
            break;
        }
        k += 1;
    }
    if !found {
        p.aggm(agg).a_col.push(AggInfoCol::default());
        k = p.agg(agg).a_col.len() - 1;
        if k as i32 > mx_term {
            // Additional columns are used only as parameters to aggregate functions.
            p.error_msg(format!("more than {mx_term} aggregate terms"));
            k = mx_term as usize;
        }
        let y_tab = p.e(e).y_tab;
        {
            let c = &mut p.aggm(agg).a_col[k];
            c.p_tab = y_tab;
            c.i_table = i_table;
            c.i_column = i_column;
            c.i_sorter_column = -1;
            c.p_c_expr = e;
        }
        let gb = p.agg(agg).p_group_by;
        if gb.nn() && op != TK_IF_NULL_ROW {
            for j in 0..p.l(gb).a.len() {
                let pe = p.e(p.l(gb).a[j].p_expr);
                if pe.op == TK_COLUMN && pe.i_table == i_table && pe.i_column as i32 == i_column {
                    p.aggm(agg).a_col[k].i_sorter_column = j as i32;
                    break;
                }
            }
        }
        if p.agg(agg).a_col[k].i_sorter_column < 0 {
            let n = p.agg(agg).n_sorting_column;
            p.aggm(agg).a_col[k].i_sorter_column = n as i32;
            p.aggm(agg).n_sorting_column += 1;
        }
    }
    // fix_up_expr:
    let x = p.em(e);
    x.p_agg_info = agg;
    if x.op == TK_COLUMN {
        x.op = TK_AGG_COLUMN;
    }
    x.i_agg = k as i16;
}

/// `analyzeAggregate(pWalker, pExpr)`: the xExprCallback for a tree walker used to implement
/// sqlite3ExprAnalyzeAggregates(): add columns and aggregate functions to the AggInfo.
fn analyze_aggregate(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    let nc = w.p_nc;
    let src_list = p.nc(nc).p_src_list;
    let agg = p.nc(nc).p_agg_info;
    match p.e(e).op {
        TK_IF_NULL_ROW | TK_AGG_COLUMN | TK_COLUMN => {
            // Check to see if the column is in one of the tables in the FROM clause of the
            // aggregate query
            if src_list.nn() {
                let i_table = p.e(e).i_table;
                if p.src(src_list).a.iter().any(|it| it.i_cursor == i_table) {
                    find_or_create_agg_info_column(p, agg, e);
                } // endif pExpr->iTable==pItem->iCursor
            }
            WRC_Continue
        }
        TK_AGG_FUNCTION => {
            if (p.nc(nc).nc_flags & NC_InAggFunc) == 0
                && w.walker_depth == p.e(e).op2 as i32
                && p.e(e).p_agg_info.is_null()
            {
                // Check to see if pExpr is a duplicate of another aggregate function that is
                // already in the pAggInfo structure
                let mx_term = p.db.a_limit[SQLITE_LIMIT_COLUMN as usize];
                let mut i = 0usize;
                while i < p.agg(agg).a_func.len() {
                    let fe = p.agg(agg).a_func[i].p_f_expr;
                    if fe == e {
                        break;
                    }
                    if sqlite3_expr_compare(p, false, fe, e, -1) == 0 {
                        break;
                    }
                    i += 1;
                }
                if i as i32 > mx_term {
                    p.error_msg(format!("more than {mx_term} aggregate terms"));
                    i = mx_term as usize;
                } else if i >= p.agg(agg).a_func.len() {
                    // pExpr is original. Make a new entry in pAggInfo->aFunc[]
                    p.aggm(agg).a_func.push(AggInfoFunc::default());
                    i = p.agg(agg).a_func.len() - 1;
                    let list = p.e(e).x_list();
                    let n_arg = if list.nn() { p.l(list).n_expr() } else { 0 };
                    let name = p.e(e).tok().to_owned();
                    let func = sqlite3_find_function(p, &name, n_arg, 0);
                    let left = p.e(e).p_left;
                    let mut item = AggInfoFunc { p_f_expr: e, p_func: func.clone(), ..AggInfoFunc::default() };
                    let needcoll = func.as_ref().is_some_and(|f| (f.func_flags & SQLITE_FUNC_NEEDCOLL) != 0);
                    if left.nn() && !needcoll {
                        item.i_ob_tab = p.n_tab;
                        p.n_tab += 1;
                        let ob_list = p.e(left).x_list();
                        if p.l(ob_list).n_expr() == 1
                            && n_arg == 1
                            && sqlite3_expr_compare(p, false, p.l(ob_list).a[0].p_expr, p.l(list).a[0].p_expr, -1) == 0
                        {
                            item.b_ob_payload = false;
                            item.b_ob_unique = p.e(e).has(EP_Distinct);
                        } else {
                            item.b_ob_payload = true;
                        }
                        item.b_use_subtype = func.as_ref().is_some_and(|f| (f.func_flags & SQLITE_SUBTYPE as u32) != 0);
                    } else {
                        item.i_ob_tab = -1;
                    }
                    if p.e(e).has(EP_Distinct) && !item.b_ob_unique {
                        item.i_distinct = p.n_tab;
                        p.n_tab += 1;
                    } else {
                        item.i_distinct = -1;
                    }
                    p.aggm(agg).a_func[i] = item;
                }
                // Make pExpr point to the appropriate pAggInfo->aFunc[] entry
                let x = p.em(e);
                x.i_agg = i as i16;
                x.p_agg_info = agg;
                WRC_Prune
            } else {
                WRC_Continue
            }
        }
        _ => {
            if (p.nc(nc).nc_flags & NC_InAggFunc) == 0 {
                return WRC_Continue;
            }
            if p.p_idx_epr.is_empty() {
                return WRC_Continue;
            }
            // (C walks the list from its head: the entry added last comes first)
            let mut found: Option<IndexedExpr> = None;
            for k in (0..p.p_idx_epr.len()).rev() {
                let i_data_cur = p.p_idx_epr[k].i_data_cur;
                if i_data_cur < 0 {
                    continue;
                }
                if sqlite3_expr_compare(p, false, e, p.p_idx_epr[k].p_expr, i_data_cur) == 0 {
                    found = Some(p.p_idx_epr[k].clone());
                    break;
                }
            }
            let Some(ie) = found else {
                return WRC_Continue;
            };
            if !p.e(e).use_y_tab() {
                return WRC_Continue;
            }
            if !p.src(src_list).a.iter().any(|it| it.i_cursor == ie.i_data_cur) {
                return WRC_Continue;
            }
            if p.e(e).p_agg_info.nn() {
                return WRC_Continue; // Resolved by outer context
            }
            if p.n_err != 0 {
                return WRC_Abort;
            }

            // If we reach this point, it means that expression pExpr can be translated into a
            // reference to an index column as described by pIEpr.
            let tmp = p.new_expr(Expr { op: TK_AGG_COLUMN, i_table: ie.i_idx_cur, i_column: ie.i_idx_col as i16, ..Expr::default() });
            find_or_create_agg_info_column(p, agg, tmp);
            if p.n_err != 0 {
                return WRC_Abort;
            }
            let i_agg = p.e(tmp).i_agg;
            p.aggm(agg).a_col[i_agg as usize].p_c_expr = e;
            let x = p.em(e);
            x.p_agg_info = agg;
            x.i_agg = i_agg;
            WRC_Prune
        }
    }
}

/// `sqlite3ExprAnalyzeAggregates(pNC, pExpr)`: analyze the expression for aggregate functions
/// and for variables that need to be added to AggInfo object that `nc` points to.
pub fn sqlite3_expr_analyze_aggregates(p: &mut Parse, nc: NcId, e: ExprId) {
    let mut w = Walker {
        x_expr_callback: Some(analyze_aggregate),
        x_select_callback: Some(sqlite3_walker_depth_increase),
        x_select_callback2: Some(sqlite3_walker_depth_decrease),
        walker_depth: 0,
        p_nc: nc,
        has_parse: false,
        ..Walker::default()
    };
    sqlite3_walk_expr(p, &mut w, e);
}

/// `sqlite3ExprAnalyzeAggList(pNC, pList)`: call sqlite3ExprAnalyzeAggregates() for every
/// expression in the list.
pub fn sqlite3_expr_analyze_agg_list(p: &mut Parse, nc: NcId, list: ExprListId) {
    if list.nn() {
        for i in 0..p.l(list).a.len() {
            let e = p.l(list).a[i].p_expr;
            sqlite3_expr_analyze_aggregates(p, nc, e);
        }
    }
}

/// `sqlite3GetTempReg(pParse)`: allocate a single new register for use to hold some
/// intermediate result.
pub fn sqlite3_get_temp_reg(p: &mut Parse) -> i32 {
    if p.n_temp_reg == 0 {
        p.n_mem += 1;
        return p.n_mem;
    }
    p.n_temp_reg -= 1;
    p.a_temp_reg[p.n_temp_reg as usize]
}

/// `sqlite3ReleaseTempReg(pParse, iReg)`: deallocate a register, making it available for reuse
/// for some other purpose.
pub fn sqlite3_release_temp_reg(p: &mut Parse, i_reg: i32) {
    if i_reg != 0 && (p.n_temp_reg as usize) < p.a_temp_reg.len() {
        p.a_temp_reg[p.n_temp_reg as usize] = i_reg;
        p.n_temp_reg += 1;
    }
}

/// `sqlite3GetTempRange(pParse, nReg)`: allocate a contiguous set of registers.
pub fn sqlite3_get_temp_range(p: &mut Parse, n_reg: i32) -> i32 {
    if n_reg == 1 {
        return sqlite3_get_temp_reg(p);
    }
    let mut i = p.i_range_reg;
    let n = p.n_range_reg;
    if n_reg <= n {
        p.i_range_reg += n_reg;
        p.n_range_reg -= n_reg;
    } else {
        i = p.n_mem + 1;
        p.n_mem += n_reg;
    }
    i
}

/// `sqlite3ReleaseTempRange(pParse, iReg, nReg)`
pub fn sqlite3_release_temp_range(p: &mut Parse, i_reg: i32, n_reg: i32) {
    if n_reg == 1 {
        sqlite3_release_temp_reg(p, i_reg);
        return;
    }
    if n_reg > p.n_range_reg {
        p.n_range_reg = n_reg;
        p.i_range_reg = i_reg;
    }
}

/// `sqlite3ClearTempRegCache(pParse)`: mark all temporary registers as being unavailable for
/// reuse.
pub fn sqlite3_clear_temp_reg_cache(p: &mut Parse) {
    p.n_temp_reg = 0;
    p.n_range_reg = 0;
}

/// `sqlite3TouchRegister(pParse, iReg)`: make sure sufficient registers have been allocated so
/// that `i_reg` is a valid register number.
pub fn sqlite3_touch_register(p: &mut Parse, i_reg: i32) {
    if p.n_mem < i_reg {
        p.n_mem = i_reg;
    }
}

/// `sqlite3ExprListAppendVector(pParse, pList, pColumns, pExpr)`: append a new element to
/// the given ExprList for each column in the vector `(a, b, c) = expr` of an UPDATE SET
/// clause.
pub fn sqlite3_expr_list_append_vector(p: &mut Parse, list: ExprListId, columns: IdListId, e: ExprId) -> ExprListId {
    let mut list = list;
    let i_first = if list.nn() { p.l(list).a.len() } else { 0 };
    if columns.is_null() || e.is_null() {
        return list;
    }
    let n_id = p.a_idlist[columns.ix()].a.len() as i32;
    // If the RHS is a vector, then we can immediately check to see that the size of the RHS
    // and LHS match. But if the RHS is a SELECT, we can't really do that check until
    // sqlite3ResolveExprNames() is called.
    if p.e(e).op != TK_SELECT {
        let n = sqlite3_expr_vector_size(p, e);
        if n_id != n {
            p.error_msg(format!("{n_id} columns assigned {n} values"));
            return list;
        }
    }
    for i in 0..n_id {
        let sub = sqlite3_expr_for_vector_field(p, e, i, n_id);
        if sub.is_null() {
            continue;
        }
        list = sqlite3_expr_list_append(p, list, sub);
        if list.nn() {
            let name = p.a_idlist[columns.ix()].a[i as usize].clone();
            p.lm(list).a.last_mut().expect("item").z_e_name = Some(name);
        }
    }
    if p.e(e).op == TK_SELECT && list.nn() {
        let first = p.l(list).a[i_first].p_expr;
        // Store the SELECT statement in pRight so it will be deleted when
        // sqlite3ExprListDelete() is called
        p.em(first).p_right = e;
        // Remember the size of the LHS in iTable so that we can check that the RHS and LHS
        // sizes match during code generation.
        p.em(first).i_table = n_id;
    }
    list
}
