//! SQLite shell completion virtual table (ext/misc/completion.c).
use std::collections::VecDeque;

use sqliters::{
    ErrorCode, Result, SqliteError, VTabConstraintOp, VTabHostContext, VTabIndexInfo,
    VTabRiskFlags, Value, VirtualCursor, VirtualTable, VirtualTableModule,
};

pub struct CompletionModule;

impl VirtualTableModule for CompletionModule {
    fn name(&self) -> &str {
        "completion"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn is_eponymous(&self) -> bool {
        true
    }

    fn is_eponymous_only(&self) -> bool {
        true
    }

    fn create(
        &self,
        _ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        Err(SqliteError::new(
            ErrorCode::Error,
            "no such module: completion",
        ))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        ctx.declare_vtab(
            "CREATE TABLE x(candidate TEXT, prefix TEXT HIDDEN, wholeline TEXT HIDDEN, phase INT HIDDEN)",
        )?;
        Ok(Box::new(CompletionTable))
    }
}

struct CompletionTable;

impl VirtualTable for CompletionTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let mut selected = [None, None];
        for (index, constraint) in info.constraints.iter().enumerate() {
            if constraint.usable
                && constraint.op == VTabConstraintOp::Eq
                && (1..=2).contains(&constraint.i_column)
            {
                selected[(constraint.i_column - 1) as usize] = Some(index);
            }
        }
        let mut argc = 0;
        for (position, index) in selected.into_iter().enumerate() {
            if let Some(index) = index {
                argc += 1;
                info.idx_num |= 1 << position;
                info.constraint_usage[index].argv_index = argc;
                info.constraint_usage[index].omit = true;
            }
        }
        info.estimated_cost = 5000.0 - 1000.0 * argc as f64;
        info.estimated_rows = 500 - 100 * argc as i64;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(CompletionCursor::default()))
    }
}

#[derive(Clone, Copy, Default)]
enum Phase {
    #[default]
    Keyword = 1,
    Database = 7,
    Schema = 8,
    Column = 9,
    Eof = 11,
}

struct Candidate {
    text: String,
    phase: Phase,
}

#[derive(Default)]
struct CompletionCursor {
    prefix: Option<String>,
    line: Option<String>,
    pending: VecDeque<Candidate>,
    phase: Phase,
    rowid: i64,
    lazy: bool,
}

impl CompletionCursor {
    fn read_phase(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let phase = self.phase;
        let candidates = match phase {
            Phase::Keyword => {
                self.phase = Phase::Database;
                KEYWORDS.iter().map(|word| (*word).to_owned()).collect()
            }
            Phase::Database => {
                self.phase = Phase::Schema;
                db_names(ctx)?
            }
            Phase::Schema | Phase::Column => {
                self.phase = if matches!(phase, Phase::Schema) {
                    Phase::Column
                } else {
                    Phase::Eof
                };
                let names = db_names(ctx)?;
                let sql = names.iter().map(|name| {
                    let identifier = name.replace('"', "\"\"");
                    if matches!(phase, Phase::Schema) {
                        format!("SELECT name FROM \"{identifier}\".sqlite_schema")
                    } else {
                        let literal = name.replace('\'', "''");
                        format!(
                            "SELECT pti.name FROM \"{identifier}\".sqlite_schema AS sm \
                             JOIN pragma_table_xinfo(sm.name,'{literal}') AS pti WHERE sm.type='table'"
                        )
                    }
                }).collect::<Vec<_>>().join(" UNION ");
                if sql.is_empty() {
                    Vec::new()
                } else {
                    text_column(ctx.query_sql(&sql, &[])?, 0)
                }
            }
            Phase::Eof => return Ok(()),
        };

        let prefix = self.prefix.as_deref().unwrap_or("").as_bytes();
        self.pending
            .extend(candidates.into_iter().filter_map(|text| {
                text.as_bytes()
                    .get(..prefix.len())
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
                    .then_some(Candidate { text, phase })
            }));
        Ok(())
    }

    fn read_pending(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        while !matches!(self.phase, Phase::Eof) && (!self.lazy || self.pending.is_empty()) {
            self.read_phase(ctx)?;
        }
        Ok(())
    }
}

fn db_names(ctx: &mut dyn VTabHostContext) -> Result<Vec<String>> {
    Ok(text_column(ctx.query_sql("PRAGMA database_list", &[])?, 1))
}

fn text_column(rows: Vec<Vec<Value>>, index: usize) -> Vec<String> {
    rows.into_iter()
        .filter_map(|row| {
            row.get(index)
                .filter(|value| !value.is_null())
                .map(Value::to_text_lossy)
        })
        .collect()
}

fn arg_text(value: Option<&Value>) -> Option<String> {
    let mut text = value?.to_text_lossy();
    if text.is_empty() {
        return None;
    }
    if let Some(end) = text.find('\0') {
        text.truncate(end);
    }
    Some(text)
}

