//! Recursive-descent SQL statement parser and Pratt precedence-climbing expression parser
//! modeled after `sqlite/src/parse.y`.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use sqliters_types::{ErrorCode, Result, SQLITE_MAX_EXPR_DEPTH, SqliteError};

use crate::ast::*;
use crate::complete::first_statement_end_offset;
use crate::token::{
    Token, TokenKind, decode_blob_literal, dequote, parse_float_literal, parse_int_literal,
    tokenize,
};

/// Byte span of a lexical token within the input SQL string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenSpan {
    pub offset: usize,
    pub len: usize,
}

/// Maps parsed identifier `String` allocations in an AST back to their exact lexical [`TokenSpan`]
/// in the source SQL string (used for structural `ALTER TABLE` SQL rewriting).
#[derive(Debug, Clone, Default)]
pub struct IdentSpanMap {
    by_ptr: HashMap<usize, TokenSpan>,
}

impl IdentSpanMap {
    /// Returns the source [`TokenSpan`] of an AST identifier string parsed by [`parse_one_with_spans`].
    #[inline]
    pub fn span_of(&self, ident: &str) -> Option<TokenSpan> {
        self.by_ptr.get(&(ident.as_ptr() as usize)).copied()
    }
}

/// Parses a single SQL statement from the beginning of `sql`, returning `(Some(stmt), remaining_tail)`
/// or `(None, "")` if `sql` contains only whitespace, comments, and/or empty semicolons.
pub fn parse_one(sql: &str) -> Result<(Option<Stmt>, &str)> {
    let (stmt, _spans, tail) = parse_one_inner(sql, false)?;
    Ok((stmt, tail))
}

/// Parses a single SQL statement while recording the exact source [`TokenSpan`] of every parsed
/// identifier in the returned [`IdentSpanMap`].
pub fn parse_one_with_spans(sql: &str) -> Result<(Option<Stmt>, IdentSpanMap, &str)> {
    parse_one_inner(sql, true)
}

fn parse_one_inner(sql: &str, track_spans: bool) -> Result<(Option<Stmt>, IdentSpanMap, &str)> {
    let prefix_end = first_statement_end_offset(sql);
    let prefix = &sql[..prefix_end];
    let mut tokens = tokenize(prefix)?;
    disambiguate_window_keywords(&mut tokens);
    let mut parser = Parser::new(prefix, &tokens, track_spans);
    while parser.eat(TokenKind::Semi) {}
    if parser.at(TokenKind::Eof) {
        return Ok((None, IdentSpanMap::default(), ""));
    }
    let stmt = parser.parse_stmt()?;
    if !parser.at(TokenKind::Semi) && !parser.at(TokenKind::Eof) {
        let tok = parser.peek();
        return Err(parser.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text)));
    }
    if let Some(kw) = parser.orphan_join_clause {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("a JOIN clause is required before {kw}"),
        ));
    }
    let tail_off = if parser.at(TokenKind::Semi) {
        let semi = parser.advance();
        semi.offset + semi.text.len()
    } else {
        parser.peek().offset
    };
    Ok((Some(stmt), parser.ident_spans, &sql[tail_off..]))
}

/// Contextual lexer disambiguation for `WINDOW`, `OVER`, and `FILTER` (`analyzeWindowKeyword`,
/// `analyzeOverKeyword`, and `analyzeFilterKeyword` in `sqlite/src/tokenize.c:197-266, 686-695`).
fn disambiguate_window_keywords(tokens: &mut [Token<'_>]) {
    let is_get_token_id = |k: TokenKind| -> bool {
        k == TokenKind::StringLiteral
            || Parser::is_join_kw(k)
            || k == TokenKind::Window
            || k == TokenKind::Over
            || (Parser::can_be_ident(k) && k != TokenKind::Filter && k != TokenKind::Indexed)
    };

    let mut last_token: Option<TokenKind> = None;
    for i in 0..tokens.len() {
        let cur = tokens[i].kind;
        if cur == TokenKind::Eof {
            break;
        }
        if cur == TokenKind::Semi {
            last_token = Some(TokenKind::Semi);
            continue;
        }
        let next1 = tokens.get(i + 1).map_or(TokenKind::Eof, |t| t.kind);
        let next2 = tokens.get(i + 2).map_or(TokenKind::Eof, |t| t.kind);
        let resolved = match cur {
            TokenKind::Window => {
                if is_get_token_id(next1) && next2 == TokenKind::As {
                    TokenKind::Window
                } else {
                    TokenKind::Id
                }
            }
            TokenKind::Over => {
                if last_token == Some(TokenKind::Rp)
                    && (next1 == TokenKind::Lp || is_get_token_id(next1))
                {
                    TokenKind::Over
                } else {
                    TokenKind::Id
                }
            }
            TokenKind::Filter => {
                if last_token == Some(TokenKind::Rp) && next1 == TokenKind::Lp {
                    TokenKind::Filter
                } else {
                    TokenKind::Id
                }
            }
            other => other,
        };
        tokens[i].kind = resolved;
        last_token = Some(resolved);
    }
}

/// Parses all semicolon-separated SQL statements in `sql`.
pub fn parse_statements(mut sql: &str) -> Result<Vec<Stmt>> {
    let mut out = Vec::new();
    while !sql.is_empty() {
        let (maybe_stmt, tail) = parse_one(sql)?;
        if let Some(stmt) = maybe_stmt {
            out.push(stmt);
        }
        if tail.len() == sql.len() {
            break;
        }
        sql = tail;
    }
    Ok(out)
}

struct Parser<'a> {
    sql: &'a str,
    tokens: &'a [Token<'a>],
    pos: usize,
    expr_depth: usize,
    max_param_idx: usize,
    named_params: HashMap<String, usize>,
    track_spans: bool,
    ident_spans: IdentSpanMap,
    orphan_join_clause: Option<&'static str>,
    in_trigger_body: bool,
}

impl<'a> Parser<'a> {
    fn new(sql: &'a str, tokens: &'a [Token<'a>], track_spans: bool) -> Self {
        Self {
            sql,
            tokens,
            pos: 0,
            expr_depth: 0,
            max_param_idx: 0,
            named_params: HashMap::new(),
            track_spans,
            ident_spans: IdentSpanMap::default(),
            orphan_join_clause: None,
            in_trigger_body: false,
        }
    }

    #[inline]
    fn prev_token_end(&self) -> usize {
        if self.pos == 0 {
            0
        } else {
            let prev = self.tokens[self.pos - 1];
            prev.offset + prev.text.len()
        }
    }

    #[inline]
    fn peek(&self) -> Token<'a> {
        self.tokens.get(self.pos).copied().unwrap_or(Token {
            kind: TokenKind::Eof,
            text: "",
            offset: self.sql.len(),
        })
    }

