//! SQL Abstract Syntax Tree (AST) representing SQLite 3.54.1 statements and expressions.

#[derive(Debug, Clone, PartialEq)]
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
    CreateTable(CreateTableStmt),
    DropTable {
        if_exists: bool,
        name: String,
    },
    CreateIndex(CreateIndexStmt),
    DropIndex {
        if_exists: bool,
        name: String,
    },
    AlterTable {
        table: String,
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
    Vacuum,
    Analyze(Option<String>),
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

#[derive(Debug, Clone, PartialEq)]
pub struct CreateTableStmt {
    pub temp: bool,
    pub if_not_exists: bool,
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub without_rowid: bool,
    pub strict: bool,
    /// Original `CREATE TABLE ...` SQL text stored verbatim in `sqlite_schema.sql`.
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
    Check(Expr),
    Collate(String),
    ForeignKey {
        ref_table: String,
        ref_columns: Vec<String>,
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
    Check(Expr),
    ForeignKey {
        columns: Vec<String>,
        ref_table: String,
        ref_columns: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedColumn {
    pub name: String,
    pub collation: Option<String>,
    pub desc: bool,
    pub nulls_last: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateIndexStmt {
    pub unique: bool,
    pub if_not_exists: bool,
    pub name: String,
    pub table_name: String,
    pub columns: Vec<IndexedColumn>,
    pub where_clause: Option<Expr>,
    pub raw_sql: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AlterTableAction {
    RenameTo(String),
    RenameColumn { old_name: String, new_name: String },
    AddColumn(ColumnDef),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InsertStmt {
    pub or_conflict: Option<ConflictAction>,
    pub table: String,
    pub columns: Option<Vec<String>>,
    pub source: InsertSource,
    pub returning: Vec<ResultColumn>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Select(Box<SelectStmt>),
    DefaultValues,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateStmt {
    pub or_conflict: Option<ConflictAction>,
    pub table: String,
    pub alias: Option<String>,
    pub assignments: Vec<(String, Expr)>,
    pub selection: Option<Expr>,
    pub returning: Vec<ResultColumn>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeleteStmt {
    pub table: String,
    pub alias: Option<String>,
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

#[derive(Debug, Clone, PartialEq)]
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
    TableStar(String),
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FromClause {
    pub head: TableOrSubquery,
    pub joins: Vec<JoinClause>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableOrSubquery {
    Table {
        name: String,
        alias: Option<String>,
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
    Column {
        table: Option<String>,
        column: String,
    },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentTimeKind {
    Date,
    Time,
    Timestamp,
}
