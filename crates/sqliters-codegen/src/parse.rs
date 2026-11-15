//! The LALR(1) parser of SQLite 3.54.1: the driver of `lempar.c` over the tables lemon
//! generated from `parse.y` ([`crate::parse_tables`]) and the reduce actions of `parse.y`.
//!
//! Because the tables are C's tables, the reductions happen in C's order and syntax errors are
//! found at the same token as in C. The reduce actions build C's parse tree and call the code
//! generator exactly where `parse.y` does.
//!
//! Step V1 ports the actions of the expression, SELECT / VALUES, WITH and transaction rules.
//! The actions of the other rules (FROM clause terms, DML, DDL, triggers,
//! PRAGMA, …) set [`Parse::unsupported`]; the statement is then handled by the engine's old
//! code. Rules without an action in `parse.y` need none here either.

use crate::build::*;
use crate::consts::*;
use crate::expr::*;
use crate::parse_tables::*;
use crate::select::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::util::*;

/// `YYMINORTYPE`: the semantic value of a grammar symbol. C uses a union; the fields below are
/// the members of that union that the ported actions use.
#[derive(Debug, Clone, Copy, Default)]
struct Minor {
    /// `yy0`: a token
    tok: Token,
    /// `yy144` (int), `yy462` (u8), `yy168` (scan pointer, as an offset), `yy509.eType`
    i: i32,
    /// `yy454` (Expr*), `yy14` (ExprList*), `yy555` (Select*), `yy203` (SrcList*),
    /// `yy132` (IdList*), `yy59` (With*), `yy67` (Cte*), `yy211` (Window*), `yy269.pOn`,
    /// `yy509.pExpr`
    id: u32,
    /// `yy269.pUsing`
    id2: u32,
}

#[derive(Debug, Clone, Copy, Default)]
struct YyStackEntry {
    /// The state-number, or reduce action in SHIFTREDUCE
    stateno: u16,
    /// The major token value
    major: u16,
    /// The user-supplied minor token value
    minor: Minor,
}

/// `yyParser`: the state of the parser.
pub struct YyParser {
    /// The parser stack; `yytos` is the last element
    yystack: Vec<YyStackEntry>,
    /// Number of entries the stack may currently hold without growing (`yystackEnd+1-yystack`)
    stack_size: usize,
    /// CTEs built by `wqitem` rules, referred to by index (`yy67`)
    a_cte: Vec<Cte>,
}

/// `sqlite3ParserFallback(iToken)`: the fallback token of `iToken`, 0 if none.
pub fn sqlite3_parser_fallback(i_token: u8) -> u8 {
    YY_FALLBACK[i_token as usize] as u8
}

/// `yy_find_shift_action(iLookAhead, stateno)`
fn yy_find_shift_action(mut i_look_ahead: u16, stateno: u16) -> u16 {
    if stateno as i32 > YY_MAX_SHIFT {
        return stateno;
    }
    loop {
        let i = YY_SHIFT_OFST[stateno as usize] as usize + i_look_ahead as usize;
        if YY_LOOKAHEAD[i] != i_look_ahead {
            let i_fallback = YY_FALLBACK[i_look_ahead as usize];
            if i_fallback != 0 {
                i_look_ahead = i_fallback;
                continue;
            }
            let j = i - i_look_ahead as usize + YYWILDCARD as usize;
            if YY_LOOKAHEAD[j] == YYWILDCARD as u16 && i_look_ahead > 0 {
                return YY_ACTION[j];
            }
            return YY_DEFAULT[stateno as usize];
        } else {
            return YY_ACTION[i];
        }
    }
}

/// `yy_find_reduce_action(stateno, iLookAhead)`
fn yy_find_reduce_action(stateno: u16, i_look_ahead: u16) -> u16 {
    let i = YY_REDUCE_OFST[stateno as usize] as i32 + i_look_ahead as i32;
    YY_ACTION[i as usize]
}

/// `tokenExpr(pParse, op, t)`: construct a new Expr object from a single token.
fn token_expr(p: &mut Parse, op: u8, t: Token) -> ExprId {
    let z = p.tok_str(&t).to_owned();
    let mut e = Expr { op, flags: EP_Leaf, w: t.z as i32, n_height: 1, ..Expr::default() };
    if !z.is_empty() && is_quote(z.as_bytes()[0]) {
        // sqlite3DequoteExpr(p)
        e.flags |= if z.as_bytes()[0] == b'"' { EP_Quoted | EP_DblQuoted } else { EP_Quoted };
        e.z_token = Some(sqlite3_dequote(&z));
    } else {
        e.z_token = Some(z);
    }
    p.new_expr(e)
}

/// `sqlite3PExprIsNull(pParse, op, pLeft)`: create a TK_ISNULL or TK_NOTNULL expression,
/// perhaps optimized to TK_TRUEFALSE, if possible.
fn sqlite3_p_expr_is_null(p: &mut Parse, op: u8, p_left: ExprId) -> ExprId {
    let mut x = p_left;
    while p.e(x).op == TK_UPLUS || p.e(x).op == TK_UMINUS {
        x = p.e(x).p_left;
    }
    match p.e(x).op {
        TK_INTEGER | TK_STRING | TK_FLOAT | TK_BLOB => {
            // sqlite3ExprDeferredDelete(pParse, pLeft);
            return sqlite3_expr_int32(p, (op == TK_NOTNULL) as i32);
        }
        _ => {}
    }
    sqlite3_p_expr(p, op, p_left, ExprId::NULL)
}

/// `sqlite3PExprIs(pParse, op, pLeft, pRight)`: create a TK_IS or TK_ISNOT operator, perhaps
/// optimized to a NOTNULL or ISNULL.
fn sqlite3_p_expr_is(p: &mut Parse, op: u8, p_left: ExprId, p_right: ExprId) -> ExprId {
    if p_right.nn() && p.e(p_right).op == TK_NULL {
        return sqlite3_p_expr_is_null(p, if op == TK_IS { TK_ISNULL } else { TK_NOTNULL }, p_left);
    }
    sqlite3_p_expr(p, op, p_left, p_right)
}

/// `parserAddExprIdListTerm(pParse, pPrior, pIdToken, hasCollate, sortOrder)`: add a single new
/// term to an ExprList that is used to store a list of identifiers. Report an error if the ID
/// list contains a COLLATE clause or an ASC or DESC keyword, except ignore the error if
/// parsing a legacy schema.
fn parser_add_expr_id_list_term(p: &mut Parse, prior: ExprListId, id_token: &Token, has_collate: i32, sort_order: i32) -> ExprListId {
    let l = sqlite3_expr_list_append(p, prior, ExprId::NULL);
    if (has_collate != 0 || sort_order != SQLITE_SO_UNDEFINED) && !p.db.init_busy {
        let msg = format!("syntax error after column name \"{}\"", p.tok_str(id_token));
        p.error_msg(msg);
    }
    sqlite3_expr_list_set_name(p, l, id_token, 1);
    l
}

/// `parserDoubleLinkSelect(pParse, p)`: for a compound SELECT statement, make sure
/// p->pPrior->pNext==p for all elements in the list, and make sure the list length does not
/// exceed SQLITE_LIMIT_COMPOUND_SELECT.
fn parser_double_link_select(p: &mut Parse, sel: SelectId) {
    if p.s(sel).p_prior.nn() {
        let mut p_next = SelectId::NULL;
        let mut p_loop = sel;
        let mut cnt = 1;
        loop {
            p.sm(p_loop).p_next = p_next;
            p.sm(p_loop).sel_flags |= SF_Compound;
            p_next = p_loop;
            p_loop = p.s(p_loop).p_prior;
            if p_loop.is_null() {
                break;
            }
            cnt += 1;
            if p.s(p_loop).p_order_by.nn() || p.s(p_loop).p_limit.nn() {
                let msg = format!(
                    "{} clause should come after {} not before",
                    if p.s(p_loop).p_order_by.nn() { "ORDER BY" } else { "LIMIT" },
                    sqlite3_select_op_name(p.s(p_next).op)
                );
                p.error_msg(msg);
                break;
            }
        }
        let mx_select = p.db.a_limit[SQLITE_LIMIT_COMPOUND_SELECT as usize];
        if (p.s(sel).sel_flags & (SF_MultiValue | SF_Values)) == 0 && mx_select > 0 && cnt > mx_select {
            p.error_msg("too many terms in compound SELECT");
        }
    }
}

/// `attachWithToSelect(pParse, pSelect, pWith)`
fn attach_with_to_select(p: &mut Parse, sel: SelectId, with: WithId) -> SelectId {
    if sel.nn() {
        p.sm(sel).p_with = with;
        parser_double_link_select(p, sel);
    }
    sel
}

impl Default for YyParser {
    fn default() -> Self {
        Self::new()
    }
}

impl YyParser {
    /// `sqlite3ParserInit()`
    pub fn new() -> YyParser {
        let mut yystack = Vec::with_capacity(YYSTACKDEPTH as usize);
        yystack.push(YyStackEntry::default());
        YyParser { yystack, stack_size: YYSTACKDEPTH as usize, a_cte: Vec::new() }
    }

