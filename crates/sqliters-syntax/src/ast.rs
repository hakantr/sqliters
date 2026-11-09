//! SQL Abstract Syntax Tree (AST) representing SQLite 3.54.1 statements and expressions.
#![forbid(unsafe_code)]

/// Structural `[schema.]object` SQL name (`nm dbnm` / `fullname` in `sqlite/src/parse.y`).
///
/// `schema` and `name` are stored already dequoted and are **never** flattened with `.` so a
/// quoted identifier containing a literal dot (e.g. `"a.b"`) remains unambiguous from `a.b`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedName {
    pub schema: Option<String>,
    pub name: String,
}

impl QualifiedName {
    /// Creates an unqualified object name (`schema: None`).
    #[inline]
    pub fn unqualified(name: impl Into<String>) -> Self {
        Self {
            schema: None,
            name: name.into(),
        }
    }

    /// Creates a schema-qualified object name (`schema: Some(schema)`).
    #[inline]
    pub fn qualified(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: Some(schema.into()),
            name: name.into(),
        }
    }

    /// Formats the name for SQLite diagnostic messages (`"schema.name"` or `"name"`).
    pub fn display_name(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Formats the name as valid SQL, double-quoting components when needed.
    pub fn display_sql(&self) -> String {
        match &self.schema {
            Some(s) => format!("{}.{}", quote_sql_ident(s), quote_sql_ident(&self.name)),
            None => quote_sql_ident(&self.name),
        }
    }
}

/// Structural 1-, 2-, or 3-part column reference (`[schema.][table.]column`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColumnRef {
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: String,
}

impl ColumnRef {
    /// Creates a 1-part unqualified column reference (`column`).
    #[inline]
    pub fn unqualified(column: impl Into<String>) -> Self {
        Self {
            schema: None,
            table: None,
            column: column.into(),
        }
    }

    /// Creates a 2-part table-qualified column reference (`table.column`).
    #[inline]
    pub fn table_qualified(table: impl Into<String>, column: impl Into<String>) -> Self {
        Self {
            schema: None,
            table: Some(table.into()),
            column: column.into(),
        }
    }

    /// Creates a 3-part schema-and-table-qualified column reference (`schema.table.column`).
    #[inline]
    pub fn schema_table_qualified(
        schema: impl Into<String>,
        table: impl Into<String>,
        column: impl Into<String>,
    ) -> Self {
        Self {
            schema: Some(schema.into()),
            table: Some(table.into()),
            column: column.into(),
        }
    }

    /// Formats the column reference for SQLite diagnostic messages (`"s.t.c"`, `"t.c"`, or `"c"`).
    pub fn display_name(&self) -> String {
        match (&self.schema, &self.table) {
            (Some(s), Some(t)) => format!("{s}.{t}.{}", self.column),
            (None, Some(t)) => format!("{t}.{}", self.column),
            (_, None) => self.column.clone(),
        }
    }
}