impl VirtualCursor for CompletionCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.prefix = (idx_num & 1 != 0).then(|| arg_text(args.first())).flatten();
        self.line = (idx_num & 2 != 0)
            .then(|| arg_text(args.get(usize::from(idx_num & 1 != 0))))
            .flatten();
        if self.prefix.is_none()
            && let Some(line) = self.line.as_deref()
        {
            let suffix = line
                .rsplit(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .next()
                .filter(|part| !part.is_empty());
            self.prefix = suffix.map(str::to_owned);
        }
        self.pending.clear();
        self.phase = Phase::Keyword;
        self.rowid = 1;
        self.read_pending(ctx)
    }

    fn set_lazy(&mut self, lazy: bool) -> bool {
        self.lazy = lazy;
        true
    }

    fn unlazy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.lazy = false;
        self.read_pending(ctx)
    }

    fn next(&mut self) -> Result<()> {
        self.pending.pop_front();
        self.rowid += 1;
        Ok(())
    }

    fn next_ctx(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.next()?;
        self.read_pending(ctx)
    }

    fn eof(&self) -> bool {
        self.pending.is_empty()
    }

    fn column(&self, index: usize) -> Result<Value> {
        let Some(candidate) = self.pending.front() else {
            return Ok(Value::Null);
        };
        Ok(match index {
            0 => Value::Text(candidate.text.clone()),
            1 => self
                .prefix
                .as_ref()
                .map_or(Value::Null, |text| Value::Text(text.clone())),
            2 => self
                .line
                .as_ref()
                .map_or(Value::Null, |text| Value::Text(text.clone())),
            3 => Value::Integer(candidate.phase as i64),
            _ => Value::Null,
        })
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.rowid)
    }
}

// sqlite3_keyword_name order from the SQLite 3.54.1 reference build.
const KEYWORDS: &[&str] = &[
    "REINDEX",
    "INDEXED",
    "INDEX",
    "DESC",
    "ESCAPE",
    "EACH",
    "CHECK",
    "KEY",
    "BEFORE",
    "FOREIGN",
    "FOR",
    "IGNORE",
    "REGEXP",
    "EXPLAIN",
    "INSTEAD",
    "ADD",
    "DATABASE",
    "AS",
    "SELECT",
    "TABLE",
    "LEFT",
    "THEN",
    "END",
    "DEFERRABLE",
    "ELSE",
    "EXCLUDE",
    "DELETE",
    "TEMPORARY",
    "TEMP",
    "OR",
    "ISNULL",
    "NULLS",
    "SAVEPOINT",
    "INTERSECT",
    "TIES",
    "NOTNULL",
    "NOT",
    "NO",
    "NULL",
    "LIKE",
    "EXCEPT",
    "TRANSACTION",
    "ACTION",
    "ON",
    "NATURAL",
    "ALTER",
    "RAISE",
    "EXCLUSIVE",
    "EXISTS",
    "CONSTRAINT",
    "INTO",
    "OFFSET",
    "OF",
    "SET",
    "TRIGGER",
    "RANGE",
    "GENERATED",
    "DETACH",
    "HAVING",
    "GLOB",
    "BEGIN",
    "INNER",
    "REFERENCES",
    "UNIQUE",
    "QUERY",
    "WITHOUT",
    "WITH",
    "OUTER",
    "RELEASE",
    "ATTACH",
    "BETWEEN",
    "NOTHING",
    "GROUPS",
    "GROUP",
    "CASCADE",
    "ASC",
    "DEFAULT",
    "CASE",
    "COLLATE",
    "CREATE",
    "CURRENT_DATE",
    "IMMEDIATE",
    "JOIN",
    "INSERT",
    "MATCH",
    "PLAN",
    "ANALYZE",
    "PRAGMA",
    "MATERIALIZED",
    "DEFERRED",
    "DISTINCT",
    "IS",
    "UPDATE",
    "VALUES",
    "VIRTUAL",
    "ALWAYS",
    "WHEN",
    "WHERE",
    "RECURSIVE",
    "ABORT",
    "AFTER",
    "RENAME",
    "AND",
    "DROP",
    "PARTITION",
    "AUTOINCREMENT",
    "TO",
    "IN",
    "CAST",
    "COLUMN",
    "COMMIT",
    "CONFLICT",
    "CROSS",
    "CURRENT_TIMESTAMP",
    "CURRENT_TIME",
    "CURRENT",
    "PRECEDING",
    "FAIL",
    "LAST",
    "FILTER",
    "REPLACE",
    "FIRST",
    "FOLLOWING",
    "FROM",
    "FULL",
    "LIMIT",
    "IF",
    "ORDER",
    "RESTRICT",
    "OTHERS",
    "OVER",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "ROWS",
    "ROW",
    "UNBOUNDED",
    "UNION",
    "USING",
    "VACUUM",
    "VIEW",
    "WINDOW",
    "DO",
    "BY",
    "INITIALLY",
    "ALL",
    "PRIMARY",
];
