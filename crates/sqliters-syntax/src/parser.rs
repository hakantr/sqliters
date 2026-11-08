//! Recursive-descent SQL statement parser and Pratt precedence-climbing expression parser
//! modeled after `sqlite/src/parse.y`.

use std::collections::HashMap;

use sqliters_types::{ErrorCode, Result, SQLITE_MAX_EXPR_DEPTH, SqliteError};

use crate::ast::*;
use crate::token::{
    Token, TokenKind, decode_blob_literal, dequote, parse_float_literal, parse_int_literal,
    tokenize,
};

/// Parses a single SQL statement from the beginning of `sql`, returning `(Some(stmt), remaining_tail)`
/// or `(None, "")` if `sql` contains only whitespace, comments, and/or empty semicolons.
pub fn parse_one(sql: &str) -> Result<(Option<Stmt>, &str)> {
    let tokens = tokenize(sql)?;
    let mut parser = Parser::new(sql, &tokens);
    while parser.eat(TokenKind::Semi) {}
    if parser.at(TokenKind::Eof) {
        return Ok((None, ""));
    }
    let stmt = parser.parse_stmt()?;
    let _ = parser.eat(TokenKind::Semi);
    let tail_off = parser.peek().offset;
    Ok((Some(stmt), &sql[tail_off..]))
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
}

impl<'a> Parser<'a> {
    fn new(sql: &'a str, tokens: &'a [Token<'a>]) -> Self {
        Self {
            sql,
            tokens,
            pos: 0,
            expr_depth: 0,
            max_param_idx: 0,
            named_params: HashMap::new(),
        }
    }

    #[inline]
    fn peek(&self) -> Token<'a> {
        self.tokens
            .get(self.pos)
            .copied()
            .unwrap_or(Token {
                kind: TokenKind::Eof,
                text: "",
                offset: self.sql.len(),
            })
    }