    #[inline]
    fn peek_n(&self, n: usize) -> Token<'a> {
        self.tokens.get(self.pos + n).copied().unwrap_or(Token {
            kind: TokenKind::Eof,
            text: "",
            offset: self.sql.len(),
        })
    }

    #[inline]
    fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    fn advance(&mut self) -> Token<'a> {
        let tok = self.peek();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token<'a>> {
        let tok = self.peek();
        if tok.kind == kind {
            self.advance();
            Ok(tok)
        } else {
            Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text)))
        }
    }

    fn syntax_err_at(&self, tok: Token<'a>, msg: impl Into<String>) -> SqliteError {
        let mut s = msg.into();
        if tok.kind == TokenKind::Eof && s == "near \"\": syntax error" {
            s = "incomplete input".to_owned();
        }
        SqliteError::syntax(s, tok.offset)
    }

    fn alloc_ident_from_token(&mut self, tok: Token<'a>) -> String {
        let raw = dequote(tok.text);
        if self.track_spans {
            let mut owned = String::with_capacity(raw.len().max(1));
            owned.push_str(&raw);
            self.ident_spans.by_ptr.insert(
                owned.as_ptr() as usize,
                TokenSpan {
                    offset: tok.offset,
                    len: tok.text.len(),
                },
            );
            owned
        } else {
            raw
        }
    }

    /// Parses an identifier, allowing SQLite fallback keywords (`%fallback ID` in `parse.y`).
    fn parse_ident(&mut self) -> Result<String> {
        let tok = self.peek();
        if Self::can_be_ident(tok.kind) {
            self.advance();
            Ok(self.alloc_ident_from_token(tok))
        } else {
            Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text)))
        }
    }

    const fn is_join_kw(kind: TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Cross
                | TokenKind::Full
                | TokenKind::Inner
                | TokenKind::Left
                | TokenKind::Natural
                | TokenKind::Outer
                | TokenKind::Right
        )
    }

    /// Parses a DDL object/column name (`nm ::= id | STRING | JOIN_KW` in `parse.y`).
    fn parse_nm(&mut self) -> Result<String> {
        let tok = self.peek();
        if Self::can_be_ident(tok.kind)
            || Self::is_join_kw(tok.kind)
            || tok.kind == TokenKind::StringLiteral
        {
            self.advance();
            Ok(self.alloc_ident_from_token(tok))
        } else {
            Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text)))
        }
    }

    /// Parses `[schema.]object_name` into a structural [`QualifiedName`].
    fn parse_qualified_name(&mut self) -> Result<QualifiedName> {
        Ok(self.parse_qualified_name_with_offset()?.0)
    }

    /// Parses `[schema.]object_name`, returning `(QualifiedName, byte_offset_of_unqualified_name_token)`.
    fn parse_qualified_name_with_offset(&mut self) -> Result<(QualifiedName, usize)> {
        let (qn, off, _) = self.parse_qualified_name_with_token()?;
        Ok((qn, off))
    }

    /// Parses `[schema.]object_name`, returning `(QualifiedName, byte_offset_of_unqualified_name_token, raw_unqualified_token_text)`.
    fn parse_qualified_name_with_token(&mut self) -> Result<(QualifiedName, usize, &'a str)> {
        let first_tok = self.peek();
        let first = self.parse_nm()?;
        if self.eat(TokenKind::Dot) {
            let second_tok = self.peek();
            let second = self.parse_nm()?;
            Ok((
                QualifiedName::qualified(first, second),
                second_tok.offset,
                second_tok.text,
            ))
        } else {
            Ok((
                QualifiedName::unqualified(first),
                first_tok.offset,
                first_tok.text,
            ))
        }
    }

    const fn can_be_ident(kind: TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Id
                | TokenKind::QuotedId
                | TokenKind::Abort
                | TokenKind::Action
                | TokenKind::After
                | TokenKind::Always
                | TokenKind::Analyze
                | TokenKind::Asc
                | TokenKind::Attach
                | TokenKind::Before
                | TokenKind::Begin
                | TokenKind::By
                | TokenKind::Cascade
                | TokenKind::Cast
                | TokenKind::ColumnKw
                | TokenKind::Conflict
                | TokenKind::Current
                | TokenKind::Database
                | TokenKind::Deferred
                | TokenKind::Desc
                | TokenKind::Detach
                | TokenKind::Do
                | TokenKind::Each
                | TokenKind::End
                | TokenKind::Exclude
                | TokenKind::Exclusive
                | TokenKind::Explain
                | TokenKind::Fail
                | TokenKind::Filter
                | TokenKind::First
                | TokenKind::Following
                | TokenKind::For
                | TokenKind::Generated
                | TokenKind::Glob
                | TokenKind::Groups
                | TokenKind::If
                | TokenKind::Ignore
                | TokenKind::Immediate
                | TokenKind::Indexed
                | TokenKind::Initially
                | TokenKind::Instead
                | TokenKind::Key
                | TokenKind::Last
                | TokenKind::Like
                | TokenKind::Match
                | TokenKind::Materialized
                | TokenKind::No
                | TokenKind::Nothing
                | TokenKind::Nulls
                | TokenKind::Of
                | TokenKind::Offset
                | TokenKind::Others
                | TokenKind::Over
                | TokenKind::Partition
                | TokenKind::Plan
                | TokenKind::Pragma
                | TokenKind::Preceding
                | TokenKind::Query
                | TokenKind::Raise
                | TokenKind::Range
                | TokenKind::Recursive
                | TokenKind::Regexp
                | TokenKind::Reindex
                | TokenKind::Release
                | TokenKind::Rename
                | TokenKind::Replace
                | TokenKind::Restrict
                | TokenKind::Rollback
                | TokenKind::Row
                | TokenKind::Rowid
                | TokenKind::Rows
                | TokenKind::Savepoint
                | TokenKind::Strict
                | TokenKind::Temp
                | TokenKind::Ties
                | TokenKind::Trigger
                | TokenKind::Unbounded
                | TokenKind::Vacuum
                | TokenKind::View
                | TokenKind::Virtual
                | TokenKind::Window
                | TokenKind::Without
        )
    }

    const fn is_soft_alias_kw(kind: TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Current
                | TokenKind::Exclude
                | TokenKind::Following
                | TokenKind::Groups
                | TokenKind::Others
                | TokenKind::Partition
                | TokenKind::Preceding
                | TokenKind::Range
                | TokenKind::Row
                | TokenKind::Rows
                | TokenKind::Ties
                | TokenKind::Unbounded
        )
    }

    // ========================================================================
    // Top-level Statements
    // ========================================================================

    fn parse_stmt(&mut self) -> Result<Stmt> {
        let start_tok = self.peek();
        match start_tok.kind {
            TokenKind::Explain => {
                self.advance();
                let mut query_plan = false;
                if self.eat(TokenKind::Query) {
                    self.expect(TokenKind::Plan)?;
                    query_plan = true;
                }
                let inner = self.parse_stmt()?;
                Ok(Stmt::Explain {
                    query_plan,
                    stmt: Box::new(inner),
                })
            }
            TokenKind::Begin => {
                self.advance();
                let mode = if self.eat(TokenKind::Deferred) {
                    TransactionMode::Deferred
                } else if self.eat(TokenKind::Immediate) {
                    TransactionMode::Immediate
                } else if self.eat(TokenKind::Exclusive) {
                    TransactionMode::Exclusive
                } else {
                    TransactionMode::Deferred
                };
                let _ = self.eat(TokenKind::Transaction);
                Ok(Stmt::Begin { mode })
            }
            TokenKind::Commit | TokenKind::End => {
                self.advance();
                let _ = self.eat(TokenKind::Transaction);
                Ok(Stmt::Commit)
            }
            TokenKind::Rollback => {
                self.advance();
                let _ = self.eat(TokenKind::Transaction);
                let savepoint = if self.eat(TokenKind::To) {
                    let _ = self.eat(TokenKind::Savepoint);
                    Some(self.parse_nm()?)
                } else {
                    None
                };
                Ok(Stmt::Rollback { savepoint })
            }
            TokenKind::Savepoint => {
                self.advance();
                Ok(Stmt::Savepoint(self.parse_nm()?))
            }
            TokenKind::Release => {
                self.advance();
                let _ = self.eat(TokenKind::Savepoint);
                Ok(Stmt::Release(self.parse_nm()?))
            }
            TokenKind::Attach => {
                self.advance();
                if self.at(TokenKind::Database)
                    && !matches!(
                        self.peek_n(1).kind,
                        TokenKind::As | TokenKind::Dot | TokenKind::Concat
                    )
                {
                    self.advance();
                }
                let expr = self.parse_expr()?;
                self.expect(TokenKind::As)?;
                let schema_name = self.parse_expr()?;
                let key = if self.eat(TokenKind::Key) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Ok(Stmt::AttachDatabase {
                    expr,
                    schema_name,
                    key,
                })
            }
            TokenKind::Detach => {
                self.advance();
                if self.at(TokenKind::Database)
                    && !matches!(
                        self.peek_n(1).kind,
                        TokenKind::Semi | TokenKind::Eof | TokenKind::Dot | TokenKind::Concat
                    )
                {
                    self.advance();
                }
                let schema_name = self.parse_expr()?;
                Ok(Stmt::DetachDatabase { schema_name })
            }
            TokenKind::Create => self.parse_create(start_tok.offset),
            TokenKind::Drop => self.parse_drop(),
            TokenKind::Alter => self.parse_alter(),
            TokenKind::Insert | TokenKind::Replace => self.parse_insert(Vec::new()),
            TokenKind::Select | TokenKind::Values => {
                Ok(Stmt::Select(self.parse_select_after_with(Vec::new())?))
            }
            TokenKind::With => {
                let ctes = self.parse_with_clause()?;
                match self.peek().kind {
                    TokenKind::Select | TokenKind::Values => {
                        Ok(Stmt::Select(self.parse_select_after_with(ctes)?))
                    }
                    TokenKind::Insert | TokenKind::Replace => self.parse_insert(ctes),
                    TokenKind::Update => Ok(Stmt::Update(self.parse_update(ctes)?)),
                    TokenKind::Delete => Ok(Stmt::Delete(self.parse_delete(ctes)?)),
                    _ => {
                        let tok = self.peek();
                        Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text)))
                    }
                }
            }
            TokenKind::Update => Ok(Stmt::Update(self.parse_update(Vec::new())?)),
            TokenKind::Delete => Ok(Stmt::Delete(self.parse_delete(Vec::new())?)),
            TokenKind::Pragma => self.parse_pragma(),
            TokenKind::Vacuum => {
                self.advance();
                let mut schema = None;
                let mut into = None;
                if !self.at(TokenKind::Semi) && !self.at(TokenKind::Eof) {
                    if self.at(TokenKind::Into) {
                        if matches!(self.peek_n(1).kind, TokenKind::Semi | TokenKind::Eof) {
                            let tok = self.advance();
                            schema = Some(dequote(tok.text));
                        } else {
                            self.advance();
                            into = Some(self.parse_expr()?);
                        }
                    } else {
                        let schema_tok = self.peek();
                        schema = if schema_tok.kind == TokenKind::StringLiteral {
                            self.advance();
                            Some(dequote(schema_tok.text))
                        } else {
                            Some(self.parse_ident()?)
                        };
                        if self.eat(TokenKind::Into) {
                            into = Some(self.parse_expr()?);
                        }
                    }
                }
                if !self.at(TokenKind::Semi) && !self.at(TokenKind::Eof) {
                    let tok = self.peek();
                    return Err(
                        self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text))
                    );
                }
                Ok(Stmt::Vacuum { schema, into })
            }
            TokenKind::Analyze => {
                self.advance();
                let target = if !self.at(TokenKind::Semi) && !self.at(TokenKind::Eof) {
                    Some(self.parse_qualified_name()?)
                } else {
                    None
                };
                Ok(Stmt::Analyze(target))
            }
            TokenKind::Reindex => {
                self.advance();
                let target = if !self.at(TokenKind::Semi) && !self.at(TokenKind::Eof) {
                    Some(self.parse_qualified_name()?)
                } else {
                    None
                };
                Ok(Stmt::Reindex(target))
            }
            _ => Err(self.syntax_err_at(
                start_tok,
                format!("near \"{}\": syntax error", start_tok.text),
            )),
        }
    }

    fn parse_create(&mut self, stmt_start: usize) -> Result<Stmt> {
        self.expect(TokenKind::Create)?;
        let temp = self.eat(TokenKind::Temp);
        let unique = self.eat(TokenKind::Unique);

        if !temp && !unique && self.eat(TokenKind::Virtual) {
            self.expect(TokenKind::Table)?;
            let if_not_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Not)?;
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let (name, name_offset) = self.parse_qualified_name_with_offset()?;
            self.expect(TokenKind::Using)?;
            let module_name = self.parse_nm()?;
            let args = if self.at(TokenKind::Lp) {
                let lp_tok = self.expect(TokenKind::Lp)?;
                let mut depth = 1usize;
                let rp_tok = loop {
                    let tok = self.peek();
                    match tok.kind {
                        TokenKind::Lp => {
                            self.advance();
                            depth += 1;
                        }
                        TokenKind::Rp => {
                            let rp = self.advance();
                            depth -= 1;
                            if depth == 0 {
                                break rp;
                            }
                        }
                        TokenKind::Eof => {
                            return Err(self.syntax_err_at(tok, "near \"\": syntax error"));
                        }
                        _ => {
                            self.advance();
                        }
                    }
                };
                let inner_sql = &self.sql[lp_tok.offset + lp_tok.text.len()..rp_tok.offset];
                split_vtab_raw_args(inner_sql)
            } else {
                Vec::new()
            };
            let body_end = self.prev_token_end();
            let end_off = self.peek().offset;
            let catalog_sql = format!("CREATE VIRTUAL TABLE {}", &self.sql[name_offset..body_end]);
            let raw_sql = self.sql[stmt_start..end_off].trim().to_owned();
            return Ok(Stmt::CreateVirtualTable {
                if_not_exists,
                name,
                module_name,
                args,
                catalog_sql,
                raw_sql,
            });
        }

        if !unique && self.eat(TokenKind::Table) {
            let if_not_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Not)?;
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let (name, name_offset) = self.parse_qualified_name_with_offset()?;
            if self.eat(TokenKind::As) {
                let query = self.parse_select_stmt()?;
                return Ok(Stmt::CreateTableAs {
                    temp,
                    if_not_exists,
                    name,
                    query: Box::new(query),
                });
            }
            self.expect(TokenKind::Lp)?;

            let mut columns = Vec::new();
            let mut constraints = Vec::new();

            loop {
                if self.at(TokenKind::Constraint)
                    || self.at(TokenKind::Primary)
                    || self.at(TokenKind::Unique)
                    || self.at(TokenKind::Check)
                    || self.at(TokenKind::Foreign)
                {
                    constraints.push(self.parse_table_constraint()?);
                } else {
                    columns.push(self.parse_column_def()?);
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Rp)?;

            let mut without_rowid = false;
            let mut strict = false;
            loop {
                if self.eat(TokenKind::Without) {
                    self.expect(TokenKind::Rowid)?;
                    without_rowid = true;
                } else if self.eat(TokenKind::Strict) {
                    strict = true;
                } else {
                    break;
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }

            let body_end = self.prev_token_end();
            let end_off = self.peek().offset;
            let catalog_sql = format!("CREATE TABLE {}", &self.sql[name_offset..body_end]);
            let raw_sql = self.sql[stmt_start..end_off].trim().to_owned();
            return Ok(Stmt::CreateTable(CreateTableStmt {
                temp,
                if_not_exists,
                name,
                columns,
                constraints,
                without_rowid,
                strict,
                catalog_sql,
                raw_sql,
            }));
        }

        if !temp && self.eat(TokenKind::Index) {
            let if_not_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Not)?;
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let (name, name_offset) = self.parse_qualified_name_with_offset()?;
            self.expect(TokenKind::On)?;
            let table_name = self.parse_nm()?;
            self.expect(TokenKind::Lp)?;
            let mut columns = Vec::new();
            loop {
                columns.push(self.parse_indexed_column()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Rp)?;
            let where_clause = if self.eat(TokenKind::Where) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            let body_end = self.prev_token_end();
            let end_off = self.peek().offset;
            let prefix = if unique {
                "CREATE UNIQUE INDEX "
            } else {
                "CREATE INDEX "
            };
            let catalog_sql = format!("{prefix}{}", &self.sql[name_offset..body_end]);
            let raw_sql = self.sql[stmt_start..end_off].trim().to_owned();
            return Ok(Stmt::CreateIndex(CreateIndexStmt {
                unique,
                if_not_exists,
                name,
                table_name,
                columns,
                where_clause,
                catalog_sql,
                raw_sql,
            }));
        }

        if !unique && self.eat(TokenKind::View) {
            let if_not_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Not)?;
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let (name, name_offset) = self.parse_qualified_name_with_offset()?;
            let columns = if self.eat(TokenKind::Lp) {
                let mut cols = Vec::new();
                loop {
                    let col_tok = self.peek();
                    let col_name = self.parse_nm()?;
                    if matches!(
                        self.peek().kind,
                        TokenKind::Collate | TokenKind::Asc | TokenKind::Desc
                    ) {
                        return Err(self.syntax_err_at(
                            col_tok,
                            format!("syntax error after column name \"{}\"", col_tok.text),
                        ));
                    }
                    cols.push(col_name);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Rp)?;
                Some(cols)
            } else {
                None
            };
            self.expect(TokenKind::As)?;
            let query = self.parse_select_stmt()?;
            let body_end = self.prev_token_end();
            let end_off = self.peek().offset;
            let catalog_sql = format!("CREATE VIEW {}", &self.sql[name_offset..body_end]);
            let raw_sql = self.sql[stmt_start..end_off].trim().to_owned();
            return Ok(Stmt::CreateView(CreateViewStmt {
                temp,
                if_not_exists,
                name,
                columns,
                query: Box::new(query),
                catalog_sql,
                raw_sql,
            }));
        }

        if !unique && self.eat(TokenKind::Trigger) {
            let if_not_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Not)?;
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let (name, name_offset, name_tok_str) = self.parse_qualified_name_with_token()?;
            let timing = if self.eat(TokenKind::Before) {
                TriggerTiming::Before
            } else if self.eat(TokenKind::After) {
                TriggerTiming::After
            } else if self.eat(TokenKind::Instead) {
                self.expect(TokenKind::Of)?;
                TriggerTiming::InsteadOf
            } else {
                TriggerTiming::Before
            };

            let event = if self.eat(TokenKind::Insert) {
                TriggerEvent::Insert
            } else if self.eat(TokenKind::Delete) {
                TriggerEvent::Delete
            } else if self.eat(TokenKind::Update) {
                if self.eat(TokenKind::Of) {
                    let mut cols = Vec::new();
                    loop {
                        cols.push(self.parse_nm()?);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    TriggerEvent::Update {
                        columns: Some(cols),
                    }
                } else {
                    TriggerEvent::Update { columns: None }
                }
            } else {
                let tok = self.peek();
                return Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text)));
            };

            self.expect(TokenKind::On)?;
            let on_table = self.parse_qualified_name()?;
            let for_each_row = if self.eat(TokenKind::For) {
                self.expect(TokenKind::Each)?;
                self.expect(TokenKind::Row)?;
                true
            } else {
                false
            };
            self.in_trigger_body = true;
            let when_clause = if self.eat(TokenKind::When) {
                Some(self.parse_expr()?)
            } else {
                None
            };

            self.expect(TokenKind::Begin)?;
            let mut body = Vec::new();
            loop {
                body.push(self.parse_trigger_step()?);
                self.expect(TokenKind::Semi)?;
                if self.at(TokenKind::End) {
                    break;
                }
            }
            let end_tok = self.expect(TokenKind::End)?;
            self.in_trigger_body = false;
            let body_end = end_tok.offset + end_tok.text.len();
            let end_off = self.peek().offset;
            let catalog_sql = format!("CREATE TRIGGER {}", &self.sql[name_offset..body_end]);
            let raw_sql = self.sql[stmt_start..end_off].trim().to_owned();
            return Ok(Stmt::CreateTrigger(CreateTriggerStmt {
                temp,
                if_not_exists,
                name,
                name_token_text: name_tok_str.to_owned(),
                timing,
                event,
                on_table,
                for_each_row,
                when_clause,
                body,
                catalog_sql,
                raw_sql,
            }));
        }

        let tok = self.peek();
        Err(self.syntax_err_at(
            tok,
            format!("near \"{}\": unsupported CREATE object", tok.text),
        ))
    }

    fn parse_column_def(&mut self) -> Result<ColumnDef> {
        let name = self.parse_nm()?;
        let mut type_start: Option<usize> = None;
        let mut type_token_count = 0usize;

        // Read zero or more type words plus optional `(N)` or `(N, M)` precision
        while !self.at(TokenKind::Comma)
            && !self.at(TokenKind::Rp)
            && !self.is_column_constraint_start()
            && !self.at(TokenKind::Eof)
            && !self.at(TokenKind::Semi)
        {
            if self.at(TokenKind::Lp) {
                let lp_off = self.advance().offset;
                if type_start.is_none() {
                    type_start = Some(lp_off);
                }
                type_token_count += 1;
                while !self.at(TokenKind::Rp) && !self.at(TokenKind::Eof) {
                    self.advance();
                }
                self.expect(TokenKind::Rp)?;
                type_token_count += 1;
                break;
            } else if Self::can_be_ident(self.peek().kind) || self.at(TokenKind::Null) {
                if self.at(TokenKind::Null) {
                    break;
                }
                let tok = self.advance();
                if type_start.is_none() {
                    type_start = Some(tok.offset);
                }
                type_token_count += 1;
            } else {
                break;
            }
        }

        let type_name = match type_start {
            Some(start) => {
                let raw = self.sql[start..self.prev_token_end()].trim();
                if type_token_count == 1 {
                    dequote(raw)
                } else {
                    raw.to_owned()
                }
            }
            None => String::new(),
        };
        let mut constraints = Vec::new();
        while self.is_column_constraint_start() || self.at(TokenKind::Null) {
            let cname = if self.eat(TokenKind::Constraint) {
                Some(self.parse_ident()?)
            } else {
                None
            };
            if self.eat(TokenKind::Primary) {
                self.expect(TokenKind::Key)?;
                let mut desc = false;
                if self.eat(TokenKind::Asc) {
                    desc = false;
                } else if self.eat(TokenKind::Desc) {
                    desc = true;
                }
                let conflict = self.parse_optional_on_conflict()?;
                let autoincrement = self.eat(TokenKind::Autoincrement);
                constraints.push(ColumnConstraint::PrimaryKey {
                    desc,
                    conflict,
                    autoincrement,
                });
            } else if self.eat(TokenKind::Not) {
                self.expect(TokenKind::Null)?;
                let conflict = self.parse_optional_on_conflict()?;
                constraints.push(ColumnConstraint::NotNull { conflict });
            } else if self.eat(TokenKind::Null) {
                constraints.push(ColumnConstraint::Null);
            } else if self.eat(TokenKind::Unique) {
                let conflict = self.parse_optional_on_conflict()?;
                constraints.push(ColumnConstraint::Unique { conflict });
            } else if self.eat(TokenKind::Check) {
                self.expect(TokenKind::Lp)?;
                let expr_start = self.peek().offset;
                let e = self.parse_expr()?;
                let expr_end = self.peek().offset;
                let expr_sql = self.sql[expr_start..expr_end].trim().to_owned();
                self.expect(TokenKind::Rp)?;
                constraints.push(ColumnConstraint::Check {
                    name: cname,
                    expr_sql,
                    expr: e,
                });
            } else if self.eat(TokenKind::Generated) {
                self.expect(TokenKind::Always)?;
                self.expect(TokenKind::As)?;
                self.expect(TokenKind::Lp)?;
                let e = self.parse_expr()?;
                self.expect(TokenKind::Rp)?;
                let mut stored = false;
                if self.eat(TokenKind::Virtual) {
                    stored = false;
                } else if self.peek().text.eq_ignore_ascii_case("STORED") {
                    self.advance();
                    stored = true;
                }
                constraints.push(ColumnConstraint::Generated { expr: e, stored });
            } else if self.eat(TokenKind::As) {
                self.expect(TokenKind::Lp)?;
                let e = self.parse_expr()?;
                self.expect(TokenKind::Rp)?;
                let mut stored = false;
                if self.eat(TokenKind::Virtual) {
                    stored = false;
                } else if self.peek().text.eq_ignore_ascii_case("STORED") {
                    self.advance();
                    stored = true;
                }
                constraints.push(ColumnConstraint::Generated { expr: e, stored });
            } else if self.eat(TokenKind::Default) {
                let expr = if self.eat(TokenKind::Lp) {
                    let e = self.parse_expr()?;
                    self.expect(TokenKind::Rp)?;
                    e
                } else if self.at(TokenKind::Plus) || self.at(TokenKind::Minus) {
                    let sign_tok = self.advance();
                    let base = self.parse_primary_expr()?;
                    let op = if sign_tok.kind == TokenKind::Minus {
                        UnaryOp::Negate
                    } else {
                        UnaryOp::Plus
                    };
                    Expr::Unary {
                        op,
                        expr: Box::new(base),
                    }
                } else {
                    self.parse_primary_expr()?
                };
                constraints.push(ColumnConstraint::Default(expr));
            } else if self.eat(TokenKind::Collate) {
                constraints.push(ColumnConstraint::Collate(self.parse_ident()?));
            } else if self.eat(TokenKind::References) {
                let ref_table = self.parse_ident()?;
                let mut ref_columns = Vec::new();
                if self.eat(TokenKind::Lp) {
                    loop {
                        ref_columns.push(self.parse_ident()?);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::Rp)?;
                }
                let (on_delete, on_update, deferred) = self.parse_fk_tail()?;
                constraints.push(ColumnConstraint::ForeignKey {
                    ref_table,
                    ref_columns,
                    on_delete,
                    on_update,
                    deferred,
                });
            } else {
                break;
            }
        }

        Ok(ColumnDef {
            name,
            type_name,
            constraints,
        })
    }

    fn is_column_constraint_start(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Constraint
                | TokenKind::Primary
                | TokenKind::Not
                | TokenKind::Unique
                | TokenKind::Check
                | TokenKind::Default
                | TokenKind::Collate
                | TokenKind::References
                | TokenKind::Generated
                | TokenKind::As
        )
    }

    fn parse_table_constraint(&mut self) -> Result<TableConstraint> {
        let cname = if self.eat(TokenKind::Constraint) {
            Some(self.parse_ident()?)
        } else {
            None
        };
        if self.eat(TokenKind::Primary) {
            self.expect(TokenKind::Key)?;
            self.expect(TokenKind::Lp)?;
            let mut columns = Vec::new();
            loop {
                columns.push(self.parse_indexed_column()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Rp)?;
            let conflict = self.parse_optional_on_conflict()?;
            Ok(TableConstraint::PrimaryKey { columns, conflict })
        } else if self.eat(TokenKind::Unique) {
            self.expect(TokenKind::Lp)?;
            let mut columns = Vec::new();
            loop {
                columns.push(self.parse_indexed_column()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Rp)?;
            let conflict = self.parse_optional_on_conflict()?;
            Ok(TableConstraint::Unique { columns, conflict })
        } else if self.eat(TokenKind::Check) {
            self.expect(TokenKind::Lp)?;
            let expr_start = self.peek().offset;
            let expr = self.parse_expr()?;
            let expr_end = self.peek().offset;
            let expr_sql = self.sql[expr_start..expr_end].trim().to_owned();
            self.expect(TokenKind::Rp)?;
            // SQLite legacy quirk (`conflict-16.2`): ON CONFLICT is allowed on table CHECK constraints (ignored)
            let _ = self.parse_optional_on_conflict()?;
            Ok(TableConstraint::Check {
                name: cname,
                expr_sql,
                expr,
            })
        } else if self.eat(TokenKind::Foreign) {
            self.expect(TokenKind::Key)?;
            self.expect(TokenKind::Lp)?;
            let mut columns = Vec::new();
            loop {
                columns.push(self.parse_ident()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Rp)?;
            self.expect(TokenKind::References)?;
            let ref_table = self.parse_ident()?;
            let mut ref_columns = Vec::new();
            if self.eat(TokenKind::Lp) {
                loop {
                    ref_columns.push(self.parse_ident()?);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Rp)?;
            }
            let (on_delete, on_update, deferred) = self.parse_fk_tail()?;
            Ok(TableConstraint::ForeignKey {
                columns,
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                deferred,
            })
        } else {
            let tok = self.peek();
            Err(self.syntax_err_at(tok, "expected table constraint"))
        }
    }

    fn parse_fk_tail(&mut self) -> Result<(FkAction, FkAction, bool)> {
        let mut on_delete = FkAction::NoAction;
        let mut on_update = FkAction::NoAction;
        let mut deferred = false;

        loop {
            if self.at(TokenKind::On)
                && matches!(self.peek_n(1).kind, TokenKind::Delete | TokenKind::Update)
            {
                self.advance();
                let is_delete = if self.eat(TokenKind::Delete) {
                    true
                } else {
                    self.expect(TokenKind::Update)?;
                    false
                };
                let act = self.parse_fk_action()?;
                if is_delete {
                    on_delete = act;
                } else {
                    on_update = act;
                }
            } else if self.eat(TokenKind::Match) {
                let tok = self.peek();
                if tok.kind == TokenKind::Eof {
                    return Err(self.syntax_err_at(tok, "expected MATCH type"));
                }
                self.advance();
            } else if self.at(TokenKind::Not) && self.peek_n(1).kind == TokenKind::Deferrable {
                self.advance();
                self.advance();
                if self.eat(TokenKind::Initially)
                    && !self.eat(TokenKind::Deferred)
                    && !self.eat(TokenKind::Immediate)
                {
                    let tok = self.peek();
                    return Err(self.syntax_err_at(tok, "expected DEFERRED or IMMEDIATE"));
                }
                deferred = false;
            } else if self.eat(TokenKind::Deferrable) {
                if self.eat(TokenKind::Initially) {
                    if self.eat(TokenKind::Deferred) {
                        deferred = true;
                    } else if self.eat(TokenKind::Immediate) {
                        deferred = false;
                    } else {
                        let tok = self.peek();
                        return Err(self.syntax_err_at(tok, "expected DEFERRED or IMMEDIATE"));
                    }
                } else {
                    deferred = false;
                }
            } else {
                break;
            }
        }

        Ok((on_delete, on_update, deferred))
    }

    fn parse_fk_action(&mut self) -> Result<FkAction> {
        if self.eat(TokenKind::Set) {
            if self.eat(TokenKind::Null) {
                Ok(FkAction::SetNull)
            } else if self.eat(TokenKind::Default) {
                Ok(FkAction::SetDefault)
            } else {
                let tok = self.peek();
                Err(self.syntax_err_at(tok, "expected NULL or DEFAULT after SET"))
            }
        } else if self.eat(TokenKind::Cascade) {
            Ok(FkAction::Cascade)
        } else if self.eat(TokenKind::Restrict) {
            Ok(FkAction::Restrict)
        } else if self.eat(TokenKind::No) {
            self.expect(TokenKind::Action)?;
            Ok(FkAction::NoAction)
        } else {
            let tok = self.peek();
            Err(self.syntax_err_at(tok, "expected foreign key action"))
        }
    }

    fn parse_optional_on_conflict(&mut self) -> Result<ConflictAction> {
        if self.eat(TokenKind::On) {
            self.expect(TokenKind::Conflict)?;
            self.parse_conflict_action()
        } else {
            Ok(ConflictAction::Abort)
        }
    }

    fn parse_conflict_action(&mut self) -> Result<ConflictAction> {
        let tok = self.peek();
        let action = match tok.kind {
            TokenKind::Rollback => ConflictAction::Rollback,
            TokenKind::Abort => ConflictAction::Abort,
            TokenKind::Fail => ConflictAction::Fail,
            TokenKind::Ignore => ConflictAction::Ignore,
            TokenKind::Replace => ConflictAction::Replace,
            _ => return Err(self.syntax_err_at(tok, "expected conflict resolution action")),
        };
        self.advance();
        Ok(action)
    }

    fn parse_indexed_column(&mut self) -> Result<IndexedColumn> {
        let mut raw_expr = self.parse_expr()?;
        let mut collation = None;
        while let Expr::Collate { expr, collation: c } = raw_expr {
            collation = Some(c);
            raw_expr = *expr;
        }
        if self.eat(TokenKind::Collate) {
            collation = Some(self.parse_ident()?);
        }
        let (name, expr) = match raw_expr {
            Expr::Column(ColumnRef {
                schema: None,
                table: None,
                column,
            }) => (column, None),
            Expr::String(s) => (s, None),
            other => (String::new(), Some(other)),
        };
        let mut desc = false;
        if self.eat(TokenKind::Asc) {
            desc = false;
        } else if self.eat(TokenKind::Desc) {
            desc = true;
        }
        let nulls_last = if self.eat(TokenKind::Nulls) {
            if self.eat(TokenKind::First) {
                Some(false)
            } else {
                self.expect(TokenKind::Last)?;
                Some(true)
            }
        } else {
            None
        };
        Ok(IndexedColumn {
            name,
            expr,
            collation,
            desc,
            nulls_last,
        })
    }

    fn parse_drop(&mut self) -> Result<Stmt> {
        self.expect(TokenKind::Drop)?;
        if self.eat(TokenKind::Table) {
            let if_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let name = self.parse_qualified_name()?;
            Ok(Stmt::DropTable { if_exists, name })
        } else if self.eat(TokenKind::Index) {
            let if_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let name = self.parse_qualified_name()?;
            Ok(Stmt::DropIndex { if_exists, name })
        } else if self.eat(TokenKind::View) {
            let if_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let name = self.parse_qualified_name()?;
            Ok(Stmt::DropView { if_exists, name })
        } else if self.eat(TokenKind::Trigger) {
            let if_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let name = self.parse_qualified_name()?;
            Ok(Stmt::DropTrigger { if_exists, name })
        } else {
            let tok = self.peek();
            Err(self.syntax_err_at(tok, "expected TABLE, INDEX, VIEW, or TRIGGER after DROP"))
        }
    }

    fn parse_alter(&mut self) -> Result<Stmt> {
        self.expect(TokenKind::Alter)?;
        self.expect(TokenKind::Table)?;
        let table = self.parse_qualified_name()?;
        if self.eat(TokenKind::Rename) {
            if self.eat(TokenKind::To) {
                let new_name = self.parse_nm()?;
                Ok(Stmt::AlterTable {
                    table,
                    action: AlterTableAction::RenameTo(new_name),
                })
            } else {
                let _ = self.eat(TokenKind::ColumnKw);
                let old_name = self.parse_nm()?;
                self.expect(TokenKind::To)?;
                let new_name = self.parse_nm()?;
                Ok(Stmt::AlterTable {
                    table,
                    action: AlterTableAction::RenameColumn { old_name, new_name },
                })
            }
        } else if self.eat(TokenKind::Add) {
            let _ = self.eat(TokenKind::ColumnKw);
            let col = self.parse_column_def()?;
            Ok(Stmt::AlterTable {
                table,
                action: AlterTableAction::AddColumn(col),
            })
        } else if self.eat(TokenKind::Drop) {
            let _ = self.eat(TokenKind::ColumnKw);
            let col_name = self.parse_nm()?;
            Ok(Stmt::AlterTable {
                table,
                action: AlterTableAction::DropColumn(col_name),
            })
        } else {
            let tok = self.peek();
            Err(self.syntax_err_at(tok, "expected RENAME, ADD, or DROP in ALTER TABLE"))
        }
    }

    fn parse_insert(&mut self, ctes: Vec<CommonTableExpr>) -> Result<Stmt> {
        let or_conflict = if self.eat(TokenKind::Replace) {
            Some(ConflictAction::Replace)
        } else {
            self.expect(TokenKind::Insert)?;
            if self.eat(TokenKind::Or) {
                Some(self.parse_conflict_action()?)
            } else {
                None
            }
        };
        self.expect(TokenKind::Into)?;
        let table = self.parse_qualified_name()?;
        let alias = if self.eat(TokenKind::As) {
            Some(self.parse_nm()?)
        } else {
            None
        };

        let columns = if self.at(TokenKind::Lp) && self.peek_n(1).kind != TokenKind::Select {
            self.expect(TokenKind::Lp)?;
            let mut cols = Vec::new();
            loop {
                cols.push(self.parse_nm()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Rp)?;
            Some(cols)
        } else {
            None
        };

        let source = if self.eat(TokenKind::Default) {
            self.expect(TokenKind::Values)?;
            InsertSource::DefaultValues
        } else if self.at(TokenKind::Values) && self.peek_n(1).kind == TokenKind::Lp {
            self.advance();
            let rows = self.parse_values_rows()?;
            // If followed by UNION/ORDER BY/LIMIT, treat as Select
            if matches!(
                self.peek().kind,
                TokenKind::Union
                    | TokenKind::Intersect
                    | TokenKind::Except
                    | TokenKind::Order
                    | TokenKind::Limit
            ) {
                let mut order_by = self.parse_optional_order_by()?;
                let mut limit = self.parse_optional_limit()?;
                let mut compounds = Vec::new();
                while let Some(op) = self.eat_compound_op() {
                    if !order_by.is_empty() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "ORDER BY clause should come after {} not before",
                                op.as_sql_str()
                            ),
                        ));
                    }
                    if limit.is_some() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "LIMIT clause should come after {} not before",
                                op.as_sql_str()
                            ),
                        ));
                    }
                    compounds.push((op, self.parse_select_core()?));
                    order_by = self.parse_optional_order_by()?;
                    limit = self.parse_optional_limit()?;
                }
                InsertSource::Select(Box::new(SelectStmt {
                    ctes: Vec::new(),
                    body: SelectBody {
                        head: SelectCore::Values(rows),
                        compounds,
                    },
                    order_by,
                    limit,
                }))
            } else {
                InsertSource::Values(rows)
            }
        } else {
            InsertSource::Select(Box::new(self.parse_select_stmt()?))
        };

        let upsert_clauses = if matches!(source, InsertSource::DefaultValues) {
            Vec::new()
        } else {
            self.parse_upsert_clauses()?
        };
        let returning = self.parse_optional_returning()?;
        Ok(Stmt::Insert(InsertStmt {
            ctes,
            or_conflict,
            table,
            alias,
            columns,
            source,
            upsert_clauses,
            returning,
        }))
    }

    fn parse_upsert_target_column(&mut self) -> Result<IndexedColumn> {
        let mut raw_expr = self.parse_expr()?;
        let mut collation = None;
        while let Expr::Collate { expr, collation: c } = raw_expr {
            collation = Some(c);
            raw_expr = *expr;
        }
        if self.eat(TokenKind::Collate) {
            collation = Some(self.parse_ident()?);
        }
        let (name, expr) = match raw_expr {
            Expr::Column(ColumnRef {
                schema: None,
                table: None,
                column,
            }) => (column, None),
            other => (String::new(), Some(other)),
        };
        let mut desc = false;
        if self.eat(TokenKind::Asc) {
            desc = false;
        } else if self.eat(TokenKind::Desc) {
            desc = true;
        }
        let nulls_last = if self.eat(TokenKind::Nulls) {
            if self.eat(TokenKind::First) {
                Some(false)
            } else {
                self.expect(TokenKind::Last)?;
                Some(true)
            }
        } else {
            None
        };
        Ok(IndexedColumn {
            name,
            expr,
            collation,
            desc,
            nulls_last,
        })
    }

    fn parse_upsert_clauses(&mut self) -> Result<Vec<UpsertClause>> {
        let mut clauses = Vec::new();
        while self.eat(TokenKind::On) {
            self.expect(TokenKind::Conflict)?;
            let target = if self.eat(TokenKind::Lp) {
                let mut cols = Vec::new();
                loop {
                    cols.push(self.parse_upsert_target_column()?);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Rp)?;
                let where_clause = if self.eat(TokenKind::Where) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Some(UpsertTarget {
                    columns: cols,
                    where_clause,
                })
            } else {
                None
            };

            self.expect(TokenKind::Do)?;
            let action = if self.eat(TokenKind::Nothing) {
                UpsertAction::DoNothing
            } else if self.eat(TokenKind::Update) {
                self.expect(TokenKind::Set)?;
                let assignments = self.parse_set_assignments()?;
                let where_clause = if self.eat(TokenKind::Where) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                UpsertAction::DoUpdate {
                    assignments,
                    where_clause,
                }
            } else {
                let tok = self.peek();
                return Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text)));
            };

            let is_targetless = target.is_none();
            clauses.push(UpsertClause { target, action });
            if is_targetless {
                break;
            }
        }
        Ok(clauses)
    }

    fn parse_set_assignments(&mut self) -> Result<Vec<SetAssignment>> {
        let mut assignments = Vec::new();
        loop {
            if self.eat(TokenKind::Lp) {
                let mut columns = Vec::new();
                loop {
                    columns.push(self.parse_nm()?);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Rp)?;
                self.expect(TokenKind::Eq)?;
                let expr = self.parse_expr()?;
                assignments.push(SetAssignment {
                    columns,
                    row_value: true,
                    expr,
                });
            } else {
                let col = self.parse_nm()?;
                self.expect(TokenKind::Eq)?;
                let expr = self.parse_expr()?;
                assignments.push(SetAssignment {
                    columns: vec![col],
                    row_value: false,
                    expr,
                });
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        Ok(assignments)
    }

    fn check_trigger_indexed_by(&mut self) -> Result<()> {
        if self.eat(TokenKind::Indexed) {
            self.expect(TokenKind::By)?;
            let _ = self.parse_nm()?;
            return Err(SqliteError::new(
                ErrorCode::Error,
                "the INDEXED BY clause is not allowed on UPDATE or DELETE statements within triggers",
            ));
        }
        if self.at(TokenKind::Not) && self.peek_n(1).kind == TokenKind::Indexed {
            self.advance();
            self.advance();
            return Err(SqliteError::new(
                ErrorCode::Error,
                "the NOT INDEXED clause is not allowed on UPDATE or DELETE statements within triggers",
            ));
        }
        Ok(())
    }

    fn parse_trigger_step(&mut self) -> Result<TriggerStep> {
        let tok = self.peek();
        match tok.kind {
            TokenKind::Select | TokenKind::Values | TokenKind::With => {
                Ok(TriggerStep::Select(self.parse_select_stmt()?))
            }
            TokenKind::Insert | TokenKind::Replace => {
                let or_conflict = if self.eat(TokenKind::Replace) {
                    Some(ConflictAction::Replace)
                } else {
                    self.expect(TokenKind::Insert)?;
                    if self.eat(TokenKind::Or) {
                        Some(self.parse_conflict_action()?)
                    } else {
                        None
                    }
                };
                self.expect(TokenKind::Into)?;
                let table = self.parse_qualified_name()?;
                let alias = if self.eat(TokenKind::As) {
                    Some(self.parse_nm()?)
                } else {
                    None
                };
                let columns = if self.at(TokenKind::Lp) && self.peek_n(1).kind != TokenKind::Select
                {
                    self.expect(TokenKind::Lp)?;
                    let mut cols = Vec::new();
                    loop {
                        cols.push(self.parse_nm()?);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::Rp)?;
                    Some(cols)
                } else {
                    None
                };
                let source = if self.at(TokenKind::Values) && self.peek_n(1).kind == TokenKind::Lp {
                    self.advance();
                    let rows = self.parse_values_rows()?;
                    if matches!(
                        self.peek().kind,
                        TokenKind::Union
                            | TokenKind::Intersect
                            | TokenKind::Except
                            | TokenKind::Order
                            | TokenKind::Limit
                    ) {
                        let mut order_by = self.parse_optional_order_by()?;
                        let mut limit = self.parse_optional_limit()?;
                        let mut compounds = Vec::new();
                        while let Some(op) = self.eat_compound_op() {
                            if !order_by.is_empty() {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    format!(
                                        "ORDER BY clause should come after {} not before",
                                        op.as_sql_str()
                                    ),
                                ));
                            }
                            if limit.is_some() {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    format!(
                                        "LIMIT clause should come after {} not before",
                                        op.as_sql_str()
                                    ),
                                ));
                            }
                            compounds.push((op, self.parse_select_core()?));
                            order_by = self.parse_optional_order_by()?;
                            limit = self.parse_optional_limit()?;
                        }
                        InsertSource::Select(Box::new(SelectStmt {
                            ctes: Vec::new(),
                            body: SelectBody {
                                head: SelectCore::Values(rows),
                                compounds,
                            },
                            order_by,
                            limit,
                        }))
                    } else {
                        InsertSource::Values(rows)
                    }
                } else if matches!(self.peek().kind, TokenKind::Select | TokenKind::With) {
                    InsertSource::Select(Box::new(self.parse_select_stmt()?))
                } else {
                    let bad = self.peek();
                    return Err(
                        self.syntax_err_at(bad, format!("near \"{}\": syntax error", bad.text))
                    );
                };
                let upsert_clauses = self.parse_upsert_clauses()?;
                if self.eat(TokenKind::Returning) {
                    let _ = self.parse_projection()?;
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "cannot use RETURNING in a trigger",
                    ));
                }
                Ok(TriggerStep::Insert(InsertStmt {
                    ctes: Vec::new(),
                    or_conflict,
                    table,
                    alias,
                    columns,
                    source,
                    upsert_clauses,
                    returning: Vec::new(),
                }))
            }
            TokenKind::Update => {
                self.expect(TokenKind::Update)?;
                let or_conflict = if self.eat(TokenKind::Or) {
                    Some(self.parse_conflict_action()?)
                } else {
                    None
                };
                let table = self.parse_qualified_name()?;
                let alias = if self.eat(TokenKind::As) {
                    Some(self.parse_nm()?)
                } else {
                    None
                };
                self.check_trigger_indexed_by()?;
                self.expect(TokenKind::Set)?;
                let assignments = self.parse_set_assignments()?;
                let from = if self.eat(TokenKind::From) {
                    Some(self.parse_from_clause()?)
                } else {
                    None
                };
                let selection = if self.eat(TokenKind::Where) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Ok(TriggerStep::Update(UpdateStmt {
                    ctes: Vec::new(),
                    or_conflict,
                    table,
                    alias,
                    indexed_by: None,
                    assignments,
                    from,
                    selection,
                    returning: Vec::new(),
                }))
            }
            TokenKind::Delete => {
                self.expect(TokenKind::Delete)?;
                self.expect(TokenKind::From)?;
                let table = self.parse_qualified_name()?;
                let alias = if self.eat(TokenKind::As) {
                    Some(self.parse_nm()?)
                } else {
                    None
                };
                self.check_trigger_indexed_by()?;
                let selection = if self.eat(TokenKind::Where) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Ok(TriggerStep::Delete(DeleteStmt {
                    ctes: Vec::new(),
                    table,
                    alias,
                    indexed_by: None,
                    selection,
                    returning: Vec::new(),
                }))
            }
            _ => Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text))),
        }
    }

    fn parse_values_rows(&mut self) -> Result<Vec<Vec<Expr>>> {
        let mut rows = Vec::new();
        loop {
            self.expect(TokenKind::Lp)?;
            let mut row = Vec::new();
            loop {
                row.push(self.parse_expr()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Rp)?;
            rows.push(row);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        Ok(rows)
    }

    fn parse_update(&mut self, ctes: Vec<CommonTableExpr>) -> Result<UpdateStmt> {
        self.expect(TokenKind::Update)?;
        let or_conflict = if self.eat(TokenKind::Or) {
            Some(self.parse_conflict_action()?)
        } else {
            None
        };
        let table = self.parse_qualified_name()?;
        let alias = self.parse_optional_table_alias(&[TokenKind::Set])?;
        let indexed_by = self.parse_optional_index_hint()?;
        self.expect(TokenKind::Set)?;

        let assignments = self.parse_set_assignments()?;
        let from = if self.eat(TokenKind::From) {
            Some(self.parse_from_clause()?)
        } else {
            None
        };
        let selection = if self.eat(TokenKind::Where) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let returning = self.parse_optional_returning()?;

        Ok(UpdateStmt {
            ctes,
            or_conflict,
            table,
            alias,
            indexed_by,
            assignments,
            from,
            selection,
            returning,
        })
    }

    fn parse_delete(&mut self, ctes: Vec<CommonTableExpr>) -> Result<DeleteStmt> {
        self.expect(TokenKind::Delete)?;
        self.expect(TokenKind::From)?;
        let table = self.parse_qualified_name()?;
        let alias = self.parse_optional_table_alias(&[TokenKind::Where, TokenKind::Returning])?;
        let indexed_by = self.parse_optional_index_hint()?;
        let selection = if self.eat(TokenKind::Where) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let returning = self.parse_optional_returning()?;
        Ok(DeleteStmt {
            ctes,
            table,
            alias,
            indexed_by,
            selection,
            returning,
        })
    }

    fn parse_optional_returning(&mut self) -> Result<Vec<ResultColumn>> {
        if !self.eat(TokenKind::Returning) {
            return Ok(Vec::new());
        }
        self.parse_projection()
    }

    fn parse_pragma(&mut self) -> Result<Stmt> {
        self.expect(TokenKind::Pragma)?;
        let qn = self.parse_qualified_name()?;
        let value = if self.eat(TokenKind::Eq) {
            Some(self.parse_pragma_value()?)
        } else if self.eat(TokenKind::Lp) {
            let v = self.parse_pragma_value()?;
            self.expect(TokenKind::Rp)?;
            Some(v)
        } else {
            None
        };
        Ok(Stmt::Pragma {
            schema: qn.schema,
            name: qn.name,
            value,
        })
    }

    fn parse_pragma_value(&mut self) -> Result<PragmaValue> {
        let neg = if self.eat(TokenKind::Minus) {
            -1i64
        } else {
            let _ = self.eat(TokenKind::Plus);
            1i64
        };
        let tok = self.peek();
        match tok.kind {
            TokenKind::Integer => {
                self.advance();
                let v = parse_int_literal(tok.text)?.unwrap_or(0) * neg;
                Ok(PragmaValue::Integer(v))
            }
            TokenKind::Float => {
                self.advance();
                let s = if neg < 0 {
                    format!("-{}", tok.text)
                } else {
                    tok.text.to_owned()
                };
                Ok(PragmaValue::String(s))
            }
            TokenKind::StringLiteral => {
                self.advance();
                Ok(PragmaValue::String(dequote(tok.text)))
            }
            TokenKind::True => {
                self.advance();
                Ok(PragmaValue::Integer(1))
            }
            TokenKind::False => {
                self.advance();
                Ok(PragmaValue::Integer(0))
            }
            TokenKind::QuotedId | TokenKind::Id => {
                self.advance();
                Ok(PragmaValue::Ident(dequote(tok.text)))
            }
            _ if tok
                .text
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') =>
            {
                self.advance();
                Ok(PragmaValue::Ident(dequote(tok.text)))
            }
            _ => Err(self.syntax_err_at(tok, "invalid PRAGMA value")),
        }
    }

    // ========================================================================
    // SELECT & Clauses
    // ========================================================================

    fn parse_with_clause(&mut self) -> Result<Vec<CommonTableExpr>> {
        self.expect(TokenKind::With)?;
        let _ = self.eat(TokenKind::Recursive);
        let mut ctes: Vec<CommonTableExpr> = Vec::new();
        loop {
            let name = self.parse_ident()?;
            if ctes.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("duplicate WITH table name: {name}"),
                ));
            }
            let columns = if self.eat(TokenKind::Lp) {
                let mut cols = Vec::new();
                loop {
                    cols.push(self.parse_ident()?);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Rp)?;
                Some(cols)
            } else {
                None
            };
            self.expect(TokenKind::As)?;
            let materialized = if self.eat(TokenKind::Materialized) {
                Some(true)
            } else if self.eat(TokenKind::Not) {
                self.expect(TokenKind::Materialized)?;
                Some(false)
            } else {
                None
            };
            self.expect(TokenKind::Lp)?;
            let query = self.parse_select_stmt()?;
            self.expect(TokenKind::Rp)?;
            ctes.push(CommonTableExpr {
                name,
                columns,
                materialized,
                query,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        Ok(ctes)
    }

    fn parse_select_stmt(&mut self) -> Result<SelectStmt> {
        let ctes = if self.at(TokenKind::With) {
            self.parse_with_clause()?
        } else {
            Vec::new()
        };
        self.parse_select_after_with(ctes)
    }

    fn parse_select_after_with(&mut self, ctes: Vec<CommonTableExpr>) -> Result<SelectStmt> {
        let head = self.parse_select_core()?;
        let mut order_by = self.parse_optional_order_by()?;
        let mut limit = self.parse_optional_limit()?;
        let mut compounds = Vec::new();
        while let Some(op) = self.eat_compound_op() {
            if !order_by.is_empty() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "ORDER BY clause should come after {} not before",
                        op.as_sql_str()
                    ),
                ));
            }
            if limit.is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "LIMIT clause should come after {} not before",
                        op.as_sql_str()
                    ),
                ));
            }
            let rhs = self.parse_select_core()?;
            compounds.push((op, rhs));
            order_by = self.parse_optional_order_by()?;
            limit = self.parse_optional_limit()?;
        }

        Ok(SelectStmt {
            ctes,
            body: SelectBody { head, compounds },
            order_by,
            limit,
        })
    }

    fn eat_compound_op(&mut self) -> Option<CompoundOp> {
        if self.eat(TokenKind::Union) {
            if self.eat(TokenKind::All) {
                Some(CompoundOp::UnionAll)
            } else {
                Some(CompoundOp::Union)
            }
        } else if self.eat(TokenKind::Intersect) {
            Some(CompoundOp::Intersect)
        } else if self.eat(TokenKind::Except) {
            Some(CompoundOp::Except)
        } else {
            None
        }
    }

    fn parse_select_core(&mut self) -> Result<SelectCore> {
        if self.eat(TokenKind::Values) {
            return Ok(SelectCore::Values(self.parse_values_rows()?));
        }

        self.expect(TokenKind::Select)?;
        let mut distinct = false;
        if self.eat(TokenKind::Distinct) {
            distinct = true;
        } else {
            let _ = self.eat(TokenKind::All);
        }

        let projection = self.parse_projection()?;

        let from = if self.eat(TokenKind::From) {
            Some(self.parse_from_clause()?)
        } else {
            None
        };

        let selection = if self.eat(TokenKind::Where) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        let mut group_by = Vec::new();
        let mut having = None;
        if self.eat(TokenKind::Group) {
            self.expect(TokenKind::By)?;
            loop {
                group_by.push(self.parse_expr()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            if self.eat(TokenKind::Having) {
                having = Some(self.parse_expr()?);
            }
        } else if self.eat(TokenKind::Having) {
            having = Some(self.parse_expr()?);
        }

        let windows = if self.eat(TokenKind::Window) {
            self.parse_windowdefn_list()?
        } else {
            Vec::new()
        };

        Ok(SelectCore::Select {
            distinct,
            projection,
            from,
            selection,
            group_by,
            having,
            windows,
        })
    }

    fn parse_windowdefn_list(&mut self) -> Result<Vec<(String, WindowSpec)>> {
        let mut defs = Vec::new();
        loop {
            let name = self.parse_nm()?;
            self.expect(TokenKind::As)?;
            self.expect(TokenKind::Lp)?;
            let spec = self.parse_window_spec()?;
            self.expect(TokenKind::Rp)?;
            defs.push((name, spec));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        Ok(defs)
    }

    fn parse_window_spec(&mut self) -> Result<WindowSpec> {
        if self.at(TokenKind::Rp) {
            return Ok(WindowSpec::default());
        }
        let base_window = if matches!(
            self.peek().kind,
            TokenKind::Partition
                | TokenKind::Order
                | TokenKind::Range
                | TokenKind::Rows
                | TokenKind::Groups
        ) {
            None
        } else {
            Some(self.parse_nm()?)
        };

        let mut partition_by = Vec::new();
        if self.eat(TokenKind::Partition) {
            self.expect(TokenKind::By)?;
            loop {
                partition_by.push(self.parse_expr()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
        }

        let order_by = self.parse_optional_order_by()?;
        let frame = self.parse_optional_window_frame()?;

        Ok(WindowSpec {
            base_window,
            partition_by,
            order_by,
            frame,
        })
    }

    fn parse_optional_window_frame(&mut self) -> Result<Option<WindowFrame>> {
        let units = if self.eat(TokenKind::Rows) {
            WindowFrameUnits::Rows
        } else if self.eat(TokenKind::Range) {
            WindowFrameUnits::Range
        } else if self.eat(TokenKind::Groups) {
            WindowFrameUnits::Groups
        } else {
            return Ok(None);
        };

        let (start, end) = if self.eat(TokenKind::Between) {
            let s = self.parse_window_frame_bound(true)?;
            self.expect(TokenKind::And)?;
            let e = self.parse_window_frame_bound(false)?;
            (s, e)
        } else {
            let s = self.parse_window_frame_bound(true)?;
            (s, WindowFrameBound::CurrentRow)
        };

        let exclude = if self.eat(TokenKind::Exclude) {
            if self.eat(TokenKind::No) {
                self.expect(TokenKind::Others)?;
                WindowFrameExclude::NoOthers
            } else if self.eat(TokenKind::Current) {
                self.expect(TokenKind::Row)?;
                WindowFrameExclude::CurrentRow
            } else if self.eat(TokenKind::Group) {
                WindowFrameExclude::Group
            } else if self.eat(TokenKind::Ties) {
                WindowFrameExclude::Ties
            } else {
                let bad = self.peek();
                return Err(self.syntax_err_at(bad, format!("near \"{}\": syntax error", bad.text)));
            }
        } else {
            WindowFrameExclude::NoOthers
        };

        // Match `sqlite3WindowAlloc` (`sqlite/src/window.c:1221-1226`):
        let invalid_order = matches!(
            (&start, &end),
            (WindowFrameBound::CurrentRow, WindowFrameBound::Preceding(_))
                | (
                    WindowFrameBound::Following(_),
                    WindowFrameBound::Preceding(_) | WindowFrameBound::CurrentRow
                )
        );
        if invalid_order {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "unsupported frame specification",
            ));
        }

        Ok(Some(WindowFrame {
            units,
            start,
            end,
            exclude,
        }))
    }

    fn parse_window_frame_bound(&mut self, is_start: bool) -> Result<WindowFrameBound> {
        if self.eat(TokenKind::Unbounded) {
            if is_start {
                self.expect(TokenKind::Preceding)?;
                return Ok(WindowFrameBound::UnboundedPreceding);
            } else {
                self.expect(TokenKind::Following)?;
                return Ok(WindowFrameBound::UnboundedFollowing);
            }
        }
        if self.at(TokenKind::Current) && self.peek_n(1).kind == TokenKind::Row {
            self.advance();
            self.advance();
            return Ok(WindowFrameBound::CurrentRow);
        }
        let expr = self.parse_expr()?;
        if self.eat(TokenKind::Preceding) {
            Ok(WindowFrameBound::Preceding(Box::new(expr)))
        } else if self.eat(TokenKind::Following) {
            Ok(WindowFrameBound::Following(Box::new(expr)))
        } else {
            let bad = self.peek();
            Err(self.syntax_err_at(bad, format!("near \"{}\": syntax error", bad.text)))
        }
    }

    fn parse_projection(&mut self) -> Result<Vec<ResultColumn>> {
        let mut cols = Vec::new();
        loop {
            if self.eat(TokenKind::Star) {
                cols.push(ResultColumn::Star);
            } else if (Self::can_be_ident(self.peek().kind) || Self::is_join_kw(self.peek().kind))
                && self.peek_n(1).kind == TokenKind::Dot
                && self.peek_n(2).kind == TokenKind::Star
            {
                let tbl = self.parse_nm()?;
                self.expect(TokenKind::Dot)?;
                self.expect(TokenKind::Star)?;
                cols.push(ResultColumn::TableStar {
                    schema: None,
                    table: tbl,
                });
            } else if (Self::can_be_ident(self.peek().kind) || Self::is_join_kw(self.peek().kind))
                && self.peek_n(1).kind == TokenKind::Dot
                && (Self::can_be_ident(self.peek_n(2).kind)
                    || Self::is_join_kw(self.peek_n(2).kind))
                && self.peek_n(3).kind == TokenKind::Dot
                && self.peek_n(4).kind == TokenKind::Star
            {
                let schema = self.parse_nm()?;
                self.expect(TokenKind::Dot)?;
                let tbl = self.parse_nm()?;
                self.expect(TokenKind::Dot)?;
                self.expect(TokenKind::Star)?;
                cols.push(ResultColumn::TableStar {
                    schema: Some(schema),
                    table: tbl,
                });
            } else {
                let expr_start = self.peek().offset;
                let expr = self.parse_expr()?;
                let expr_end = self.prev_token_end();
                let alias = if self.eat(TokenKind::As) {
                    Some(self.parse_nm()?)
                } else if self.at(TokenKind::Id)
                    || self.at(TokenKind::QuotedId)
                    || self.at(TokenKind::StringLiteral)
                    || Self::is_soft_alias_kw(self.peek().kind)
                {
                    // Implicit alias `SELECT expr alias` when not a clause keyword
                    let tok = self.advance();
                    Some(self.alloc_ident_from_token(tok))
                } else {
                    None
                };
                let span_text = if alias.is_none() && expr_end >= expr_start {
                    Some(self.sql[expr_start..expr_end].to_owned())
                } else {
                    None
                };
                cols.push(ResultColumn::Expr {
                    expr,
                    alias,
                    span_text,
                });
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        Ok(cols)
    }

    fn parse_from_clause(&mut self) -> Result<FromClause> {
        let head = self.parse_table_or_subquery()?;
        if self.eat(TokenKind::On) {
            let _ = self.parse_expr()?;
            if self.orphan_join_clause.is_none() {
                self.orphan_join_clause = Some("ON");
            }
        } else if self.eat(TokenKind::Using) {
            self.expect(TokenKind::Lp)?;
            loop {
                let _ = self.parse_ident()?;
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Rp)?;
            if self.orphan_join_clause.is_none() {
                self.orphan_join_clause = Some("USING");
            }
        }
        let mut joins = Vec::new();

        loop {
            let (natural, join_type) = if self.eat(TokenKind::Join) || self.eat(TokenKind::Comma) {
                (false, Some(JoinType::Inner))
            } else if matches!(
                self.peek().kind,
                TokenKind::Natural
                    | TokenKind::Left
                    | TokenKind::Right
                    | TokenKind::Full
                    | TokenKind::Outer
                    | TokenKind::Inner
                    | TokenKind::Cross
            ) {
                let n_kw = if self.peek_n(1).kind == TokenKind::Join {
                    1
                } else if self.peek_n(2).kind == TokenKind::Join {
                    2
                } else if self.peek_n(3).kind == TokenKind::Join {
                    3
                } else {
                    0
                };
                if n_kw == 0 {
                    self.advance();
                    let bad = self.peek();
                    return Err(
                        self.syntax_err_at(bad, format!("near \"{}\": syntax error", bad.text))
                    );
                }
                const JT_INNER: u8 = 0x01;
                const JT_CROSS: u8 = 0x02;
                const JT_NATURAL: u8 = 0x04;
                const JT_LEFT: u8 = 0x08;
                const JT_RIGHT: u8 = 0x10;
                const JT_OUTER: u8 = 0x20;
                const JT_ERROR: u8 = 0x80;
                let mut mask: u8 = 0;
                let mut words = Vec::with_capacity(n_kw);
                for _ in 0..n_kw {
                    let wt = self.advance();
                    words.push(wt.text);
                    if wt.text.eq_ignore_ascii_case("natural") {
                        mask |= JT_NATURAL;
                    } else if wt.text.eq_ignore_ascii_case("left") {
                        mask |= JT_LEFT | JT_OUTER;
                    } else if wt.text.eq_ignore_ascii_case("outer") {
                        mask |= JT_OUTER;
                    } else if wt.text.eq_ignore_ascii_case("right") {
                        mask |= JT_RIGHT | JT_OUTER;
                    } else if wt.text.eq_ignore_ascii_case("full") {
                        mask |= JT_LEFT | JT_RIGHT | JT_OUTER;
                    } else if wt.text.eq_ignore_ascii_case("inner") {
                        mask |= JT_INNER;
                    } else if wt.text.eq_ignore_ascii_case("cross") {
                        mask |= JT_INNER | JT_CROSS;
                    } else {
                        mask |= JT_ERROR;
                    }
                }
                self.expect(TokenKind::Join)?;
                if (mask & (JT_INNER | JT_OUTER)) == (JT_INNER | JT_OUTER)
                    || (mask & JT_ERROR) != 0
                    || (mask & (JT_OUTER | JT_LEFT | JT_RIGHT)) == JT_OUTER
                {
                    return Err(SqliteError::new(
                        sqliters_types::ErrorCode::Error,
                        format!("unknown join type: {}", words.join(" ")),
                    ));
                }
                let is_nat = (mask & JT_NATURAL) != 0;
                let jt = if (mask & (JT_LEFT | JT_RIGHT)) == (JT_LEFT | JT_RIGHT) {
                    JoinType::Full
                } else if (mask & JT_LEFT) != 0 {
                    JoinType::Left
                } else if (mask & JT_RIGHT) != 0 {
                    JoinType::Right
                } else if (mask & JT_CROSS) != 0 && !is_nat {
                    JoinType::Cross
                } else {
                    JoinType::Inner
                };
                (is_nat, Some(jt))
            } else {
                (false, None)
            };

            let Some(jt) = join_type else {
                break;
            };

            let right = self.parse_table_or_subquery()?;
            let constraint = if self.eat(TokenKind::On) {
                Some(JoinConstraint::On(self.parse_expr()?))
            } else if self.eat(TokenKind::Using) {
                self.expect(TokenKind::Lp)?;
                let mut cols = Vec::new();
                loop {
                    cols.push(self.parse_ident()?);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Rp)?;
                Some(JoinConstraint::Using(cols))
            } else {
                None
            };

            joins.push(JoinClause {
                join_type: jt,
                natural,
                right,
                constraint,
            });
        }

        Ok(FromClause { head, joins })
    }

    fn parse_table_or_subquery(&mut self) -> Result<TableOrSubquery> {
        if self.eat(TokenKind::Lp) {
            if matches!(
                self.peek().kind,
                TokenKind::Select | TokenKind::Values | TokenKind::With
            ) {
                let subq = self.parse_select_stmt()?;
                self.expect(TokenKind::Rp)?;
                let alias = self.parse_optional_table_alias(&[])?;
                return Ok(TableOrSubquery::Subquery {
                    query: Box::new(subq),
                    alias,
                });
            }
            let inner = self.parse_table_or_subquery()?;
            self.expect(TokenKind::Rp)?;
            return Ok(inner);
        }

        let name = self.parse_qualified_name()?;
        if self.eat(TokenKind::Lp) {
            let mut args = Vec::new();
            if !self.at(TokenKind::Rp) {
                loop {
                    args.push(self.parse_expr()?);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::Rp)?;
            let alias = self.parse_optional_table_alias(&[])?;
            return Ok(TableOrSubquery::TableFunction {
                schema: name.schema,
                name: name.name,
                args,
                alias,
            });
        }
        let alias = self.parse_optional_table_alias(&[])?;
        let indexed_by = self.parse_optional_index_hint()?;
        Ok(TableOrSubquery::Table {
            name,
            table_args: None,
            alias,
            indexed_by,
        })
    }

    fn parse_optional_index_hint(&mut self) -> Result<Option<IndexHint>> {
        if self.eat(TokenKind::Indexed) {
            self.expect(TokenKind::By)?;
            Ok(Some(IndexHint::IndexedBy(self.parse_ident()?)))
        } else if self.at(TokenKind::Not) && self.peek_n(1).kind == TokenKind::Indexed {
            self.advance();
            self.advance();
            Ok(Some(IndexHint::NotIndexed))
        } else {
            Ok(None)
        }
    }

    fn parse_optional_table_alias(&mut self, stop_tokens: &[TokenKind]) -> Result<Option<String>> {
        if self.eat(TokenKind::As) {
            return Ok(Some(self.parse_nm()?));
        }
        let next = self.peek().kind;
        if stop_tokens.contains(&next) {
            return Ok(None);
        }
        if matches!(next, TokenKind::Id | TokenKind::QuotedId)
            || Self::is_soft_alias_kw(next)
            || (stop_tokens.is_empty() && next == TokenKind::StringLiteral)
        {
            let tok = self.advance();
            return Ok(Some(self.alloc_ident_from_token(tok)));
        }
        Ok(None)
    }

    fn parse_optional_order_by(&mut self) -> Result<Vec<OrderByTerm>> {
        if !self.eat(TokenKind::Order) {
            return Ok(Vec::new());
        }
        self.expect(TokenKind::By)?;
        let mut terms = Vec::new();
        loop {
            let expr = self.parse_expr()?;
            let mut desc = false;
            if self.eat(TokenKind::Asc) {
                desc = false;
            } else if self.eat(TokenKind::Desc) {
                desc = true;
            }
            let nulls_last = if self.eat(TokenKind::Nulls) {
                if self.eat(TokenKind::First) {
                    Some(false)
                } else {
                    self.expect(TokenKind::Last)?;
                    Some(true)
                }
            } else {
                None
            };
            terms.push(OrderByTerm {
                expr,
                desc,
                nulls_last,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        Ok(terms)
    }

    fn parse_optional_limit(&mut self) -> Result<Option<LimitClause>> {
        if !self.eat(TokenKind::Limit) {
            return Ok(None);
        }
        let first = self.parse_expr()?;
        if self.eat(TokenKind::Offset) {
            let offset = self.parse_expr()?;
            Ok(Some(LimitClause {
                limit: first,
                offset: Some(offset),
            }))
        } else if self.eat(TokenKind::Comma) {
            // SQLite legacy `LIMIT offset, count` syntax!
            let count = self.parse_expr()?;
            Ok(Some(LimitClause {
                limit: count,
                offset: Some(first),
            }))
        } else {
            Ok(Some(LimitClause {
                limit: first,
                offset: None,
            }))
        }
    }

    // ========================================================================
    // Pratt Expression Parser (`sqlite/src/parse.y` operator precedences)
    // ========================================================================

    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_expr_bp(0)
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr> {
        self.expr_depth += 1;
        if self.expr_depth > SQLITE_MAX_EXPR_DEPTH {
            self.expr_depth -= 1;
            return Err(SqliteError::new(
                sqliters_types::ErrorCode::Error,
                format!("Expression tree is too large (maximum depth {SQLITE_MAX_EXPR_DEPTH})"),
            ));
        }
        let res = self.parse_expr_bp_inner(min_bp);
        self.expr_depth -= 1;
        res
    }

    fn parse_expr_bp_inner(&mut self, min_bp: u8) -> Result<Expr> {
        let mut lhs = self.parse_prefix_expr()?;

        loop {
            let tok = self.peek();

            // Postfix COLLATE
            if tok.kind == TokenKind::Collate && 19 >= min_bp {
                self.advance();
                let collation = self.parse_ident()?;
                lhs = Expr::Collate {
                    expr: Box::new(lhs),
                    collation,
                };
                continue;
            }

            // Postfix ISNULL / NOTNULL / two-token `NOT NULL`
            if (tok.kind == TokenKind::IsNull || tok.kind == TokenKind::NotNull) && 7 >= min_bp {
                self.advance();
                lhs = Expr::IsNull {
                    expr: Box::new(lhs),
                    negated: tok.kind == TokenKind::NotNull,
                };
                continue;
            }
            if tok.kind == TokenKind::Not && self.peek_n(1).kind == TokenKind::Null && 7 >= min_bp {
                self.advance();
                self.advance();
                lhs = Expr::IsNull {
                    expr: Box::new(lhs),
                    negated: true,
                };
                continue;
            }

            // IS [NOT] NULL / IS [NOT] DISTINCT FROM / IS [NOT] rhs
            if tok.kind == TokenKind::Is && 7 >= min_bp {
                self.advance();
                let not = self.eat(TokenKind::Not);
                if self.eat(TokenKind::Null) {
                    lhs = Expr::IsNull {
                        expr: Box::new(lhs),
                        negated: not,
                    };
                    continue;
                }
                if self.eat(TokenKind::Distinct) {
                    self.expect(TokenKind::From)?;
                    let rhs = self.parse_expr_bp(8)?;
                    lhs = Expr::Binary {
                        left: Box::new(lhs),
                        op: if not { BinaryOp::Eq } else { BinaryOp::Ne },
                        // Wait: IS DISTINCT FROM is `IS NOT`, IS NOT DISTINCT FROM is `IS`!
                        right: Box::new(rhs),
                    };
                    if let Expr::Binary { ref mut op, .. } = lhs {
                        *op = if not { BinaryOp::Is } else { BinaryOp::IsNot };
                    }
                    continue;
                }
                let rhs = self.parse_expr_bp(8)?;
                lhs = Expr::Binary {
                    left: Box::new(lhs),
                    op: if not { BinaryOp::IsNot } else { BinaryOp::Is },
                    right: Box::new(rhs),
                };
                continue;
            }

            // [NOT] BETWEEN / [NOT] IN / [NOT] LIKE / [NOT] GLOB / [NOT] REGEXP / [NOT] MATCH
            if (tok.kind == TokenKind::Not
                && matches!(
                    self.peek_n(1).kind,
                    TokenKind::Between
                        | TokenKind::In
                        | TokenKind::Like
                        | TokenKind::Glob
                        | TokenKind::Regexp
                        | TokenKind::Match
                )
                || matches!(
                    tok.kind,
                    TokenKind::Between
                        | TokenKind::In
                        | TokenKind::Like
                        | TokenKind::Glob
                        | TokenKind::Regexp
                        | TokenKind::Match
                ))
                && 7 >= min_bp
            {
                let negated = self.eat(TokenKind::Not);
                let kw = self.advance();
                match kw.kind {
                    TokenKind::Between => {
                        // Parse bounds above AND (bp 5) so `AND` separates low and high!
                        let low = self.parse_expr_bp(8)?;
                        self.expect(TokenKind::And)?;
                        let high = self.parse_expr_bp(8)?;
                        lhs = Expr::Between {
                            expr: Box::new(lhs),
                            low: Box::new(low),
                            high: Box::new(high),
                            negated,
                        };
                    }
                    TokenKind::In => {
                        if self.eat(TokenKind::Lp) {
                            if self.eat(TokenKind::Rp) {
                                lhs = Expr::InList {
                                    expr: Box::new(lhs),
                                    list: Vec::new(),
                                    negated,
                                };
                            } else if matches!(
                                self.peek().kind,
                                TokenKind::Select | TokenKind::Values | TokenKind::With
                            ) {
                                let subq = self.parse_select_stmt()?;
                                self.expect(TokenKind::Rp)?;
                                lhs = Expr::InSubquery {
                                    expr: Box::new(lhs),
                                    subquery: Box::new(subq),
                                    negated,
                                };
                            } else {
                                let mut list = Vec::new();
                                loop {
                                    list.push(self.parse_expr()?);
                                    if !self.eat(TokenKind::Comma) {
                                        break;
                                    }
                                }
                                self.expect(TokenKind::Rp)?;
                                lhs = Expr::InList {
                                    expr: Box::new(lhs),
                                    list,
                                    negated,
                                };
                            }
                        } else {
                            // SQLite `expr [NOT] IN [schema.]table_name`
                            let table = self.parse_qualified_name()?;
                            lhs = Expr::InTable {
                                expr: Box::new(lhs),
                                table,
                                negated,
                            };
                        }
                    }
                    TokenKind::Like | TokenKind::Glob | TokenKind::Regexp | TokenKind::Match => {
                        let pattern = self.parse_expr_bp(8)?;
                        let escape = if self.eat(TokenKind::Escape) {
                            Some(Box::new(self.parse_expr_bp(8)?))
                        } else {
                            None
                        };
                        let op = match kw.kind {
                            TokenKind::Glob => LikeOp::Glob,
                            TokenKind::Regexp => LikeOp::Regexp,
                            TokenKind::Match => LikeOp::Match,
                            _ => LikeOp::Like,
                        };
                        lhs = Expr::Like {
                            expr: Box::new(lhs),
                            pattern: Box::new(pattern),
                            escape,
                            op,
                            negated,
                        };
                    }
                    _ => unreachable!(),
                }
                continue;
            }

            // Standard binary operators
            let Some((op, l_bp, r_bp)) = Self::infix_binding_power(tok.kind) else {
                break;
            };
            if l_bp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_expr_bp(r_bp)?;
            lhs = Expr::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            };
        }

        Ok(lhs)
    }

    const fn infix_binding_power(kind: TokenKind) -> Option<(BinaryOp, u8, u8)> {
        match kind {
            TokenKind::Or => Some((BinaryOp::Or, 1, 2)),
            TokenKind::And => Some((BinaryOp::And, 3, 4)),
            TokenKind::Eq => Some((BinaryOp::Eq, 7, 8)),
            TokenKind::Ne => Some((BinaryOp::Ne, 7, 8)),
            TokenKind::Lt => Some((BinaryOp::Lt, 9, 10)),
            TokenKind::Le => Some((BinaryOp::Le, 9, 10)),
            TokenKind::Gt => Some((BinaryOp::Gt, 9, 10)),
            TokenKind::Ge => Some((BinaryOp::Ge, 9, 10)),
            TokenKind::BitAnd => Some((BinaryOp::BitAnd, 11, 12)),
            TokenKind::BitOr => Some((BinaryOp::BitOr, 11, 12)),
            TokenKind::LShift => Some((BinaryOp::ShiftLeft, 11, 12)),
            TokenKind::RShift => Some((BinaryOp::ShiftRight, 11, 12)),
            TokenKind::Plus => Some((BinaryOp::Add, 13, 14)),
            TokenKind::Minus => Some((BinaryOp::Subtract, 13, 14)),
            TokenKind::Star => Some((BinaryOp::Multiply, 15, 16)),
            TokenKind::Slash => Some((BinaryOp::Divide, 15, 16)),
            TokenKind::Rem => Some((BinaryOp::Remainder, 15, 16)),
            TokenKind::Concat => Some((BinaryOp::Concat, 17, 18)),
            TokenKind::Arrow => Some((BinaryOp::JsonExtract, 17, 18)),
            TokenKind::DoubleArrow => Some((BinaryOp::JsonExtractText, 17, 18)),
            _ => None,
        }
    }

    fn parse_prefix_expr(&mut self) -> Result<Expr> {
        let tok = self.peek();
        match tok.kind {
            TokenKind::Not => {
                self.advance();
                if self.eat(TokenKind::Exists) {
                    self.expect(TokenKind::Lp)?;
                    let subq = self.parse_select_stmt()?;
                    self.expect(TokenKind::Rp)?;
                    return Ok(Expr::Exists {
                        subquery: Box::new(subq),
                        negated: true,
                    });
                }
                let e = self.parse_expr_bp(5)?;
                Ok(Expr::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(e),
                })
            }
            TokenKind::Minus => {
                self.advance();
                let next_tok = self.peek();
                let next_is_hex = next_tok.kind == TokenKind::Integer
                    && (next_tok.text.starts_with("0x") || next_tok.text.starts_with("0X"));
                let rhs = self.parse_expr_bp(20)?;
                // Fold `-9223372036854775808` and negative integer literals cleanly!
                if let Expr::Float(f) = rhs {
                    return Ok(Expr::Float(-f));
                }
                if let Expr::OversizedHex(h) = rhs {
                    let neg_h = if let Some(stripped) = h.strip_prefix('-') {
                        stripped.to_owned()
                    } else {
                        format!("-{h}")
                    };
                    if self.in_trigger_body {
                        return Ok(Expr::OversizedHex(neg_h));
                    }
                    return Err(SqliteError::new(
                        sqliters_types::ErrorCode::Error,
                        format!("hex literal too big: {neg_h}"),
                    ));
                }
                if let Expr::Integer(i) = rhs {
                    if i == i64::MIN && next_is_hex {
                        let neg_h = format!("-{}", next_tok.text);
                        if self.in_trigger_body {
                            return Ok(Expr::OversizedHex(neg_h));
                        }
                        return Err(SqliteError::new(
                            sqliters_types::ErrorCode::Error,
                            format!("hex literal too big: {neg_h}"),
                        ));
                    }
                    if let Some(neg) = i.checked_neg() {
                        return Ok(Expr::Integer(neg));
                    }
                }
                Ok(Expr::Unary {
                    op: UnaryOp::Negate,
                    expr: Box::new(rhs),
                })
            }
            TokenKind::Plus => {
                self.advance();
                let rhs = self.parse_expr_bp(20)?;
                Ok(Expr::Unary {
                    op: UnaryOp::Plus,
                    expr: Box::new(rhs),
                })
            }
            TokenKind::BitNot => {
                self.advance();
                let rhs = self.parse_expr_bp(20)?;
                Ok(Expr::Unary {
                    op: UnaryOp::BitNot,
                    expr: Box::new(rhs),
                })
            }
            _ => {
                // Special case `-9223372036854775808` where positive token became Float(9223372036854775808.0)
                self.parse_primary_expr()
            }
        }
    }

    fn parse_primary_expr(&mut self) -> Result<Expr> {
        let tok = self.peek();
        match tok.kind {
            TokenKind::Null => {
                self.advance();
                Ok(Expr::Null)
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            TokenKind::CurrentDate => {
                self.advance();
                Ok(Expr::CurrentTimeKw(CurrentTimeKind::Date))
            }
            TokenKind::CurrentTime => {
                self.advance();
                Ok(Expr::CurrentTimeKw(CurrentTimeKind::Time))
            }
            TokenKind::CurrentTimestamp => {
                self.advance();
                Ok(Expr::CurrentTimeKw(CurrentTimeKind::Timestamp))
            }
            TokenKind::Integer => {
                self.advance();
                match parse_int_literal(tok.text) {
                    Ok(Some(i)) => Ok(Expr::Integer(i)),
                    Ok(None) => Ok(Expr::Float(parse_float_literal(tok.text)?)),
                    Err(e)
                        if self.in_trigger_body
                            && (tok.text.starts_with("0x") || tok.text.starts_with("0X")) =>
                    {
                        let _ = e;
                        Ok(Expr::OversizedHex(tok.text.to_owned()))
                    }
                    Err(e) => Err(e),
                }
            }
            TokenKind::Float => {
                self.advance();
                Ok(Expr::Float(parse_float_literal(tok.text)?))
            }
            TokenKind::StringLiteral => {
                self.advance();
                Ok(Expr::String(dequote(tok.text)))
            }
            TokenKind::BlobLiteral => {
                self.advance();
                Ok(Expr::Blob(decode_blob_literal(tok.text)?))
            }
            TokenKind::Variable => {
                if tok.text.starts_with('#')
                    && tok.text.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                {
                    return Err(
                        self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text))
                    );
                }
                self.advance();
                let param = self.resolve_bind_param(tok)?;
                Ok(Expr::Bind(param))
            }
            TokenKind::Exists => {
                self.advance();
                self.expect(TokenKind::Lp)?;
                let subq = self.parse_select_stmt()?;
                self.expect(TokenKind::Rp)?;
                Ok(Expr::Exists {
                    subquery: Box::new(subq),
                    negated: false,
                })
            }
            TokenKind::Raise if self.peek_n(1).kind == TokenKind::Lp => {
                self.advance();
                self.expect(TokenKind::Lp)?;
                let action_tok = self.peek();
                match action_tok.kind {
                    TokenKind::Ignore => {
                        self.advance();
                        self.expect(TokenKind::Rp)?;
                        Ok(Expr::Raise {
                            action: RaiseAction::Ignore,
                            message: None,
                        })
                    }
                    TokenKind::Rollback | TokenKind::Abort | TokenKind::Fail => {
                        self.advance();
                        let action = match action_tok.kind {
                            TokenKind::Rollback => RaiseAction::Rollback,
                            TokenKind::Abort => RaiseAction::Abort,
                            TokenKind::Fail => RaiseAction::Fail,
                            _ => unreachable!(),
                        };
                        self.expect(TokenKind::Comma)?;
                        let msg = self.parse_expr()?;
                        self.expect(TokenKind::Rp)?;
                        Ok(Expr::Raise {
                            action,
                            message: Some(Box::new(msg)),
                        })
                    }
                    _ => Err(self.syntax_err_at(
                        action_tok,
                        format!("near \"{}\": syntax error", action_tok.text),
                    )),
                }
            }
            TokenKind::Cast => {
                self.advance();
                self.expect(TokenKind::Lp)?;
                let expr = self.parse_expr()?;
                self.expect(TokenKind::As)?;
                let mut type_parts = Vec::new();
                while !self.at(TokenKind::Rp) && !self.at(TokenKind::Eof) {
                    if self.eat(TokenKind::Lp) {
                        while !self.at(TokenKind::Rp) && !self.at(TokenKind::Eof) {
                            self.advance();
                        }
                        self.expect(TokenKind::Rp)?;
                    } else {
                        type_parts.push(self.advance().text.to_owned());
                    }
                }
                self.expect(TokenKind::Rp)?;
                Ok(Expr::Cast {
                    expr: Box::new(expr),
                    type_name: type_parts.join(" "),
                })
            }
            TokenKind::Case => {
                self.advance();
                let operand = if !self.at(TokenKind::When) {
                    Some(Box::new(self.parse_expr()?))
                } else {
                    None
                };
                let mut branches = Vec::new();
                while self.eat(TokenKind::When) {
                    let cond = self.parse_expr()?;
                    self.expect(TokenKind::Then)?;
                    let val = self.parse_expr()?;
                    branches.push((cond, val));
                }
                if branches.is_empty() {
                    return Err(self.syntax_err_at(tok, "CASE requires at least one WHEN branch"));
                }
                let else_expr = if self.eat(TokenKind::Else) {
                    Some(Box::new(self.parse_expr()?))
                } else {
                    None
                };
                self.expect(TokenKind::End)?;
                Ok(Expr::Case {
                    operand,
                    branches,
                    else_expr,
                })
            }
            TokenKind::Lp => {
                self.advance();
                if matches!(
                    self.peek().kind,
                    TokenKind::Select | TokenKind::Values | TokenKind::With
                ) {
                    let subq = self.parse_select_stmt()?;
                    self.expect(TokenKind::Rp)?;
                    return Ok(Expr::Subquery(Box::new(subq)));
                }
                let first = self.parse_expr()?;
                if self.eat(TokenKind::Comma) {
                    let mut elems = vec![first];
                    loop {
                        elems.push(self.parse_expr()?);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::Rp)?;
                    return Ok(Expr::RowValue(elems));
                }
                self.expect(TokenKind::Rp)?;
                Ok(first)
            }
            k if Self::can_be_ident(k) || Self::is_join_kw(k) => {
                let first = self.parse_nm()?;
                // Function call: `name(...)`
                if self.eat(TokenKind::Lp) {
                    let mut distinct = false;
                    let args = if self.eat(TokenKind::Rp) {
                        FunctionArgs::Exprs(Vec::new())
                    } else if self.eat(TokenKind::Star) {
                        self.expect(TokenKind::Rp)?;
                        FunctionArgs::Star
                    } else {
                        if self.eat(TokenKind::Distinct) {
                            distinct = true;
                            if self.eat(TokenKind::Rp) {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    "DISTINCT aggregates must have exactly one argument",
                                ));
                            }
                        } else {
                            let _ = self.eat(TokenKind::All);
                        }
                        let mut exprs = Vec::new();
                        loop {
                            exprs.push(self.parse_expr()?);
                            if !self.eat(TokenKind::Comma) {
                                break;
                            }
                        }
                        self.expect(TokenKind::Rp)?;
                        FunctionArgs::Exprs(exprs)
                    };
                    let filter = if self.eat(TokenKind::Filter) {
                        self.expect(TokenKind::Lp)?;
                        self.expect(TokenKind::Where)?;
                        let f = self.parse_expr()?;
                        self.expect(TokenKind::Rp)?;
                        Some(Box::new(f))
                    } else {
                        None
                    };
                    let over = if self.eat(TokenKind::Over) {
                        if self.eat(TokenKind::Lp) {
                            let spec = self.parse_window_spec()?;
                            self.expect(TokenKind::Rp)?;
                            Some(WindowRef::Spec(Box::new(spec)))
                        } else {
                            Some(WindowRef::Name(self.parse_nm()?))
                        }
                    } else {
                        None
                    };
                    if distinct && over.is_some() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "DISTINCT is not supported for window functions",
                        ));
                    }
                    return Ok(Expr::FunctionCall {
                        name: first,
                        distinct,
                        args,
                        filter,
                        over,
                    });
                }

                // Qualified column `table.col` or `schema.table.col`
                if self.eat(TokenKind::Dot) {
                    let second = self.parse_nm()?;
                    if self.eat(TokenKind::Dot) {
                        let third = self.parse_nm()?;
                        return Ok(Expr::Column(ColumnRef::schema_table_qualified(
                            first, second, third,
                        )));
                    }
                    return Ok(Expr::Column(ColumnRef::table_qualified(first, second)));
                }

                Ok(Expr::Column(ColumnRef::unqualified(first)))
            }
            _ => Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text))),
        }
    }

    fn resolve_bind_param(&mut self, tok: Token<'a>) -> Result<BindParam> {
        let txt = tok.text;
        if txt == "?" {
            self.max_param_idx += 1;
            return Ok(BindParam::Anonymous(self.max_param_idx));
        }
        if let Some(digits) = txt.strip_prefix('?') {
            let idx = digits
                .parse::<usize>()
                .map_err(|_| self.syntax_err_at(tok, "variable number out of range"))?;
            if idx == 0 {
                return Err(self.syntax_err_at(tok, "?0 is not a valid SQLite parameter"));
            }
            if idx > self.max_param_idx {
                self.max_param_idx = idx;
            }
            return Ok(BindParam::Numbered(idx));
        }
        if let Some(&existing_idx) = self.named_params.get(txt) {
            return Ok(BindParam::Named(txt.to_owned(), existing_idx));
        }
        self.max_param_idx += 1;
        let idx = self.max_param_idx;
        self.named_params.insert(txt.to_owned(), idx);
        Ok(BindParam::Named(txt.to_owned(), idx))
    }
}

/// Splits raw module arguments inside `CREATE VIRTUAL TABLE ... USING module(...)`
/// at top-level commas while preserving balanced parentheses, brackets, braces,
/// quoted strings/identifiers, and comments.
pub fn split_vtab_raw_args(inner_sql: &str) -> Vec<String> {
    if inner_sql.trim().is_empty() {
        return Vec::new();
    }
    let bytes = inner_sql.as_bytes();
    let mut args = Vec::new();
    let mut seg_start = 0usize;
    let mut i = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' | b'"' | b'`' => {
                let quote = b;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                if i + 1 < bytes.len() {
                    i += 2;
                }
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'[' => {
                bracket_depth += 1;
                i += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                i += 1;
            }
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                i += 1;
            }
            b',' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                args.push(inner_sql[seg_start..i].trim().to_owned());
                i += 1;
                seg_start = i;
            }
            _ => {
                i += 1;
            }
        }
    }

    args.push(inner_sql[seg_start..].trim().to_owned());
    args
}