    /// `yyGrowStack(p)`: try to increase the size of the parser stack. Returns true on failure.
    fn yy_grow_stack(&mut self, p: &Parse) -> bool {
        let old_size = self.stack_size as i32;
        let n_limit = p.db.a_limit[SQLITE_LIMIT_PARSER_DEPTH as usize];
        let mut new_size = old_size * 2 + 100;
        if new_size > n_limit {
            new_size = n_limit;
            if new_size <= old_size {
                return true;
            }
        }
        self.stack_size = new_size as usize;
        false
    }

    /// `yyStackOverflow(yypParser)`
    fn yy_stack_overflow(&mut self, p: &mut Parse) {
        self.yystack.truncate(1);
        if p.n_err == 0 {
            p.error_msg("Recursion limit");
        }
    }

    /// `yy_shift(yypParser, yyNewState, yyMajor, yyMinor)`
    fn yy_shift(&mut self, p: &mut Parse, mut yy_new_state: u16, yy_major: u16, yy_minor: Token) {
        // yypParser->yytos++; if( yytos>yypParser->yystackEnd ) grow
        if self.yystack.len() + 1 > self.stack_size && self.yy_grow_stack(p) {
            self.yy_stack_overflow(p);
            return;
        }
        if yy_new_state as i32 > YY_MAX_SHIFT {
            yy_new_state += (YY_MIN_REDUCE - YY_MIN_SHIFTREDUCE) as u16;
        }
        self.yystack.push(YyStackEntry {
            stateno: yy_new_state,
            major: yy_major,
            minor: Minor { tok: yy_minor, ..Minor::default() },
        });
    }

    /// `sqlite3Parser(yyp, yymajor, yyminor)`: the main parser program.
    pub fn sqlite3_parser(&mut self, p: &mut Parse, yymajor: u8, yyminor: Token) {
        let mut yyact = self.yystack.last().expect("parser stack").stateno;
        loop {
            yyact = yy_find_shift_action(yymajor as u16, yyact);
            if yyact as i32 >= YY_MIN_REDUCE {
                let yyruleno = (yyact as i32 - YY_MIN_REDUCE) as usize;
                // Check that the stack is large enough to grow by a single entry if the RHS of
                // the rule is empty.
                if YY_RULE_INFO_NRHS[yyruleno] == 0
                    && self.yystack.len() >= self.stack_size
                    && self.yy_grow_stack(p)
                {
                    self.yy_stack_overflow(p);
                    break;
                }
                yyact = self.yy_reduce(p, yyruleno, yymajor, yyminor);
            } else if yyact as i32 <= YY_MAX_SHIFTREDUCE {
                self.yy_shift(p, yyact, yymajor as u16, yyminor);
                break;
            } else if yyact as i32 == YY_ACCEPT_ACTION {
                self.yystack.pop();
                // yy_accept(): nothing to do
                return;
            } else {
                debug_assert!(yyact as i32 == YY_ERROR_ACTION);
                // YYNOERRORRECOVERY: report the error and discard the token.
                // %syntax_error
                if yyminor.z != Token::NO_Z && (yyminor.z as usize) < p.z_sql.len() {
                    let msg = format!("near \"{}\": syntax error", p.tok_str(&yyminor));
                    p.error_msg_tok(msg, &yyminor);
                } else {
                    p.error_msg("incomplete input");
                }
                break;
            }
        }
    }

