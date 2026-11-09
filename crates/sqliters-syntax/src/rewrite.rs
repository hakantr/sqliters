//! Token-span SQL rewriting primitives for `ALTER TABLE` (`sqlite/src/alter.c`).
#![forbid(unsafe_code)]

use sqliters_types::{ErrorCode, Result, SqliteError};

use crate::ast::Stmt;
use crate::parser::{TokenSpan, parse_one};
use crate::token::{Token, TokenKind, keyword_kind, tokenize};

/// Formats `new_name` as a replacement SQL identifier token, preserving quoting when the original
/// token was quoted or adding double quotes when `new_name` is a keyword or contains non-identifier
/// characters.
pub fn format_replacement_ident(orig_tok_text: &str, new_name: &str) -> String {
    let was_quoted = orig_tok_text.starts_with('"')
        || orig_tok_text.starts_with('`')
        || orig_tok_text.starts_with('[')
        || orig_tok_text.starts_with('\'');
    let needs_quotes = was_quoted
        || new_name.is_empty()
        || keyword_kind(new_name).is_some()
        || new_name.as_bytes()[0].is_ascii_digit()
        || !new_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if needs_quotes {
        format!("\"{}\"", new_name.replace('"', "\"\""))
    } else {
        new_name.to_owned()
    }
}

/// Applies non-overlapping [`TokenSpan`] replacements to `sql`, preserving all surrounding
/// whitespace, comments, and unmodified tokens verbatim.
pub fn apply_span_edits(sql: &str, mut edits: Vec<(TokenSpan, String)>) -> String {
    if edits.is_empty() {
        return sql.to_owned();
    }
    edits.sort_by_key(|(span, _)| span.offset);
    let mut out = String::with_capacity(sql.len() + 16);
    let mut cursor = 0usize;
    for (span, rep) in edits {
        if span.offset >= cursor && span.offset + span.len <= sql.len() {
            out.push_str(&sql[cursor..span.offset]);
            out.push_str(&rep);
            cursor = span.offset + span.len;
        }
    }
    out.push_str(&sql[cursor..]);
    out
}

fn find_matching_rparen(tokens: &[Token<'_>], lp_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, tok) in tokens.iter().enumerate().skip(lp_idx) {
        match tok.kind {
            TokenKind::Lp => depth += 1,
            TokenKind::Rp => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_table_constraint_item(tokens: &[Token<'_>], start: usize, end: usize) -> bool {
    if start >= end {
        return false;
    }
    if tokens[start].kind == TokenKind::Constraint && start + 2 < end {
        matches!(
            tokens[start + 2].kind,
            TokenKind::Primary | TokenKind::Unique | TokenKind::Check | TokenKind::Foreign
        )
    } else {
        matches!(
            tokens[start].kind,
            TokenKind::Primary | TokenKind::Unique | TokenKind::Check | TokenKind::Foreign
        )
    }
}

/// Structurally removes the `col_idx`-th column definition from a `CREATE TABLE` SQL definition
/// via lexical token spans (`dropColumnFunc` in `sqlite/src/alter.c`), preserving surrounding
/// comments, formatting, remaining column definitions, and table constraints.
pub fn remove_column_def_from_create_table_sql(sql: &str, col_idx: usize) -> Result<String> {
    let corrupt_err = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let Ok((Some(Stmt::CreateTable(ct)), _)) = parse_one(sql) else {
        return Err(corrupt_err());
    };
    if ct.columns.len() <= 1 || col_idx >= ct.columns.len() {
        return Err(corrupt_err());
    }

    let tokens = tokenize(sql).map_err(|_| corrupt_err())?;
    let Some(outer_lp) = tokens.iter().position(|t| t.kind == TokenKind::Lp) else {
        return Err(corrupt_err());
    };
    let Some(outer_rp) = find_matching_rparen(&tokens, outer_lp) else {
        return Err(corrupt_err());
    };

    // Each top-level item inside `(...)`: `(start_tok_idx, end_tok_idx, preceding_comma_tok_idx)`
    let mut items: Vec<(usize, usize, Option<usize>)> = Vec::new();
    let mut depth = 0usize;
    let mut cur_start = outer_lp + 1;
    let mut prev_comma: Option<usize> = None;
    for (idx, tok) in tokens.iter().enumerate().take(outer_rp).skip(outer_lp + 1) {
        match tok.kind {
            TokenKind::Lp => depth += 1,
            TokenKind::Rp => depth = depth.saturating_sub(1),
            TokenKind::Comma if depth == 0 => {
                if cur_start < idx {
                    items.push((cur_start, idx, prev_comma));
                }
                prev_comma = Some(idx);
                cur_start = idx + 1;
            }
            _ => {}
        }
    }
    if cur_start < outer_rp {
        items.push((cur_start, outer_rp, prev_comma));
    }

    let mut seen_cols = 0usize;
    let mut target_item_pos = None;
    for (pos, &(istart, iend, _)) in items.iter().enumerate() {
        if !is_table_constraint_item(&tokens, istart, iend) {
            if seen_cols == col_idx {
                target_item_pos = Some(pos);
            }
            seen_cols += 1;
        }
    }

    let Some(item_pos) = target_item_pos else {
        return Err(SqliteError::new(
            ErrorCode::Corrupt,
            "database disk image is malformed",
        ));
    };
    if seen_cols <= 1 {
        return Err(SqliteError::new(
            ErrorCode::Corrupt,
            "database disk image is malformed",
        ));
    }

    let (del_start, del_end) = if item_pos + 1 < items.len() {
        let start_off = tokens[items[item_pos].0].offset;
        let end_off = tokens[items[item_pos + 1].0].offset;
        (start_off, end_off)
    } else {
        let Some(comma_idx) = items[item_pos].2 else {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "database disk image is malformed",
            ));
        };
        let start_off = tokens[comma_idx].offset;
        let end_off = tokens[outer_rp].offset;
        (start_off, end_off)
    };

    let mut out = String::with_capacity(sql.len());
    out.push_str(&sql[..del_start]);
    out.push_str(&sql[del_end..]);
    Ok(out)
}