    #[inline]
    fn peek_n(&self, n: usize) -> Token<'a> {
        self.tokens
            .get(self.pos + n)
            .copied()
            .unwrap_or(Token {
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
        SqliteError::syntax(msg, tok.offset)
    }

    /// Parses an identifier, allowing SQLite fallback keywords (`%fallback ID` in `parse.y`).
    fn parse_ident(&mut self) -> Result<String> {
        let tok = self.peek();
        if Self::can_be_ident(tok.kind) {
            self.advance();
            Ok(dequote(tok.text))
        } else {
            Err(self.syntax_err_at(tok, format!("near \"{}\": syntax error", tok.text)))
        }
    }

    /// Parses `[schema.]object_name`, returning `(Option<schema>, object_name)`.
    fn parse_qualified_name(&mut self) -> Result<(Option<String>, String)> {
        let first = self.parse_ident()?;
        if self.eat(TokenKind::Dot) {
            let second = self.parse_ident()?;
            Ok((Some(first), second))
        } else {
            Ok((None, first))
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
                | TokenKind::Database
                | TokenKind::Deferred
                | TokenKind::Desc
                | TokenKind::Detach
                | TokenKind::Do
                | TokenKind::Each
                | TokenKind::End
                | TokenKind::Exclusive
                | TokenKind::Explain
                | TokenKind::Fail
                | TokenKind::Filter
                | TokenKind::First
                | TokenKind::Generated
                | TokenKind::Glob
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
                | TokenKind::No
                | TokenKind::Nothing
                | TokenKind::Nulls
                | TokenKind::Of
                | TokenKind::Offset
                | TokenKind::Over
                | TokenKind::Partition
                | TokenKind::Plan
                | TokenKind::Pragma
                | TokenKind::Query
                | TokenKind::Raise
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
                | TokenKind::Trigger
                | TokenKind::Vacuum
                | TokenKind::View
                | TokenKind::Virtual
                | TokenKind::Window
                | TokenKind::Without
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
                    Some(self.parse_ident()?)
                } else {
                    None
                };
                Ok(Stmt::Rollback { savepoint })
            }
            TokenKind::Savepoint => {
                self.advance();
                Ok(Stmt::Savepoint(self.parse_ident()?))
            }
            TokenKind::Release => {
                self.advance();
                let _ = self.eat(TokenKind::Savepoint);
                Ok(Stmt::Release(self.parse_ident()?))
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
                if Self::can_be_ident(self.peek().kind) {
                    let _ = self.parse_ident()?;
                }
                Ok(Stmt::Vacuum)
            }
            TokenKind::Analyze => {
                self.advance();
                let target = if !self.at(TokenKind::Semi) && !self.at(TokenKind::Eof) {
                    let (_, name) = self.parse_qualified_name()?;
                    Some(name)
                } else {
                    None
                };
                Ok(Stmt::Analyze(target))
            }
            TokenKind::Reindex => {
                self.advance();
                let target = if !self.at(TokenKind::Semi) && !self.at(TokenKind::Eof) {
                    let (_, name) = self.parse_qualified_name()?;
                    Some(name)
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

        if !unique && self.eat(TokenKind::Table) {
            let if_not_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Not)?;
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let (_schema, name) = self.parse_qualified_name()?;
            if self.eat(TokenKind::As) {
                if temp {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "TEMP tables are not yet supported",
                    ));
                }
                let query = self.parse_select_stmt()?;
                return Ok(Stmt::CreateTableAs {
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

            let end_off = self.peek().offset;
            let raw_sql = self.sql[stmt_start..end_off].trim().to_owned();
            return Ok(Stmt::CreateTable(CreateTableStmt {
                temp,
                if_not_exists,
                name,
                columns,
                constraints,
                without_rowid,
                strict,
                raw_sql,
            }));
        }

        if self.eat(TokenKind::Index) {
            let if_not_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Not)?;
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let (_schema, name) = self.parse_qualified_name()?;
            self.expect(TokenKind::On)?;
            let table_name = self.parse_ident()?;
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
            let end_off = self.peek().offset;
            let raw_sql = self.sql[stmt_start..end_off].trim().to_owned();
            return Ok(Stmt::CreateIndex(CreateIndexStmt {
                unique,
                if_not_exists,
                name,
                table_name,
                columns,
                where_clause,
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
        let name = self.parse_ident()?;
        let mut type_parts = Vec::new();

        // Read zero or more type words plus optional `(N)` or `(N, M)` precision
        while !self.at(TokenKind::Comma)
            && !self.at(TokenKind::Rp)
            && !self.is_column_constraint_start()
            && !self.at(TokenKind::Eof)
            && !self.at(TokenKind::Semi)
        {
            if self.eat(TokenKind::Lp) {
                let mut inner = String::from("(");
                while !self.at(TokenKind::Rp) && !self.at(TokenKind::Eof) {
                    let t = self.advance();
                    if inner.len() > 1 && t.kind != TokenKind::Comma {
                        inner.push(' ');
                    }
                    inner.push_str(t.text);
                }
                self.expect(TokenKind::Rp)?;
                inner.push(')');
                type_parts.push(inner);
                break;
            } else if Self::can_be_ident(self.peek().kind) || self.at(TokenKind::Null) {
                if self.at(TokenKind::Null) {
                    break;
                }
                type_parts.push(self.advance().text.to_owned());
            } else {
                break;
            }
        }

        let type_name = type_parts.join(" ");
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
            Expr::Column {
                table: None,
                column,
            } => (column, None),
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
            let (_schema, name) = self.parse_qualified_name()?;
            Ok(Stmt::DropTable { if_exists, name })
        } else if self.eat(TokenKind::Index) {
            let if_exists = if self.eat(TokenKind::If) {
                self.expect(TokenKind::Exists)?;
                true
            } else {
                false
            };
            let (_schema, name) = self.parse_qualified_name()?;
            Ok(Stmt::DropIndex { if_exists, name })
        } else {
            let tok = self.peek();
            Err(self.syntax_err_at(tok, "expected TABLE or INDEX after DROP"))
        }
    }

    fn parse_alter(&mut self) -> Result<Stmt> {
        self.expect(TokenKind::Alter)?;
        self.expect(TokenKind::Table)?;
        let (_schema, table) = self.parse_qualified_name()?;
        if self.eat(TokenKind::Rename) {
            if self.eat(TokenKind::To) {
                let new_name = self.parse_ident()?;
                Ok(Stmt::AlterTable {
                    table,
                    action: AlterTableAction::RenameTo(new_name),
                })
            } else {
                let _ = self.eat(TokenKind::ColumnKw);
                let old_name = self.parse_ident()?;
                self.expect(TokenKind::To)?;
                let new_name = self.parse_ident()?;
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
        } else {
            let tok = self.peek();
            Err(self.syntax_err_at(tok, "expected RENAME or ADD in ALTER TABLE"))
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
        let (_schema, table) = self.parse_qualified_name()?;
        if self.eat(TokenKind::As) {
            let _ = self.parse_ident()?;
        }

        let columns = if self.at(TokenKind::Lp) && self.peek_n(1).kind != TokenKind::Select {
            self.expect(TokenKind::Lp)?;
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

        let source = if self.eat(TokenKind::Default) {
            self.expect(TokenKind::Values)?;
            InsertSource::DefaultValues
        } else if self.at(TokenKind::Values)
            && self.peek_n(1).kind == TokenKind::Lp
        {
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

        let returning = self.parse_optional_returning()?;
        Ok(Stmt::Insert(InsertStmt {
            ctes,
            or_conflict,
            table,
            columns,
            source,
            returning,
        }))
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
        let (_schema, table) = self.parse_qualified_name()?;
        let alias = self.parse_optional_table_alias(&[TokenKind::Set])?;
        let indexed_by = self.parse_optional_index_hint()?;
        self.expect(TokenKind::Set)?;

        let mut assignments = Vec::new();
        loop {
            let col = self.parse_ident()?;
            self.expect(TokenKind::Eq)?;
            let val = self.parse_expr()?;
            assignments.push((col, val));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }

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
            selection,
            returning,
        })
    }

    fn parse_delete(&mut self, ctes: Vec<CommonTableExpr>) -> Result<DeleteStmt> {
        self.expect(TokenKind::Delete)?;
        self.expect(TokenKind::From)?;
        let (_schema, table) = self.parse_qualified_name()?;
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
        let (schema, name) = self.parse_qualified_name()?;
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
            schema,
            name,
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
            if self.at(TokenKind::Not) || Self::can_be_ident(self.peek().kind) {
                // Optional MATERIALIZED / NOT MATERIALIZED
                let _ = self.eat(TokenKind::Not);
                if self.peek().text.eq_ignore_ascii_case("MATERIALIZED") {
                    self.advance();
                }
            }
            self.expect(TokenKind::Lp)?;
            let query = self.parse_select_stmt()?;
            self.expect(TokenKind::Rp)?;
            ctes.push(CommonTableExpr {
                name,
                columns,
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

        Ok(SelectCore::Select {
            distinct,
            projection,
            from,
            selection,
            group_by,
            having,
        })
    }

    fn parse_projection(&mut self) -> Result<Vec<ResultColumn>> {
        let mut cols = Vec::new();
        loop {
            if self.eat(TokenKind::Star) {
                cols.push(ResultColumn::Star);
            } else if Self::can_be_ident(self.peek().kind)
                && self.peek_n(1).kind == TokenKind::Dot
                && self.peek_n(2).kind == TokenKind::Star
            {
                let tbl = self.parse_ident()?;
                self.expect(TokenKind::Dot)?;
                self.expect(TokenKind::Star)?;
                cols.push(ResultColumn::TableStar(tbl));
            } else {
                let expr = self.parse_expr()?;
                let alias = if self.eat(TokenKind::As) {
                    if self.at(TokenKind::StringLiteral) {
                        Some(dequote(self.advance().text))
                    } else {
                        Some(self.parse_ident()?)
                    }
                } else if self.at(TokenKind::Id) || self.at(TokenKind::QuotedId) {
                    // Implicit alias `SELECT expr alias` when not a clause keyword
                    Some(dequote(self.advance().text))
                } else {
                    None
                };
                cols.push(ResultColumn::Expr { expr, alias });
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        Ok(cols)
    }

    fn parse_from_clause(&mut self) -> Result<FromClause> {
        let head = self.parse_table_or_subquery()?;
        let mut joins = Vec::new();

        loop {
            if self.eat(TokenKind::Comma) {
                let right = self.parse_table_or_subquery()?;
                joins.push(JoinClause {
                    join_type: JoinType::Cross,
                    natural: false,
                    right,
                    constraint: None,
                });
                continue;
            }

            let natural = self.eat(TokenKind::Natural);
            let join_type = if self.eat(TokenKind::Left) {
                let _ = self.eat(TokenKind::Outer);
                self.expect(TokenKind::Join)?;
                Some(JoinType::Left)
            } else if self.eat(TokenKind::Right) {
                let _ = self.eat(TokenKind::Outer);
                self.expect(TokenKind::Join)?;
                Some(JoinType::Right)
            } else if self.eat(TokenKind::Full) {
                let _ = self.eat(TokenKind::Outer);
                self.expect(TokenKind::Join)?;
                Some(JoinType::Full)
            } else if self.eat(TokenKind::Inner) {
                self.expect(TokenKind::Join)?;
                Some(JoinType::Inner)
            } else if self.eat(TokenKind::Cross) {
                self.expect(TokenKind::Join)?;
                Some(JoinType::Cross)
            } else if self.eat(TokenKind::Join) {
                Some(JoinType::Inner)
            } else if natural {
                return Err(self.syntax_err_at(self.peek(), "expected JOIN after NATURAL"));
            } else {
                None
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

        let (_schema, name) = self.parse_qualified_name()?;
        let alias = self.parse_optional_table_alias(&[])?;
        let indexed_by = self.parse_optional_index_hint()?;
        Ok(TableOrSubquery::Table {
            name,
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
            return Ok(Some(self.parse_ident()?));
        }
        let next = self.peek().kind;
        if stop_tokens.contains(&next) {
            return Ok(None);
        }
        if matches!(next, TokenKind::Id | TokenKind::QuotedId) {
            return Ok(Some(dequote(self.advance().text)));
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

            // [NOT] BETWEEN / [NOT] IN / [NOT] LIKE / [NOT] GLOB
            if (tok.kind == TokenKind::Not
                && matches!(
                    self.peek_n(1).kind,
                    TokenKind::Between | TokenKind::In | TokenKind::Like | TokenKind::Glob
                )
                || matches!(
                    tok.kind,
                    TokenKind::Between | TokenKind::In | TokenKind::Like | TokenKind::Glob
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
                            let (_schema, tbl_name) = self.parse_qualified_name()?;
                            let subq = SelectStmt {
                                ctes: Vec::new(),
                                body: SelectBody {
                                    head: SelectCore::Select {
                                        distinct: false,
                                        projection: vec![ResultColumn::Star],
                                        from: Some(FromClause {
                                            head: TableOrSubquery::Table {
                                                name: tbl_name,
                                                alias: None,
                                                indexed_by: None,
                                            },
                                            joins: Vec::new(),
                                        }),
                                        selection: None,
                                        group_by: Vec::new(),
                                        having: None,
                                    },
                                    compounds: Vec::new(),
                                },
                                order_by: Vec::new(),
                                limit: None,
                            };
                            lhs = Expr::InSubquery {
                                expr: Box::new(lhs),
                                subquery: Box::new(subq),
                                negated,
                            };
                        }
                    }
                    TokenKind::Like | TokenKind::Glob => {
                        let pattern = self.parse_expr_bp(8)?;
                        let escape = if self.eat(TokenKind::Escape) {
                            Some(Box::new(self.parse_expr_bp(8)?))
                        } else {
                            None
                        };
                        lhs = Expr::Like {
                            expr: Box::new(lhs),
                            pattern: Box::new(pattern),
                            escape,
                            op: if kw.kind == TokenKind::Glob {
                                LikeOp::Glob
                            } else {
                                LikeOp::Like
                            },
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
                let rhs = self.parse_expr_bp(17)?;
                // Fold `-9223372036854775808` and negative integer literals cleanly!
                if let Expr::Float(f) = rhs {
                    return Ok(Expr::Float(-f));
                }
                if let Expr::Integer(i) = rhs {
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
                let rhs = self.parse_expr_bp(17)?;
                Ok(Expr::Unary {
                    op: UnaryOp::Plus,
                    expr: Box::new(rhs),
                })
            }
            TokenKind::BitNot => {
                self.advance();
                let rhs = self.parse_expr_bp(17)?;
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
                if let Some(i) = parse_int_literal(tok.text)? {
                    Ok(Expr::Integer(i))
                } else {
                    Ok(Expr::Float(parse_float_literal(tok.text)?))
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
            k if Self::can_be_ident(k) => {
                let first = self.parse_ident()?;
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
                    return Ok(Expr::FunctionCall {
                        name: first,
                        distinct,
                        args,
                        filter,
                    });
                }

                // Qualified column `table.col` (or `schema.table.col`)
                if self.eat(TokenKind::Dot) {
                    let second = self.parse_ident()?;
                    if self.eat(TokenKind::Dot) {
                        let third = self.parse_ident()?;
                        let tbl_qual = if first.eq_ignore_ascii_case("main") {
                            second
                        } else {
                            format!("{first}.{second}")
                        };
                        return Ok(Expr::Column {
                            table: Some(tbl_qual),
                            column: third,
                        });
                    }
                    return Ok(Expr::Column {
                        table: Some(first),
                        column: second,
                    });
                }

                Ok(Expr::Column {
                    table: None,
                    column: first,
                })
            }
            _ => Err(self.syntax_err_at(
                tok,
                format!("near \"{}\": syntax error", tok.text),
            )),
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