fn quote_sql_ident(ident: &str) -> String {
    let is_simple = !ident.is_empty()
        && ident
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()));
    if is_simple {
        ident.to_owned()
    } else {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Stmt {
    Explain {
        query_plan: bool,
        stmt: Box<Stmt>,
    },
    Begin {
        mode: TransactionMode,
    },
    Commit,
    Rollback {
        savepoint: Option<String>,
    },
    Savepoint(String),
    Release(String),
    AttachDatabase {
        expr: Expr,
        schema_name: Expr,
        key: Option<Expr>,
    },
    DetachDatabase {
        schema_name: Expr,
    },
    CreateTable(CreateTableStmt),
    CreateTableAs {
        temp: bool,
        if_not_exists: bool,
        name: QualifiedName,
        query: Box<SelectStmt>,
    },
    DropTable {
        if_exists: bool,
        name: QualifiedName,
    },
    CreateIndex(CreateIndexStmt),
    DropIndex {
        if_exists: bool,
        name: QualifiedName,
    },
    CreateView(CreateViewStmt),
    DropView {
        if_exists: bool,
        name: QualifiedName,
    },
    AlterTable {
        table: QualifiedName,
        action: AlterTableAction,
    },
    Insert(InsertStmt),
    Select(SelectStmt),
    Update(UpdateStmt),
    Delete(DeleteStmt),
    Pragma {
        schema: Option<String>,
        name: String,
        value: Option<PragmaValue>,
    },
    Vacuum {
        schema: Option<String>,
        into: Option<Expr>,
    },
    Analyze(Option<QualifiedName>),
    Reindex(Option<QualifiedName>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransactionMode {
    #[default]
    Deferred,
    Immediate,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictAction {
    #[default]
    Abort,
    Rollback,
    Fail,
    Ignore,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FkAction {
    #[default]
    NoAction,
    Restrict,
    SetNull,
    SetDefault,
    Cascade,
}

impl FkAction {
    pub const fn as_sql_str(self) -> &'static str {
        match self {
            Self::NoAction => "NO ACTION",
            Self::Restrict => "RESTRICT",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
            Self::Cascade => "CASCADE",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateTableStmt {
    pub temp: bool,
    pub if_not_exists: bool,
    pub name: QualifiedName,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub without_rowid: bool,
    pub strict: bool,
    /// Normalized catalog SQL (`"CREATE TABLE <name>(...)"`) without `TEMP`, `IF NOT EXISTS`, or `schema.` (`sqlite/src/build.c`).
    pub catalog_sql: String,
    /// Original `CREATE TABLE ...` SQL text as written by the caller.
    pub raw_sql: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: String,
    pub constraints: Vec<ColumnConstraint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ColumnConstraint {
    PrimaryKey {
        desc: bool,
        conflict: ConflictAction,
        autoincrement: bool,
    },
    NotNull {
        conflict: ConflictAction,
    },
    Null,
    Unique {
        conflict: ConflictAction,
    },
    Default(Expr),
    Check {
        name: Option<String>,
        expr_sql: String,
        expr: Expr,
    },
    Generated {
        expr: Expr,
        stored: bool,
    },
    Collate(String),
    ForeignKey {
        ref_table: String,
        ref_columns: Vec<String>,
        on_delete: FkAction,
        on_update: FkAction,
        deferred: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableConstraint {
    PrimaryKey {
        columns: Vec<IndexedColumn>,
        conflict: ConflictAction,
    },
    Unique {
        columns: Vec<IndexedColumn>,
        conflict: ConflictAction,
    },
    Check {
        name: Option<String>,
        expr_sql: String,
        expr: Expr,
    },
    ForeignKey {
        columns: Vec<String>,
        ref_table: String,
        ref_columns: Vec<String>,
        on_delete: FkAction,
        on_update: FkAction,
        deferred: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedColumn {
    pub name: String,
    pub expr: Option<Expr>,
    pub collation: Option<String>,
    pub desc: bool,
    pub nulls_last: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateIndexStmt {
    pub unique: bool,
    pub if_not_exists: bool,
    pub name: QualifiedName,
    pub table_name: String,
    pub columns: Vec<IndexedColumn>,
    pub where_clause: Option<Expr>,
    /// Normalized catalog SQL (`"CREATE [UNIQUE ]INDEX <name> ON ..."`) without `IF NOT EXISTS` or `schema.` (`sqlite/src/build.c`).
    pub catalog_sql: String,
    pub raw_sql: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateViewStmt {
    pub temp: bool,
    pub if_not_exists: bool,
    pub name: QualifiedName,
    pub columns: Option<Vec<String>>,
    pub query: Box<SelectStmt>,
    /// Normalized catalog SQL (`"CREATE VIEW <name>..."`) without `TEMP`, `IF NOT EXISTS`, or `schema.` (`sqlite/src/build.c`).
    pub catalog_sql: String,
    /// Original `CREATE VIEW ...` SQL text as written by the caller.
    pub raw_sql: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AlterTableAction {
    RenameTo(String),
    RenameColumn { old_name: String, new_name: String },
    AddColumn(ColumnDef),
    DropColumn(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InsertStmt {
    pub ctes: Vec<CommonTableExpr>,
    pub or_conflict: Option<ConflictAction>,
    pub table: QualifiedName,
    pub alias: Option<String>,
    pub columns: Option<Vec<String>>,
    pub source: InsertSource,
    pub upsert_clauses: Vec<UpsertClause>,
    pub returning: Vec<ResultColumn>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Select(Box<SelectStmt>),
    DefaultValues,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetAssignment {
    pub columns: Vec<String>,
    /// True when written in parenthesized `SET (c1, ...) = expr` form (`LP idlist RP EQ expr` in `sqlite/src/parse.y`).
    pub row_value: bool,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpsertClause {
    pub target: Option<UpsertTarget>,
    pub action: UpsertAction,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpsertTarget {
    pub columns: Vec<IndexedColumn>,
    pub where_clause: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpsertAction {
    DoNothing,
    DoUpdate {
        assignments: Vec<SetAssignment>,
        where_clause: Option<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexHint {
    IndexedBy(String),
    NotIndexed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateStmt {
    pub ctes: Vec<CommonTableExpr>,
    pub or_conflict: Option<ConflictAction>,
    pub table: QualifiedName,
    pub alias: Option<String>,
    pub indexed_by: Option<IndexHint>,
    pub assignments: Vec<SetAssignment>,
    pub from: Option<FromClause>,
    pub selection: Option<Expr>,
    pub returning: Vec<ResultColumn>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeleteStmt {
    pub ctes: Vec<CommonTableExpr>,
    pub table: QualifiedName,
    pub alias: Option<String>,
    pub indexed_by: Option<IndexHint>,
    pub selection: Option<Expr>,
    pub returning: Vec<ResultColumn>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PragmaValue {
    Ident(String),
    Integer(i64),
    String(String),
}

// ============================================================================
// SELECT & Compound Queries
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct SelectStmt {
    pub ctes: Vec<CommonTableExpr>,
    pub body: SelectBody,
    pub order_by: Vec<OrderByTerm>,
    pub limit: Option<LimitClause>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommonTableExpr {
    pub name: String,
    pub columns: Option<Vec<String>>,
    pub materialized: Option<bool>,
    pub query: SelectStmt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectBody {
    pub head: SelectCore,
    pub compounds: Vec<(CompoundOp, SelectCore)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundOp {
    Union,
    UnionAll,
    Intersect,
    Except,
}

impl CompoundOp {
    pub const fn as_sql_str(self) -> &'static str {
        match self {
            Self::Union => "UNION",
            Self::UnionAll => "UNION ALL",
            Self::Intersect => "INTERSECT",
            Self::Except => "EXCEPT",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum SelectCore {
    Select {
        distinct: bool,
        projection: Vec<ResultColumn>,
        from: Option<FromClause>,
        selection: Option<Expr>,
        group_by: Vec<Expr>,
        having: Option<Expr>,
    },
    Values(Vec<Vec<Expr>>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultColumn {
    Star,
    TableStar {
        schema: Option<String>,
        table: String,
    },
    Expr {
        expr: Expr,
        alias: Option<String>,
        span_text: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FromClause {
    pub head: TableOrSubquery,
    pub joins: Vec<JoinClause>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableOrSubquery {
    Table {
        name: QualifiedName,
        table_args: Option<Vec<Expr>>,
        alias: Option<String>,
        indexed_by: Option<IndexHint>,
    },
    Subquery {
        query: Box<SelectStmt>,
        alias: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct JoinClause {
    pub join_type: JoinType,
    pub natural: bool,
    pub right: TableOrSubquery,
    pub constraint: Option<JoinConstraint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JoinConstraint {
    On(Expr),
    Using(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderByTerm {
    pub expr: Expr,
    pub desc: bool,
    pub nulls_last: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitClause {
    pub limit: Expr,
    pub offset: Option<Expr>,
}

// ============================================================================
// Expressions
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum BindParam {
    /// Anonymous `?` with 1-based auto-assigned parameter index
    Anonymous(usize),
    /// Numbered `?NNN` (1-based)
    Numbered(usize),
    /// Named `:name`, `@name`, `$name` (including prefix char)
    Named(String, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Negate,
    BitNot,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Concat,
    Multiply,
    Divide,
    Remainder,
    Add,
    Subtract,
    ShiftLeft,
    ShiftRight,
    BitAnd,
    BitOr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    Is,
    IsNot,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LikeOp {
    Like,
    Glob,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FunctionArgs {
    Star,
    Exprs(Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaiseAction {
    Ignore,
    Rollback,
    Abort,
    Fail,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Blob(Vec<u8>),
    CurrentTimeKw(CurrentTimeKind),
    Bind(BindParam),
    Column(ColumnRef),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Collate {
        expr: Box<Expr>,
        collation: String,
    },
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    Between {
        expr: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        negated: bool,
    },
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    InSubquery {
        expr: Box<Expr>,
        subquery: Box<SelectStmt>,
        negated: bool,
    },
    InTable {
        expr: Box<Expr>,
        table: QualifiedName,
        negated: bool,
    },
    Like {
        expr: Box<Expr>,
        pattern: Box<Expr>,
        escape: Option<Box<Expr>>,
        op: LikeOp,
        negated: bool,
    },
    Cast {
        expr: Box<Expr>,
        type_name: String,
    },
    Case {
        operand: Option<Box<Expr>>,
        branches: Vec<(Expr, Expr)>,
        else_expr: Option<Box<Expr>>,
    },
    FunctionCall {
        name: String,
        distinct: bool,
        args: FunctionArgs,
        filter: Option<Box<Expr>>,
    },
    Subquery(Box<SelectStmt>),
    Exists {
        subquery: Box<SelectStmt>,
        negated: bool,
    },
    Raise {
        action: RaiseAction,
        message: Option<Box<Expr>>,
    },
    RowValue(Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentTimeKind {
    Date,
    Time,
    Timestamp,
}