    /// `yy_reduce(yypParser, yyruleno, yyLookahead, yyLookaheadToken)`: perform a reduce
    /// action and the shift that must immediately follow the reduce. Returns the new state.
    fn yy_reduce(&mut self, p: &mut Parse, yyruleno: usize, _yy_lookahead: u8, yy_lookahead_token: Token) -> u16 {
        let yysize = YY_RULE_INFO_NRHS[yyruleno] as isize; // negative of the number of RHS symbols
        if yysize == 0 {
            // room for yymsp[1]
            self.yystack.push(YyStackEntry::default());
        }
        // index of yymsp[0]
        let top = (self.yystack.len() as isize - 1 - if yysize == 0 { 1 } else { 0 }) as usize;
        // yymsp[k].minor
        macro_rules! m {
            ($k:expr) => {
                self.yystack[(top as isize + ($k)) as usize].minor
            };
        }
        macro_rules! major {
            ($k:expr) => {
                self.yystack[(top as isize + ($k)) as usize].major as u8
            };
        }
        macro_rules! ex {
            ($k:expr) => {
                ExprId(m!($k).id)
            };
        }
        macro_rules! el {
            ($k:expr) => {
                ExprListId(m!($k).id)
            };
        }
        macro_rules! sl {
            ($k:expr) => {
                SelectId(m!($k).id)
            };
        }
        match yyruleno {
            0 => p.explain = 1,  // explain ::= EXPLAIN  (pParse->pReprepare==0 always here)
            1 => p.explain = 2,  // explain ::= EXPLAIN QUERY PLAN
            2 => {
                // cmdx ::= cmd
                // (sqlite3FinishCoding(): while the schema is loaded there is no program:
                // "if( db->init.busy ){ pParse->rc = SQLITE_DONE; return; }")
                if !p.db.init_busy {
                    sqlite3_finish_coding(p)
                }
            }
            3 => {
                // cmd ::= BEGIN transtype trans_opt
                let ty = m!(-1).i;
                sqlite3_begin_transaction(p, ty);
            }
            4 => m!(1).i = TK_DEFERRED as i32, // transtype ::=
            5..=7 => m!(0).i = major!(0) as i32, // transtype ::= DEFERRED | IMMEDIATE | EXCLUSIVE
            8 | 9 => {
                // cmd ::= COMMIT|END trans_opt ; cmd ::= ROLLBACK trans_opt
                let ty = major!(-1) as i32;
                sqlite3_end_transaction(p, ty);
            }
            10 => {
                // cmd ::= SAVEPOINT nm
                let t = m!(0).tok;
                sqlite3_savepoint(p, SAVEPOINT_BEGIN, &t);
            }
            11 => {
                // cmd ::= RELEASE savepoint_opt nm
                let t = m!(0).tok;
                sqlite3_savepoint(p, SAVEPOINT_RELEASE, &t);
            }
            12 => {
                // cmd ::= ROLLBACK trans_opt TO savepoint_opt nm
                let t = m!(0).tok;
                sqlite3_savepoint(p, SAVEPOINT_ROLLBACK, &t);
            }
            26 | 65 => m!(1).tok = Token::EMPTY, // typetoken ::= ; conslist_opt ::=
            27 => {
                // typetoken ::= typename LP signed RP
                let end = m!(0).tok.z + m!(0).tok.n;
                m!(-3).tok.n = end - m!(-3).tok.z;
            }
            28 => {
                // typetoken ::= typename LP signed COMMA signed RP
                let end = m!(0).tok.z + m!(0).tok.n;
                m!(-5).tok.n = end - m!(-5).tok.z;
            }
            29 => {
                // typename ::= typename ID|STRING
                m!(-1).tok.n = m!(0).tok.n + (m!(0).tok.z - m!(-1).tok.z);
            }
            30 => {
                // scanpt ::=
                m!(1).i = yy_lookahead_token.z as i32;
            }
            31 => {
                // scantok ::=
                m!(1).tok = yy_lookahead_token;
            }
            84 => {
                // cmd ::= select
                let mut dest = SelectDest { e_dest: SRT_Output as u8, ..SelectDest::default() };
                // if( (db->mDbFlags & DBFLAG_EncodingFixed)!=0 || sqlite3ReadSchema(pParse)==SQLITE_OK )
                // (sqlite3ReadSchema() cannot fail here; when it has a schema to load, the
                // error offset of an error found while parsing is lost)
                let s = sl!(0);
                if !p.db.init_busy && !p.db.encoding_fixed {
                    crate::schema::sqlite3_read_schema(p);
                }
                if p.db.init_busy {
                    // (a schema row that is a bare SELECT: C would report a corrupt schema)
                    p.error_msg("");
                } else {
                    crate::treedump::v1_dump_select(p, "P", s);
                    sqlite3_select(p, s, &mut dest);
                }
            }
            85 => {
                // select ::= WITH wqlist selectnowith
                let (s, w) = (sl!(0), WithId(m!(-1).id));
                m!(-2).id = attach_with_to_select(p, s, w).0;
            }
            86 => {
                // select ::= WITH RECURSIVE wqlist selectnowith
                let (s, w) = (sl!(0), WithId(m!(-1).id));
                m!(-3).id = attach_with_to_select(p, s, w).0;
            }
            87 => {
                // select ::= selectnowith
                let s = sl!(0);
                if s.nn() {
                    parser_double_link_select(p, s);
                }
            }
            88 => {
                // selectnowith ::= selectnowith multiselect_op oneselect
                let mut p_rhs = sl!(0);
                let p_lhs = sl!(-2);
                if p_rhs.nn() && p.s(p_rhs).p_prior.nn() {
                    let x = Token { z: Token::NO_Z, n: 0 };
                    parser_double_link_select(p, p_rhs);
                    let p_from = sqlite3_src_list_append_from_term(p, SrcListId::NULL, None, None, &x, p_rhs, None);
                    p_rhs = sqlite3_select_new(
                        p,
                        ExprListId::NULL,
                        p_from,
                        ExprId::NULL,
                        ExprListId::NULL,
                        ExprId::NULL,
                        ExprListId::NULL,
                        0,
                        ExprId::NULL,
                    );
                }
                if p_rhs.nn() {
                    let op = m!(-1).i;
                    p.sm(p_rhs).op = op as u8;
                    p.sm(p_rhs).p_prior = p_lhs;
                    if p_lhs.nn() {
                        p.sm(p_lhs).sel_flags &= !SF_MultiValue;
                    }
                    p.sm(p_rhs).sel_flags &= !SF_MultiValue;
                    if op != TK_ALL as i32 {
                        p.has_compound = true;
                    }
                }
                m!(-2).id = p_rhs.0;
            }
            89 | 91 => m!(0).i = major!(0) as i32, // multiselect_op ::= UNION | EXCEPT|INTERSECT
            90 => m!(-1).i = TK_ALL as i32,         // multiselect_op ::= UNION ALL
            92 => {
                // oneselect ::= SELECT distinct selcollist from where_opt groupby_opt having_opt orderby_opt limit_opt
                let s = sqlite3_select_new(
                    p,
                    el!(-6),
                    SrcListId(m!(-5).id),
                    ex!(-4),
                    el!(-3),
                    ex!(-2),
                    el!(-1),
                    m!(-7).i as u32,
                    ex!(0),
                );
                m!(-8).id = s.0;
            }
            93 => {
                // oneselect ::= SELECT distinct selcollist from where_opt groupby_opt having_opt window_clause orderby_opt limit_opt
                let s = sqlite3_select_new(
                    p,
                    el!(-7),
                    SrcListId(m!(-6).id),
                    ex!(-5),
                    el!(-4),
                    ex!(-3),
                    el!(-1),
                    m!(-8).i as u32,
                    ex!(0),
                );
                if s.nn() {
                    p.sm(s).p_win_defn = WindowId(m!(-2).id);
                }
                m!(-9).id = s.0;
            }
            94 => {
                // values ::= VALUES LP nexprlist RP
                let s = sqlite3_select_new(
                    p,
                    el!(-1),
                    SrcListId::NULL,
                    ExprId::NULL,
                    ExprListId::NULL,
                    ExprId::NULL,
                    ExprListId::NULL,
                    SF_Values,
                    ExprId::NULL,
                );
                m!(-3).id = s.0;
            }
            95 => {
                // oneselect ::= mvalues
                let s = sl!(0);
                sqlite3_multi_values_end(p, s);
            }
            96 | 97 => {
                // mvalues ::= values COMMA LP nexprlist RP ; mvalues ::= mvalues COMMA LP nexprlist RP
                let (l, r) = (sl!(-4), el!(-1));
                m!(-4).id = sqlite3_multi_values(p, l, r).0;
            }
            98 => m!(0).i = SF_Distinct as i32, // distinct ::= DISTINCT
            99 => m!(0).i = SF_All as i32,      // distinct ::= ALL
            15 | 18 | 47 | 62 | 72 | 81 | 100 | 246 => m!(1).i = 0, // ifnotexists ::= ; temp ::= ; autoinc ::= ; init_deferred_pred_opt ::= ; defer_subclause_opt ::= ; ifexists ::= ; distinct ::= ; collate ::=
            101 | 134 | 144 | 234 | 237 | 242 => m!(1).id = 0, // sclp ::= ; orderby_opt ::= ; groupby_opt ::= ; exprlist ::= ; paren_exprlist ::= ; eidlist_opt ::=
            102 => {
                // selcollist ::= sclp scanpt expr scanpt as
                let l = sqlite3_expr_list_append(p, el!(-4), ex!(-2));
                m!(-4).id = l.0;
                let as_tok = m!(0).tok;
                if as_tok.n > 0 {
                    sqlite3_expr_list_set_name(p, l, &as_tok, 1);
                }
                let (a, b) = (m!(-3).i, m!(-1).i);
                sqlite3_expr_list_set_span(p, l, a, b);
            }
            103 => {
                // selcollist ::= sclp scanpt STAR
                let e = sqlite3_expr(p, TK_ASTERISK, None);
                let ofs = m!(0).tok.z as i32;
                sqlite3_expr_set_error_offset(p, e, ofs);
                m!(-2).id = sqlite3_expr_list_append(p, el!(-2), e).0;
            }
            104 => {
                // selcollist ::= sclp scanpt nm DOT STAR
                let p_right = sqlite3_p_expr(p, TK_ASTERISK, ExprId::NULL, ExprId::NULL);
                let ofs = m!(0).tok.z as i32;
                sqlite3_expr_set_error_offset(p, p_right, ofs);
                let t = m!(-2).tok;
                let p_left = token_expr(p, TK_ID, t);
                let p_dot = sqlite3_p_expr(p, TK_DOT, p_left, p_right);
                m!(-4).id = sqlite3_expr_list_append(p, el!(-4), p_dot).0;
            }
            105 | 117 | 258 | 259 => m!(-1).tok = m!(0).tok, // as ::= AS nm ; dbnm ::= DOT nm ; plus_num ; minus_num
            106 => m!(1).tok = Token::EMPTY,               // as ::=
            107 | 110 => m!(1).id = 0,                       // from ::= ; stl_prefix ::=
            108 => {
                // from ::= FROM seltablist
                let l = SrcListId(m!(0).id);
                m!(-1).id = l.0;
                sqlite3_src_list_shift_join_type(p, l);
            }
            109 => {
                // stl_prefix ::= seltablist joinop
                let l = SrcListId(m!(-1).id);
                if l.nn() && !p.src(l).a.is_empty() {
                    let jt = m!(0).i as u8;
                    p.srcm(l).a.last_mut().expect("item").jointype = jt;
                }
            }
            111 => {
                // seltablist ::= stl_prefix nm dbnm as on_using
                let (l, t, d, a) = (SrcListId(m!(-4).id), m!(-3).tok, m!(-2).tok, m!(-1).tok);
                let ou = OnOrUsing { p_on: ExprId(m!(0).id), p_using: IdListId(m!(0).id2) };
                m!(-4).id = sqlite3_src_list_append_from_term(p, l, Some(&t), Some(&d), &a, SelectId::NULL, Some(&ou)).0;
            }
            112 => {
                // seltablist ::= stl_prefix nm dbnm as indexed_by on_using
                let (l, t, d, a) = (SrcListId(m!(-5).id), m!(-4).tok, m!(-3).tok, m!(-2).tok);
                let ou = OnOrUsing { p_on: ExprId(m!(0).id), p_using: IdListId(m!(0).id2) };
                let l = sqlite3_src_list_append_from_term(p, l, Some(&t), Some(&d), &a, SelectId::NULL, Some(&ou));
                m!(-5).id = l.0;
                let ib = m!(-1).tok;
                sqlite3_src_list_indexed_by(p, l, &ib);
            }
            113 => {
                // seltablist ::= stl_prefix nm dbnm LP exprlist RP as on_using
                let (l, t, d, a) = (SrcListId(m!(-7).id), m!(-6).tok, m!(-5).tok, m!(-1).tok);
                let ou = OnOrUsing { p_on: ExprId(m!(0).id), p_using: IdListId(m!(0).id2) };
                let l = sqlite3_src_list_append_from_term(p, l, Some(&t), Some(&d), &a, SelectId::NULL, Some(&ou));
                m!(-7).id = l.0;
                let args = el!(-3);
                sqlite3_src_list_func_args(p, l, args);
            }
            114 => {
                // seltablist ::= stl_prefix LP select RP as on_using
                let (l, a, sub) = (SrcListId(m!(-5).id), m!(-1).tok, sl!(-3));
                let ou = OnOrUsing { p_on: ExprId(m!(0).id), p_using: IdListId(m!(0).id2) };
                m!(-5).id = sqlite3_src_list_append_from_term(p, l, None, None, &a, sub, Some(&ou)).0;
            }
            115 => {
                // seltablist ::= stl_prefix LP seltablist RP as on_using
                let (l, a, inner) = (SrcListId(m!(-5).id), m!(-1).tok, SrcListId(m!(-3).id));
                let ou = OnOrUsing { p_on: ExprId(m!(0).id), p_using: IdListId(m!(0).id2) };
                if l.is_null() && a.n == 0 && ou.p_on.is_null() && ou.p_using.is_null() {
                    m!(-5).id = inner.0;
                } else if inner.nn() && p.src(inner).a.len() == 1 {
                    let l = sqlite3_src_list_append_from_term(p, l, None, None, &a, SelectId::NULL, Some(&ou));
                    m!(-5).id = l.0;
                    if l.nn() {
                        let old = std::mem::take(&mut p.srcm(inner).a[0]);
                        let new = p.srcm(l).a.last_mut().expect("item");
                        new.z_name = old.z_name;
                        if old.is_subquery {
                            new.is_subquery = true;
                            new.p_subq = old.p_subq;
                        } else {
                            new.z_database = old.z_database;
                        }
                        if old.is_tab_func {
                            new.p_func_arg = old.p_func_arg;
                            new.is_tab_func = true;
                        } else if old.is_indexed_by {
                            new.z_indexed_by = old.z_indexed_by;
                            new.is_indexed_by = true;
                        }
                        new.not_indexed = old.not_indexed;
                        let nested = new.p_subq.as_ref().map(|q| q.p_select);
                        if let Some(sub) = nested
                            && new.is_subquery
                            && (p.s(sub).sel_flags & SF_NestedFrom) != 0
                        {
                            p.srcm(l).a.last_mut().expect("item").is_nested_from = true;
                        }
                    }
                } else {
                    sqlite3_src_list_shift_join_type(p, inner);
                    let sub = sqlite3_select_new(
                        p,
                        ExprListId::NULL,
                        inner,
                        ExprId::NULL,
                        ExprListId::NULL,
                        ExprId::NULL,
                        ExprListId::NULL,
                        SF_NestedFrom,
                        ExprId::NULL,
                    );
                    m!(-5).id = sqlite3_src_list_append_from_term(p, l, None, None, &a, sub, Some(&ou)).0;
                }
            }
            118 | 120 => {
                // fullname ::= nm ; xfullname ::= nm
                let t = m!(0).tok;
                m!(0).id = sqlite3_src_list_append(p, SrcListId::NULL, Some(&t), None).0;
            }
            119 | 121 => {
                // fullname ::= nm DOT nm ; xfullname ::= nm DOT nm
                let (t, d) = (m!(-2).tok, m!(0).tok);
                m!(-2).id = sqlite3_src_list_append(p, SrcListId::NULL, Some(&t), Some(&d)).0;
            }
            122 => {
                // xfullname ::= nm AS nm
                let (t, a) = (m!(-2).tok, m!(0).tok);
                let l = sqlite3_src_list_append(p, SrcListId::NULL, Some(&t), None);
                if l.nn() {
                    let alias = sqlite3_dequote(p.tok_str(&a));
                    p.srcm(l).a[0].z_alias = Some(alias);
                }
                m!(-2).id = l.0;
            }
            123 => {
                // xfullname ::= nm DOT nm AS nm
                let (t, d, a) = (m!(-4).tok, m!(-2).tok, m!(0).tok);
                let l = sqlite3_src_list_append(p, SrcListId::NULL, Some(&t), Some(&d));
                if l.nn() {
                    let alias = sqlite3_dequote(p.tok_str(&a));
                    p.srcm(l).a[0].z_alias = Some(alias);
                }
                m!(-4).id = l.0;
            }
            125 => {
                // joinop ::= JOIN_KW JOIN
                let a = m!(-1).tok;
                m!(-1).i = sqlite3_join_type(p, &a, None, None);
            }
            126 => {
                // joinop ::= JOIN_KW nm JOIN
                let (a, b) = (m!(-2).tok, m!(-1).tok);
                m!(-2).i = sqlite3_join_type(p, &a, Some(&b), None);
            }
            127 => {
                // joinop ::= JOIN_KW nm nm JOIN
                let (a, b, c) = (m!(-3).tok, m!(-2).tok, m!(-1).tok);
                m!(-3).i = sqlite3_join_type(p, &a, Some(&b), Some(&c));
            }
            129 => {
                // on_using ::= USING LP idlist RP
                let l = m!(-1).id;
                m!(-3).id = 0;
                m!(-3).id2 = l;
            }
            132 => m!(-2).tok = m!(0).tok, // indexed_by ::= INDEXED BY nm
            133 => m!(-1).tok = Token { z: Token::NO_Z, n: 1 }, // indexed_by ::= NOT INDEXED
            116 | 131 => m!(1).tok = Token::EMPTY, // dbnm ::= ; indexed_opt ::=
            124 => m!(0).i = JT_INNER,             // joinop ::= COMMA|JOIN
            128 => {
                // on_using ::= ON expr
                m!(-1).id = m!(0).id;
                m!(-1).id2 = 0;
            }
            130 => {
                // on_using ::=
                m!(1).id = 0;
                m!(1).id2 = 0;
            }
            135 | 145 => m!(-2).id = m!(0).id, // orderby_opt ::= ORDER BY sortlist ; groupby_opt ::= GROUP BY nexprlist
            136 => {
                // sortlist ::= sortlist COMMA expr sortorder nulls
                let l = sqlite3_expr_list_append(p, el!(-4), ex!(-2));
                m!(-4).id = l.0;
                sqlite3_expr_list_set_sort_order(p, l, m!(-1).i, m!(0).i);
            }
            137 => {
                // sortlist ::= expr sortorder nulls
                let l = sqlite3_expr_list_append(p, ExprListId::NULL, ex!(-2));
                m!(-2).id = l.0;
                sqlite3_expr_list_set_sort_order(p, l, m!(-1).i, m!(0).i);
            }
            138 => m!(0).i = SQLITE_SO_ASC,          // sortorder ::= ASC
            139 => m!(0).i = SQLITE_SO_DESC,         // sortorder ::= DESC
            140 | 143 => m!(1).i = SQLITE_SO_UNDEFINED, // sortorder ::= ; nulls ::=
            141 => m!(-1).i = SQLITE_SO_ASC,         // nulls ::= NULLS FIRST
            142 => m!(-1).i = SQLITE_SO_DESC,        // nulls ::= NULLS LAST
            146 | 148 | 153 | 155 | 232 | 233 | 252 => m!(1).id = 0, // having_opt ::= ; limit_opt ::= ; where_opt ::= ; where_opt_ret ::= ; case_else ::= ; case_operand ::= ; vinto ::=
            147 | 154 | 156 | 231 | 251 => m!(-1).id = m!(0).id, // having_opt ::= HAVING expr ; where_opt ::= WHERE expr ; ... ; case_else ::= ELSE expr ; vinto ::= INTO expr
            149 => {
                // limit_opt ::= LIMIT expr
                m!(-1).id = sqlite3_p_expr(p, TK_LIMIT, ex!(0), ExprId::NULL).0;
            }
            150 => {
                // limit_opt ::= LIMIT expr OFFSET expr
                m!(-3).id = sqlite3_p_expr(p, TK_LIMIT, ex!(-2), ex!(0)).0;
            }
            151 => {
                // limit_opt ::= LIMIT expr COMMA expr
                m!(-3).id = sqlite3_p_expr(p, TK_LIMIT, ex!(0), ex!(-2)).0;
            }
            175 => m!(1).id = 0,              // idlist_opt ::=
            176 => m!(-2).id = m!(-1).id,     // idlist_opt ::= LP idlist RP
            177 => {
                // idlist ::= idlist COMMA nm
                let (l, t) = (IdListId(m!(-2).id), m!(0).tok);
                m!(-2).id = sqlite3_id_list_append(p, l, &t).0;
            }
            178 => {
                // idlist ::= nm
                let t = m!(0).tok;
                m!(0).id = sqlite3_id_list_append(p, IdListId::NULL, &t).0;
            }
            179 => m!(-2).id = m!(-1).id, // expr ::= LP expr RP
            180 => {
                // expr ::= ID|INDEXED|JOIN_KW
                let t = m!(0).tok;
                m!(0).id = token_expr(p, TK_ID, t).0;
            }
            181 => {
                // expr ::= nm DOT nm
                let (t1, t2) = (m!(-2).tok, m!(0).tok);
                let temp1 = token_expr(p, TK_ID, t1);
                let temp2 = token_expr(p, TK_ID, t2);
                m!(-2).id = sqlite3_p_expr(p, TK_DOT, temp1, temp2).0;
            }
            182 => {
                // expr ::= nm DOT nm DOT nm
                let (t1, t2, t3) = (m!(-4).tok, m!(-2).tok, m!(0).tok);
                let temp1 = token_expr(p, TK_ID, t1);
                let temp2 = token_expr(p, TK_ID, t2);
                let temp3 = token_expr(p, TK_ID, t3);
                let temp4 = sqlite3_p_expr(p, TK_DOT, temp2, temp3);
                m!(-4).id = sqlite3_p_expr(p, TK_DOT, temp1, temp4).0;
            }
            183 | 184 => {
                // term ::= NULL|FLOAT|BLOB ; term ::= STRING
                let (op, t) = (major!(0), m!(0).tok);
                m!(0).id = token_expr(p, op, t).0;
            }
            185 => {
                // term ::= INTEGER
                let t = m!(0).tok;
                let e = match sqlite3_get_int32(p.tok_str(&t)) {
                    None => sqlite3_expr_alloc(p, TK_INTEGER, Some(&t), 0),
                    Some(i_value) => sqlite3_expr_int32(p, i_value),
                };
                p.em(e).w = t.z as i32;
                m!(0).id = e.0;
            }
            186 => {
                // expr ::= VARIABLE
                let t = m!(0).tok;
                let z = p.tok_str(&t).as_bytes();
                if !(z[0] == b'#' && z.len() > 1 && is_digit(z[1])) {
                    let n = t.n;
                    let e = token_expr(p, TK_VARIABLE, t);
                    m!(0).id = e.0;
                    sqlite3_expr_assign_var_number(p, e, n);
                } else {
                    // When doing a nested parse, one can include terms in an expression that
                    // look like this:   #1 #2 ...  These terms refer to registers in the
                    // virtual machine.  #N is the N-th register.
                    if p.nested == 0 {
                        let msg = format!("near \"{}\": syntax error", p.tok_str(&t));
                        p.error_msg_tok(msg, &t);
                        m!(0).id = 0;
                    } else {
                        let e = sqlite3_p_expr(p, TK_REGISTER, ExprId::NULL, ExprId::NULL);
                        let reg = sqlite3_get_int32(&p.tok_str(&t)[1..]).unwrap_or(0);
                        p.em(e).i_table = reg;
                        m!(0).id = e.0;
                    }
                }
            }
            187 => {
                // expr ::= expr COLLATE ID|STRING
                let t = m!(0).tok;
                m!(-2).id = sqlite3_expr_add_collate_token(p, ex!(-2), &t, 1).0;
            }
            188 => {
                // expr ::= CAST LP expr AS typetoken RP
                let t = m!(-1).tok;
                let e = sqlite3_expr_alloc(p, TK_CAST, Some(&t), 1);
                sqlite3_expr_attach_subtrees(p, e, ex!(-3), ExprId::NULL);
                m!(-5).id = e.0;
            }
            189 => {
                // expr ::= ID|INDEXED|JOIN_KW LP distinct exprlist RP
                let t = m!(-4).tok;
                m!(-4).id = sqlite3_expr_function(p, el!(-1), &t, m!(-2).i).0;
            }
            190 => {
                // expr ::= ID|INDEXED|JOIN_KW LP distinct exprlist ORDER BY sortlist RP
                let t = m!(-7).tok;
                let e = sqlite3_expr_function(p, el!(-4), &t, m!(-5).i);
                sqlite3_expr_add_function_order_by(p, e, el!(-1));
                m!(-7).id = e.0;
            }
            191 => {
                // expr ::= ID|INDEXED|JOIN_KW LP STAR RP
                let t = m!(-3).tok;
                m!(-3).id = sqlite3_expr_function(p, ExprListId::NULL, &t, 0).0;
            }
            192 => {
                // expr ::= ID|INDEXED|JOIN_KW LP distinct exprlist RP filter_over
                let t = m!(-5).tok;
                let e = sqlite3_expr_function(p, el!(-2), &t, m!(-3).i);
                crate::window::sqlite3_window_attach(p, e, WindowId(m!(0).id));
                m!(-5).id = e.0;
            }
            193 => {
                // expr ::= ID|INDEXED|JOIN_KW LP distinct exprlist ORDER BY sortlist RP filter_over
                let t = m!(-8).tok;
                let e = sqlite3_expr_function(p, el!(-5), &t, m!(-6).i);
                crate::window::sqlite3_window_attach(p, e, WindowId(m!(0).id));
                sqlite3_expr_add_function_order_by(p, e, el!(-2));
                m!(-8).id = e.0;
            }
            194 => {
                // expr ::= ID|INDEXED|JOIN_KW LP STAR RP filter_over
                let t = m!(-4).tok;
                let e = sqlite3_expr_function(p, ExprListId::NULL, &t, 0);
                crate::window::sqlite3_window_attach(p, e, WindowId(m!(0).id));
                m!(-4).id = e.0;
            }
            195 => {
                // term ::= CTIME_KW
                let t = m!(0).tok;
                m!(0).id = sqlite3_expr_function(p, ExprListId::NULL, &t, 0).0;
            }
            196 => {
                // expr ::= LP nexprlist COMMA expr RP
                let p_list = sqlite3_expr_list_append(p, el!(-3), ex!(-1));
                let e = sqlite3_p_expr(p, TK_VECTOR, ExprId::NULL, ExprId::NULL);
                p.em(e).x = p_list.0;
                for i in 0..p.l(p_list).a.len() {
                    let f = p.e(p.l(p_list).a[i].p_expr).flags & EP_Propagate;
                    p.em(e).flags |= f;
                }
                m!(-4).id = e.0;
            }
            197 => {
                // expr ::= expr AND expr
                m!(-2).id = sqlite3_expr_and(p, ex!(-2), ex!(0)).0;
            }
            198..=204 => {
                // expr ::= expr OR|LT|GT|GE|LE|EQ|NE|BITAND|BITOR|LSHIFT|RSHIFT|PLUS|MINUS|STAR|SLASH|REM|CONCAT expr
                let op = major!(-1);
                m!(-2).id = sqlite3_p_expr(p, op, ex!(-2), ex!(0)).0;
            }
            205 => {
                // likeop ::= NOT LIKE_KW|MATCH
                m!(-1).tok = m!(0).tok;
                m!(-1).tok.n |= 0x8000_0000;
            }
            206 => {
                // expr ::= expr likeop expr
                let b_not = m!(-1).tok.n & 0x8000_0000 != 0;
                m!(-1).tok.n &= 0x7fff_ffff;
                let mut p_list = sqlite3_expr_list_append(p, ExprListId::NULL, ex!(0));
                p_list = sqlite3_expr_list_append(p, p_list, ex!(-2));
                let t = m!(-1).tok;
                let mut e = sqlite3_expr_function(p, p_list, &t, 0);
                if b_not {
                    e = sqlite3_p_expr(p, TK_NOT, e, ExprId::NULL);
                }
                if e.nn() {
                    p.em(e).flags |= EP_InfixFunc;
                }
                m!(-2).id = e.0;
            }
            207 => {
                // expr ::= expr likeop expr ESCAPE expr
                let b_not = m!(-3).tok.n & 0x8000_0000 != 0;
                m!(-3).tok.n &= 0x7fff_ffff;
                let mut p_list = sqlite3_expr_list_append(p, ExprListId::NULL, ex!(-2));
                p_list = sqlite3_expr_list_append(p, p_list, ex!(-4));
                p_list = sqlite3_expr_list_append(p, p_list, ex!(0));
                let t = m!(-3).tok;
                let mut e = sqlite3_expr_function(p, p_list, &t, 0);
                if b_not {
                    e = sqlite3_p_expr(p, TK_NOT, e, ExprId::NULL);
                }
                if e.nn() {
                    p.em(e).flags |= EP_InfixFunc;
                }
                m!(-4).id = e.0;
            }
            208 => {
                // expr ::= expr ISNULL|NOTNULL
                let op = major!(0);
                m!(-1).id = sqlite3_p_expr_is_null(p, op, ex!(-1)).0;
            }
            209 => {
                // expr ::= expr NOT NULL
                m!(-2).id = sqlite3_p_expr_is_null(p, TK_NOTNULL, ex!(-2)).0;
            }
            210 => m!(-2).id = sqlite3_p_expr_is(p, TK_IS, ex!(-2), ex!(0)).0, // expr ::= expr IS expr
            211 => m!(-3).id = sqlite3_p_expr_is(p, TK_ISNOT, ex!(-3), ex!(0)).0, // expr ::= expr IS NOT expr
            212 => m!(-5).id = sqlite3_p_expr_is(p, TK_IS, ex!(-5), ex!(0)).0, // expr ::= expr IS NOT DISTINCT FROM expr
            213 => m!(-4).id = sqlite3_p_expr_is(p, TK_ISNOT, ex!(-4), ex!(0)).0, // expr ::= expr IS DISTINCT FROM expr
            214 | 215 => {
                // expr ::= NOT expr ; expr ::= BITNOT expr
                let op = major!(-1);
                m!(-1).id = sqlite3_p_expr(p, op, ex!(0), ExprId::NULL).0;
            }
            216 => {
                // expr ::= PLUS|MINUS expr
                let e = ex!(0);
                let op = major!(-1) + (TK_UPLUS - TK_PLUS);
                if e.nn() && p.e(e).op == TK_UPLUS {
                    p.em(e).op = op;
                    m!(-1).id = e.0;
                } else {
                    m!(-1).id = sqlite3_p_expr(p, op, e, ExprId::NULL).0;
                }
            }
            217 => {
                // expr ::= expr PTR expr
                let mut p_list = sqlite3_expr_list_append(p, ExprListId::NULL, ex!(-2));
                p_list = sqlite3_expr_list_append(p, p_list, ex!(0));
                let t = m!(-1).tok;
                m!(-2).id = sqlite3_expr_function(p, p_list, &t, 0).0;
            }
            218 | 221 => m!(0).i = 0,        // between_op ::= BETWEEN ; in_op ::= IN
            63 | 80 | 219 | 222 | 247 => m!(-1).i = 1, // init_deferred_pred_opt ::= INITIALLY DEFERRED ; ifexists ::= IF EXISTS ; between_op ::= NOT BETWEEN ; in_op ::= NOT IN ; collate ::= COLLATE ID|STRING
            220 => {
                // expr ::= expr between_op expr AND expr
                let mut p_list = sqlite3_expr_list_append(p, ExprListId::NULL, ex!(-2));
                p_list = sqlite3_expr_list_append(p, p_list, ex!(0));
                let mut e = sqlite3_p_expr(p, TK_BETWEEN, ex!(-4), ExprId::NULL);
                p.em(e).x = p_list.0;
                sqlite3_expr_set_height_and_flags(p, e);
                if m!(-3).i != 0 {
                    e = sqlite3_p_expr(p, TK_NOT, e, ExprId::NULL);
                }
                m!(-4).id = e.0;
            }
            223 => {
                // expr ::= expr in_op LP exprlist RP
                let b_not = m!(-3).i != 0;
                let rhs_list = el!(-1);
                let lhs = ex!(-4);
                if rhs_list.is_null() {
                    // Expressions of the form "expr1 IN ()" and "expr1 NOT IN ()" simplify to
                    // constants 0 (false) and 1 (true), respectively. Except, do not apply
                    // this optimization if expr1 contains a function because that function
                    // might be an aggregate.
                    let p_b = sqlite3_expr(p, TK_STRING, Some(if b_not { "true" } else { "false" }));
                    sqlite3_expr_id_to_true_false(p, p_b);
                    if !p.e(lhs).has(EP_HasFunc) {
                        m!(-4).id = p_b.0;
                    } else {
                        m!(-4).id = sqlite3_p_expr(p, if b_not { TK_OR } else { TK_AND }, p_b, lhs).0;
                    }
                } else {
                    let p_rhs = p.l(rhs_list).a[0].p_expr;
                    let n_rhs = p.l(rhs_list).a.len();
                    let mut e;
                    if n_rhs == 1
                        && sqlite3_expr_is_constant(p, p_rhs) != 0
                        && p.e(p_rhs).op != TK_COLLATE
                        && p.e(lhs).op != TK_VECTOR
                    {
                        let r = sqlite3_p_expr(p, TK_UPLUS, p_rhs, ExprId::NULL);
                        e = sqlite3_p_expr(p, TK_EQ, lhs, r);
                    } else if n_rhs == 1 && p.e(p_rhs).op == TK_SELECT {
                        e = sqlite3_p_expr(p, TK_IN, lhs, ExprId::NULL);
                        let sel = p.e(p_rhs).x_select();
                        sqlite3_p_expr_add_select(p, e, sel);
                        p.em(p_rhs).x = 0;
                    } else {
                        e = sqlite3_p_expr(p, TK_IN, lhs, ExprId::NULL);
                        if p.e(p.e(e).p_left).op == TK_VECTOR {
                            let n_expr = p.l(p.e(p.e(e).p_left).x_list()).n_expr();
                            let p_select_rhs = sqlite3_expr_list_to_values(p, n_expr, rhs_list);
                            if p_select_rhs.nn() {
                                parser_double_link_select(p, p_select_rhs);
                                sqlite3_p_expr_add_select(p, e, p_select_rhs);
                            }
                        } else {
                            p.em(e).x = rhs_list.0;
                            sqlite3_expr_set_height_and_flags(p, e);
                        }
                    }
                    if b_not {
                        e = sqlite3_p_expr(p, TK_NOT, e, ExprId::NULL);
                    }
                    m!(-4).id = e.0;
                }
            }
            224 => {
                // expr ::= LP select RP
                let e = sqlite3_p_expr(p, TK_SELECT, ExprId::NULL, ExprId::NULL);
                let s = sl!(-1);
                sqlite3_p_expr_add_select(p, e, s);
                m!(-2).id = e.0;
            }
            225 => {
                // expr ::= expr in_op LP select RP
                let mut e = sqlite3_p_expr(p, TK_IN, ex!(-4), ExprId::NULL);
                let s = sl!(-1);
                sqlite3_p_expr_add_select(p, e, s);
                if m!(-3).i != 0 {
                    e = sqlite3_p_expr(p, TK_NOT, e, ExprId::NULL);
                }
                m!(-4).id = e.0;
            }
            226 => {
                // expr ::= expr in_op nm dbnm paren_exprlist
                let (t, d) = (m!(-2).tok, m!(-1).tok);
                let p_src = sqlite3_src_list_append(p, SrcListId::NULL, Some(&t), Some(&d));
                let p_select = sqlite3_select_new(
                    p,
                    ExprListId::NULL,
                    p_src,
                    ExprId::NULL,
                    ExprListId::NULL,
                    ExprId::NULL,
                    ExprListId::NULL,
                    0,
                    ExprId::NULL,
                );
                let args = el!(0);
                if args.nn() {
                    sqlite3_src_list_func_args(p, if p_select.nn() { p_src } else { SrcListId::NULL }, args);
                }
                let mut e = sqlite3_p_expr(p, TK_IN, ex!(-4), ExprId::NULL);
                sqlite3_p_expr_add_select(p, e, p_select);
                if m!(-3).i != 0 {
                    e = sqlite3_p_expr(p, TK_NOT, e, ExprId::NULL);
                }
                m!(-4).id = e.0;
            }
            227 => {
                // expr ::= EXISTS LP select RP
                let e = sqlite3_p_expr(p, TK_EXISTS, ExprId::NULL, ExprId::NULL);
                let s = sl!(-1);
                sqlite3_p_expr_add_select(p, e, s);
                m!(-3).id = e.0;
            }
            228 => {
                // expr ::= CASE case_operand case_exprlist case_else END
                let e = sqlite3_p_expr(p, TK_CASE, ex!(-3), ExprId::NULL);
                let l = if ex!(-1).nn() { sqlite3_expr_list_append(p, el!(-2), ex!(-1)) } else { el!(-2) };
                p.em(e).x = l.0;
                sqlite3_expr_set_height_and_flags(p, e);
                m!(-4).id = e.0;
            }
            229 => {
                // case_exprlist ::= case_exprlist WHEN expr THEN expr
                let l = sqlite3_expr_list_append(p, el!(-4), ex!(-2));
                m!(-4).id = sqlite3_expr_list_append(p, l, ex!(0)).0;
            }
            230 => {
                // case_exprlist ::= WHEN expr THEN expr
                let l = sqlite3_expr_list_append(p, ExprListId::NULL, ex!(-2));
                m!(-3).id = sqlite3_expr_list_append(p, l, ex!(0)).0;
            }
            235 => {
                // nexprlist ::= nexprlist COMMA expr
                m!(-2).id = sqlite3_expr_list_append(p, el!(-2), ex!(0)).0;
            }
            236 => {
                // nexprlist ::= expr
                m!(0).id = sqlite3_expr_list_append(p, ExprListId::NULL, ex!(0)).0;
            }
            238 | 243 => m!(-2).id = m!(-1).id, // paren_exprlist ::= LP exprlist RP ; eidlist_opt ::= LP eidlist RP
            278 => {
                // expr ::= RAISE LP IGNORE RP
                let e = sqlite3_p_expr(p, TK_RAISE, ExprId::NULL, ExprId::NULL);
                p.em(e).aff_expr = OE_Ignore as u8;
                m!(-3).id = e.0;
            }
            279 => {
                // expr ::= RAISE LP raisetype COMMA expr RP
                let e = sqlite3_p_expr(p, TK_RAISE, ex!(-1), ExprId::NULL);
                p.em(e).aff_expr = m!(-3).i as u8;
                m!(-5).id = e.0;
            }
            280 => m!(0).i = OE_Rollback, // raisetype ::= ROLLBACK
            240 | 281 => m!(0).i = OE_Abort, // uniqueflag ::= UNIQUE ; raisetype ::= ABORT
            282 => m!(0).i = OE_Fail,     // raisetype ::= FAIL
            309 | 310 => {
                // with ::= WITH wqlist ; with ::= WITH RECURSIVE wqlist   (DML prefix)
                p.not_supported("WITH prefix of a DML statement");
            }
            311 => m!(0).i = M10d_Any,  // wqas ::= AS
            312 => m!(-1).i = M10d_Yes, // wqas ::= AS MATERIALIZED
            313 => m!(-2).i = M10d_No,  // wqas ::= AS NOT MATERIALIZED
            314 => {
                // wqitem ::= withnm eidlist_opt wqas LP select RP
                let t = m!(-5).tok;
                let z_name = sqlite3_dequote(p.tok_str(&t));
                self.a_cte.push(Cte {
                    z_name,
                    p_cols: el!(-4),
                    p_select: sl!(-1),
                    z_cte_err: None,
                    e_m10d: m!(-3).i as u8,
                    p_use: 0,
                });
                m!(-5).id = self.a_cte.len() as u32 - 1;
            }
            315 => p.b_has_with = true, // withnm ::= nm
            316 => {
                // wqlist ::= wqitem
                let cte = self.a_cte[m!(0).id as usize].clone();
                m!(0).id = sqlite3_with_add(p, WithId::NULL, cte).0;
            }
            317 => {
                // wqlist ::= wqlist COMMA wqitem
                let cte = self.a_cte[m!(0).id as usize].clone();
                m!(-2).id = sqlite3_with_add(p, WithId(m!(-2).id), cte).0;
            }
            244 => {
                // eidlist ::= eidlist COMMA nm collate sortorder
                let t = m!(-2).tok;
                m!(-4).id = parser_add_expr_id_list_term(p, el!(-4), &t, m!(-1).i, m!(0).i).0;
            }
            245 => {
                // eidlist ::= nm collate sortorder
                let t = m!(-2).tok;
                m!(-2).id = parser_add_expr_id_list_term(p, ExprListId::NULL, &t, m!(-1).i, m!(0).i).0;
            }
            343 => {
                // filter_over ::= filter_clause
                let w = p.new_win(Window { e_frm_type: TK_FILTER, p_filter: ex!(0), ..Window::default() });
                m!(0).id = w.0;
            }
            346 => {
                // filter_clause ::= FILTER LP WHERE expr RP
                m!(-4).id = m!(-1).id;
            }
            318 => {
                // windowdefn_list ::= windowdefn_list COMMA windowdefn
                let (y, z) = (WindowId(m!(-2).id), WindowId(m!(0).id));
                crate::window::sqlite3_window_chain(p, z, y);
                p.wm(z).p_next_win = y;
                m!(-2).id = z.0;
            }
            319 => {
                // windowdefn ::= nm AS LP window RP
                let (x, y) = (m!(-4).tok, WindowId(m!(-1).id));
                if y.nn() {
                    let name = p.tok_str(&x).to_owned();
                    p.wm(y).z_name = Some(name);
                }
                m!(-4).id = y.0;
            }
            320 => {
                // window ::= PARTITION BY nexprlist orderby_opt frame_opt
                let w = crate::window::sqlite3_window_assemble(p, WindowId(m!(0).id), el!(-2), el!(-1), None);
                m!(-4).id = w.0;
            }
            321 => {
                // window ::= nm PARTITION BY nexprlist orderby_opt frame_opt
                let t = m!(-5).tok;
                let w = crate::window::sqlite3_window_assemble(p, WindowId(m!(0).id), el!(-2), el!(-1), Some(&t));
                m!(-5).id = w.0;
            }
            322 => {
                // window ::= ORDER BY sortlist frame_opt
                let w = crate::window::sqlite3_window_assemble(p, WindowId(m!(0).id), ExprListId::NULL, el!(-1), None);
                m!(-3).id = w.0;
            }
            323 => {
                // window ::= nm ORDER BY sortlist frame_opt
                let t = m!(-4).tok;
                let w = crate::window::sqlite3_window_assemble(p, WindowId(m!(0).id), ExprListId::NULL, el!(-1), Some(&t));
                m!(-4).id = w.0;
            }
            324 => {
                // window ::= nm frame_opt
                let t = m!(-1).tok;
                let w = crate::window::sqlite3_window_assemble(p, WindowId(m!(0).id), ExprListId::NULL, ExprListId::NULL, Some(&t));
                m!(-1).id = w.0;
            }
            325 => {
                // frame_opt ::=
                let w = crate::window::sqlite3_window_alloc(p, 0, TK_UNBOUNDED, ExprId::NULL, TK_CURRENT, ExprId::NULL, 0);
                m!(1).id = w.0;
            }
            326 => {
                // frame_opt ::= range_or_rows frame_bound_s frame_exclude_opt
                let (x, ye, yp, z) = (m!(-2).i as u8, m!(-1).i as u8, ex!(-1), m!(0).i as u8);
                let w = crate::window::sqlite3_window_alloc(p, x, ye, yp, TK_CURRENT, ExprId::NULL, z);
                m!(-2).id = w.0;
            }
            327 => {
                // frame_opt ::= range_or_rows BETWEEN frame_bound_s AND frame_bound_e frame_exclude_opt
                let (x, ye, yp, ze, zp, wx) = (m!(-5).i as u8, m!(-3).i as u8, ex!(-3), m!(-1).i as u8, ex!(-1), m!(0).i as u8);
                let w = crate::window::sqlite3_window_alloc(p, x, ye, yp, ze, zp, wx);
                m!(-5).id = w.0;
            }
            328 => m!(0).i = major!(0) as i32, // range_or_rows ::= RANGE|ROWS|GROUPS
            329 | 331 | 342 => {}              // frame_bound_s ::= frame_bound ; frame_bound_e ::= frame_bound ; filter_over ::= over_clause
            330 | 332 | 334 => {
                // frame_bound_s ::= UNBOUNDED PRECEDING ; frame_bound_e ::= UNBOUNDED FOLLOWING ;
                // frame_bound ::= CURRENT ROW
                let x = major!(-1) as i32;
                m!(-1).i = x;
                m!(-1).id = 0;
            }
            333 => {
                // frame_bound ::= expr PRECEDING|FOLLOWING
                m!(-1).i = major!(0) as i32;
            }
            335 => m!(1).i = 0, // frame_exclude_opt ::=
            336 => {
                // frame_exclude_opt ::= EXCLUDE frame_exclude
                m!(-1).i = m!(0).i;
            }
            337 | 338 => m!(-1).i = major!(-1) as i32, // frame_exclude ::= NO OTHERS ; frame_exclude ::= CURRENT ROW
            339 => m!(0).i = major!(0) as i32,        // frame_exclude ::= GROUP|TIES
            340 => {
                // window_clause ::= WINDOW windowdefn_list
                m!(-1).id = m!(0).id;
            }
            341 => {
                // filter_over ::= filter_clause over_clause
                let (f, o) = (ex!(-1), WindowId(m!(0).id));
                if o.nn() {
                    p.wm(o).p_filter = f;
                }
                m!(-1).id = o.0;
            }
            344 => {
                // over_clause ::= OVER LP window RP
                m!(-3).id = m!(-1).id;
            }
            345 => {
                // over_clause ::= OVER nm
                let z = m!(0).tok;
                let name = p.tok_str(&z).to_owned();
                let w = p.new_win(Window { z_name: Some(name), ..Window::default() });
                m!(-1).id = w.0;
            }
            347 => {
                // term ::= QNUMBER
                let (op, t) = (major!(0), m!(0).tok);
                let e = token_expr(p, op, t);
                sqlite3_dequote_number(p, e);
                m!(0).id = e.0;
            }
            13 => {
                // create_table ::= createkw temp TABLE ifnotexists nm dbnm
                let (n1, n2, temp, noerr) = (m!(-1).tok, m!(0).tok, m!(-4).i, m!(-2).i);
                crate::build_tab::sqlite3_start_table(p, &n1, &n2, temp != 0, false, false, noerr != 0);
            }
            14 => {} // createkw ::= CREATE   (disableLookaside)
            16 => m!(-2).i = 1, // ifnotexists ::= IF NOT EXISTS
            17 => m!(0).i = (!p.db.init_busy) as i32, // temp ::= TEMP
            19 => {
                // create_table_args ::= LP columnlist conslist_opt RP table_option_set
                let opts = m!(0).i as u32;
                crate::build_tab::sqlite3_end_table(p, true, opts, SelectId::NULL);
            }
            20 => {
                // create_table_args ::= AS select
                let s = sl!(0);
                if p.db.init_busy {
                    crate::build_tab::sqlite3_end_table(p, false, 0, s);
                } else {
                    p.not_supported("statement kind (grammar rule not ported)");
                }
            }
            21 => m!(1).i = 0, // table_option_set ::=
            22 => {
                // table_option_set ::= table_option_set COMMA table_option
                let v = m!(-2).i | m!(0).i;
                m!(-2).i = v;
            }
            23 => {
                // table_option ::= WITHOUT nm
                let t = m!(0).tok;
                if p.tok_str(&t).eq_ignore_ascii_case("rowid") {
                    m!(-1).i = (TF_WithoutRowid | TF_NoVisibleRowid) as i32;
                } else {
                    m!(-1).i = 0;
                    let msg = format!("unknown table option: {}", p.tok_str(&t));
                    p.error_msg(msg);
                }
            }
            24 => {
                // table_option ::= nm
                let t = m!(0).tok;
                if p.tok_str(&t).eq_ignore_ascii_case("strict") {
                    m!(0).i = TF_Strict as i32;
                } else {
                    m!(0).i = 0;
                    let msg = format!("unknown table option: {}", p.tok_str(&t));
                    p.error_msg(msg);
                }
            }
            25 => {
                // columnname ::= nm typetoken
                let (n, t) = (m!(-1).tok, m!(0).tok);
                crate::build_tab::sqlite3_add_column(p, n, t);
            }
            32 | 67 => p.cr_constraint_name = m!(0).tok, // ccons ::= CONSTRAINT nm ; tcons ::= CONSTRAINT nm
            33 => {
                // ccons ::= DEFAULT scantok term
                let (e, t) = (ex!(0), m!(-1).tok);
                crate::build_tab::sqlite3_add_default_value(p, e, t.z as usize, (t.z + t.n) as usize);
            }
            34 => {
                // ccons ::= DEFAULT LP expr RP
                let (e, lp, rp) = (ex!(-1), m!(-2).tok, m!(0).tok);
                crate::build_tab::sqlite3_add_default_value(p, e, lp.z as usize + 1, rp.z as usize);
            }
            35 => {
                // ccons ::= DEFAULT PLUS scantok term
                let (e, plus, t) = (ex!(0), m!(-2).tok, m!(-1).tok);
                crate::build_tab::sqlite3_add_default_value(p, e, plus.z as usize, (t.z + t.n) as usize);
            }
            36 => {
                // ccons ::= DEFAULT MINUS scantok term
                let (e, minus, t) = (ex!(0), m!(-2).tok, m!(-1).tok);
                let e = sqlite3_p_expr(p, TK_UMINUS, e, ExprId::NULL);
                crate::build_tab::sqlite3_add_default_value(p, e, minus.z as usize, (t.z + t.n) as usize);
            }
            37 => {
                // ccons ::= DEFAULT scantok ID|INDEXED
                let t = m!(0).tok;
                let e = token_expr(p, TK_STRING, t);
                if e.nn() {
                    sqlite3_expr_id_to_true_false(p, e);
                }
                crate::build_tab::sqlite3_add_default_value(p, e, t.z as usize, (t.z + t.n) as usize);
            }
            38 => {
                // ccons ::= NOT NULL onconf
                let oe = m!(0).i;
                crate::build_tab::sqlite3_add_not_null(p, oe);
            }
            39 => {
                // ccons ::= PRIMARY KEY sortorder onconf autoinc
                let (oe, ai, so) = (m!(-1).i, m!(0).i, m!(-2).i);
                crate::build_tab::sqlite3_add_primary_key(p, ExprListId::NULL, oe, ai, so);
            }
            40 => {
                // ccons ::= UNIQUE onconf
                let oe = m!(0).i;
                crate::build_tab::sqlite3_create_index(p, None, None, SrcListId::NULL, ExprListId::NULL, oe, None, ExprId::NULL, 0, false, SQLITE_IDXTYPE_UNIQUE as u8);
            }
            41 => {
                // ccons ::= CHECK LP expr RP
                let (e, lp, rp) = (ex!(-1), m!(-2).tok, m!(0).tok);
                crate::build_tab::sqlite3_add_check_constraint(p, e, lp.z as usize, rp.z as usize);
            }
            42 => {
                // ccons ::= REFERENCES nm eidlist_opt refargs
                let (to, cols, flags) = (m!(-2).tok, el!(-1), m!(0).i);
                crate::build_tab::sqlite3_create_foreign_key(p, ExprListId::NULL, &to, cols, flags);
            }
            43 => {
                // ccons ::= defer_subclause
                let d = m!(0).i;
                crate::build_tab::sqlite3_defer_foreign_key(p, d);
            }
            44 => {
                // ccons ::= COLLATE ID|STRING
                let t = m!(0).tok;
                crate::build_tab::sqlite3_add_collate_type(p, &t);
            }
            45 => {
                // generated ::= LP expr RP
                let e = ex!(-1);
                crate::build_tab::sqlite3_add_generated(p, e, None);
            }
            46 => {
                // generated ::= LP expr RP ID
                let (e, t) = (ex!(-2), m!(0).tok);
                crate::build_tab::sqlite3_add_generated(p, e, Some(&t));
            }
            48 => m!(0).i = 1, // autoinc ::= AUTOINCR
            49 => m!(1).i = OE_None * 0x0101, // refargs ::=
            50 => {
                // refargs ::= refargs refarg   (refarg: value in `i`, mask in `id`)
                let (value, mask) = (m!(0).i, m!(0).id as i32);
                let v = (m!(-1).i & !mask) | value;
                m!(-1).i = v;
            }
            51 => {
                // refarg ::= MATCH nm
                m!(-1).i = 0;
                m!(-1).id = 0;
            }
            52 => {
                // refarg ::= ON INSERT refact
                m!(-2).i = 0;
                m!(-2).id = 0;
            }
            53 => {
                // refarg ::= ON DELETE refact
                let v = m!(0).i;
                m!(-2).i = v;
                m!(-2).id = 0x0000ff;
            }
            54 => {
                // refarg ::= ON UPDATE refact
                let v = m!(0).i << 8;
                m!(-2).i = v;
                m!(-2).id = 0x00ff00;
            }
            55 => m!(-1).i = OE_SetNull,  // refact ::= SET NULL
            56 => m!(-1).i = OE_SetDflt,  // refact ::= SET DEFAULT
            57 => m!(0).i = OE_Cascade,   // refact ::= CASCADE
            58 => m!(0).i = OE_Restrict,  // refact ::= RESTRICT
            59 => m!(-1).i = OE_None,     // refact ::= NO ACTION
            60 => m!(-2).i = 0,           // defer_subclause ::= NOT DEFERRABLE init_deferred_pred_opt
            61 => {
                // defer_subclause ::= DEFERRABLE init_deferred_pred_opt
                let v = m!(0).i;
                m!(-1).i = v;
            }
            64 => m!(-1).i = 0, // init_deferred_pred_opt ::= INITIALLY IMMEDIATE
            66 => p.cr_constraint_name = Token::EMPTY, // tconscomma ::= COMMA
            68 => {
                // tcons ::= PRIMARY KEY LP sortlist autoinc RP onconf
                let (list, oe, ai) = (el!(-3), m!(0).i, m!(-2).i);
                crate::build_tab::sqlite3_add_primary_key(p, list, oe, ai, 0);
            }
            69 => {
                // tcons ::= UNIQUE LP sortlist RP onconf
                let (list, oe) = (el!(-2), m!(0).i);
                crate::build_tab::sqlite3_create_index(p, None, None, SrcListId::NULL, list, oe, None, ExprId::NULL, 0, false, SQLITE_IDXTYPE_UNIQUE as u8);
            }
            70 => {
                // tcons ::= CHECK LP expr RP onconf
                let (e, lp, rp) = (ex!(-2), m!(-3).tok, m!(-1).tok);
                crate::build_tab::sqlite3_add_check_constraint(p, e, lp.z as usize, rp.z as usize);
            }
            71 => {
                // tcons ::= FOREIGN KEY LP eidlist RP REFERENCES nm eidlist_opt refargs defer_subclause_opt
                let (from, to, cols, flags, d) = (el!(-6), m!(-3).tok, el!(-2), m!(-1).i, m!(0).i);
                crate::build_tab::sqlite3_create_foreign_key(p, from, &to, cols, flags);
                crate::build_tab::sqlite3_defer_foreign_key(p, d);
            }
            73 | 75 => m!(1).i = OE_Default, // onconf ::= ; orconf ::=
            74 => {
                // onconf ::= ON CONFLICT resolvetype
                let v = m!(0).i;
                m!(-2).i = v;
            }
            77 => m!(0).i = OE_Ignore,  // resolvetype ::= IGNORE
            78 => m!(0).i = OE_Replace, // resolvetype ::= REPLACE
            82 => {
                // cmd ::= createkw temp VIEW ifnotexists nm dbnm eidlist_opt AS select
                let (n1, n2, cols, s, temp, noerr) = (m!(-4).tok, m!(-3).tok, el!(-2), sl!(0), m!(-7).i, m!(-5).i);
                if p.db.init_busy {
                    crate::build_tab::sqlite3_create_view(p, &n1, &n2, cols, s, temp != 0, noerr != 0);
                } else {
                    p.not_supported("statement kind (grammar rule not ported)");
                }
            }
            239 => {
                // cmd ::= createkw uniqueflag INDEX ifnotexists nm dbnm ON nm LP sortlist RP where_opt
                let (n1, n2, tbl, list, oe, start, wh, noerr) = (m!(-7).tok, m!(-6).tok, m!(-4).tok, el!(-2), m!(-10).i, m!(-11).tok, ex!(0), m!(-8).i);
                if p.db.init_busy {
                    let src = sqlite3_src_list_append(p, SrcListId::NULL, Some(&tbl), None);
                    crate::build_tab::sqlite3_create_index(p, Some(&n1), Some(&n2), src, list, oe, Some(&start), wh, SQLITE_SO_ASC, noerr != 0, SQLITE_IDXTYPE_APPDEF as u8);
                } else {
                    p.not_supported("statement kind (grammar rule not ported)");
                }
            }
            241 => m!(1).i = OE_None, // uniqueflag ::=
            n if (n as i32) < YYNRULE_WITH_ACTION => {
                // A rule with an action in parse.y that step V1 does not port: CREATE / DROP /
                // ALTER, INSERT / UPDATE / DELETE, triggers, PRAGMA, VACUUM, ATTACH, ANALYZE,
                // REINDEX, virtual tables.
                p.not_supported("statement kind (grammar rule not ported)");
            }
            _ => {
                // rules without an action
            }
        }
        let yygoto = YY_RULE_INFO_LHS[yyruleno];
        let below = (top as isize + yysize) as usize; // yymsp[yysize]
        let yyact = yy_find_reduce_action(self.yystack[below].stateno, yygoto);
        // yymsp += yysize+1
        let new_top = below + 1;
        self.yystack.truncate(new_top + 1);
        self.yystack[new_top].stateno = yyact;
        self.yystack[new_top].major = yygoto;
        yyact
    }
}
