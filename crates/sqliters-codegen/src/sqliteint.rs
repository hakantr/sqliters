//! The parse tree and the code-generator state of SQLite 3.54.1 (`sqliteInt.h`): `Expr`,
//! `ExprList`, `Select`, `SrcList`, `IdList`, `With`, `Window`, `AggInfo`, `NameContext`,
//! `Walker`, `Table` / `Column` / `Index`, `Parse` and the connection data (`sqlite3 *db`) the
//! code generator reads.
//!
//! C links these objects with raw pointers and mutates them in place from many functions. Here
//! every object lives in an arena owned by [`Parse`] and is referred to by an id (`ExprId`,
//! `SelectId`, …); id 0 is C's NULL pointer. Freeing (`sqlite3ExprDelete` etc.) is therefore a
//! no-op: the arenas are dropped with the `Parse`. Field names are C's names in snake case.
#![allow(non_upper_case_globals)]

use std::sync::Arc;

use sqliters_vdbe::vdbeaux::{CollSeq, FuncDef, VList, Vdbe};

use crate::consts::*;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
        pub struct $name(pub u32);
        impl $name {
            /// C's NULL pointer.
            pub const NULL: $name = $name(0);
            #[inline]
            pub fn is_null(self) -> bool {
                self.0 == 0
            }
            /// `p!=0`
            #[inline]
            pub fn nn(self) -> bool {
                self.0 != 0
            }
            #[inline]
            pub fn ix(self) -> usize {
                self.0 as usize
            }
        }
    };
}
define_id!(ExprId);
define_id!(ExprListId);
define_id!(SelectId);
define_id!(SrcListId);
define_id!(IdListId);
define_id!(WithId);
define_id!(WindowId);
define_id!(AggInfoId);
define_id!(TableId);
define_id!(NcId);

/// `LogEst`
pub type LogEst = i16;
/// `Bitmask`
pub type Bitmask = u64;

/// `BMS`: the number of bits in a Bitmask
pub const BMS: i32 = 64;
/// `SQLITE_N_LIMIT`: `SQLITE_LIMIT_TRIGGER_STEPS+1`
pub const SQLITE_N_LIMIT: i32 = SQLITE_LIMIT_TRIGGER_STEPS + 1;

/// `EP_Propagate`: the properties that automatically propagate upward into parent nodes.
pub const EP_Propagate: u32 = EP_Collate | EP_Subquery | EP_HasFunc;

/// `struct Token`: a piece of the SQL text. `z` is the byte offset of the token in the
/// statement text (`Parse::z_sql`), [`Token::NO_Z`] for C's `z==0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Token {
    pub z: u32,
    pub n: u32,
}

impl Token {
    pub const NO_Z: u32 = u32::MAX;
    /// `{z=0, n=0}`
    pub const EMPTY: Token = Token { z: Token::NO_Z, n: 0 };
}

/// `struct Expr`: a node of an expression tree.
#[derive(Debug, Clone, Default)]
pub struct Expr {
    /// Operation performed by this node (`TK_*`)
    pub op: u8,
    /// Affinity, or RAISE type
    pub aff_expr: u8,
    /// TK_REGISTER/TK_TRUTH: original value of Expr.op; TK_COLUMN: the value of p5 for
    /// OP_Column; TK_AGG_FUNCTION: nesting depth; TK_FUNCTION: NC_SelfRef flag if needs OP_PureFunc
    pub op2: u8,
    /// `EP_*`
    pub flags: u32,
    /// `u.zToken`: token value, zero terminated and dequoted (valid unless `EP_IntValue`)
    pub z_token: Option<String>,
    /// `u.iValue`: non-negative integer value if `EP_IntValue`
    pub i_value: i32,
    /// Left subnode
    pub p_left: ExprId,
    /// Right subnode
    pub p_right: ExprId,
    /// `x.pList` (an `ExprListId`) or, with `EP_xIsSelect`, `x.pSelect` (a `SelectId`)
    pub x: u32,
    /// Height of the tree headed by this node
    pub n_height: i32,
    /// TK_COLUMN: cursor number of table holding column; TK_REGISTER: register number;
    /// TK_TRIGGER: 1 -> new, 0 -> old; EP_Unlikely: 134217728 times likelihood;
    /// TK_IN: ephemeral table holding RHS; TK_SELECT_COLUMN: number of columns on the LHS;
    /// TK_SELECT: 1st register of result vector
    pub i_table: i32,
    /// TK_COLUMN: column index, -1 for rowid; TK_VARIABLE: variable number (always >= 1);
    /// TK_SELECT_COLUMN: column of the result vector
    pub i_column: i16,
    /// Which entry in pAggInfo->aCol[] or ->aFunc[]
    pub i_agg: i16,
    /// `w.iJoin` (if EP_OuterON or EP_InnerON: the right table) or `w.iOfst` (start of token
    /// from start of statement)
    pub w: i32,
    /// Used by TK_AGG_COLUMN and TK_AGG_FUNCTION
    pub p_agg_info: AggInfoId,
    /// `y.pTab`: TK_COLUMN: table containing column
    pub y_tab: TableId,
    /// `y.pWin`: EP_WinFunc: window/filter definition for a function
    pub y_win: WindowId,
    /// `y.nReg`: TK_NULLS: number of registers to NULL out
    pub y_n_reg: i32,
    /// `y.sub.iAddr`: TK_IN, TK_SELECT, TK_EXISTS: subroutine entry address
    pub y_sub_i_addr: i32,
    /// `y.sub.regReturn`: register used to hold return address
    pub y_sub_reg_return: i32,
}

impl Expr {
    /// `ExprHasProperty(E,P)`
    #[inline]
    pub fn has(&self, p: u32) -> bool {
        self.flags & p != 0
    }
    /// `ExprHasAllProperty(E,P)`
    #[inline]
    pub fn has_all(&self, p: u32) -> bool {
        self.flags & p == p
    }
    /// `ExprUseXSelect(E)`
    #[inline]
    pub fn use_x_select(&self) -> bool {
        self.flags & EP_xIsSelect != 0
    }
    /// `ExprUseXList(E)`
    #[inline]
    pub fn use_x_list(&self) -> bool {
        self.flags & EP_xIsSelect == 0
    }
    /// `E->x.pList`
    #[inline]
    pub fn x_list(&self) -> ExprListId {
        ExprListId(self.x)
    }
    /// `E->x.pSelect`
    #[inline]
    pub fn x_select(&self) -> SelectId {
        SelectId(self.x)
    }
    /// `E->u.zToken` ("" for NULL)
    #[inline]
    pub fn tok(&self) -> &str {
        self.z_token.as_deref().unwrap_or("")
    }
    /// `ExprAlwaysTrue(E)`
    #[inline]
    pub fn always_true(&self) -> bool {
        self.flags & (EP_OuterON | EP_IsTrue) == EP_IsTrue
    }
    /// `ExprAlwaysFalse(E)`
    #[inline]
    pub fn always_false(&self) -> bool {
        self.flags & (EP_OuterON | EP_IsFalse) == EP_IsFalse
    }
    /// `ExprIsFullSize(E)`
    #[inline]
    pub fn is_full_size(&self) -> bool {
        self.flags & (EP_Reduced | EP_TokenOnly) == 0
    }
    /// `ExprUseYTab(E)`
    #[inline]
    pub fn use_y_tab(&self) -> bool {
        self.flags & (EP_WinFunc | EP_Subrtn) == 0
    }
}

/// `struct ExprList_item`
#[derive(Debug, Clone, Default)]
pub struct ExprListItem {
    /// The parse tree for this expression
    pub p_expr: ExprId,
    /// Token associated with this expression
    pub z_e_name: Option<String>,
    /// `fg.sortFlags`: mask of KEYINFO_ORDER_* flags
    pub sort_flags: u8,
    /// `fg.eEName`: meaning of zEName (`ENAME_*`)
    pub e_e_name: u8,
    /// `fg.done`: indicates when processing is finished
    pub done: bool,
    /// `fg.reusable`: constant expression is reusable
    pub reusable: bool,
    /// `fg.bSorterRef`: defer evaluation until after sorting
    pub b_sorter_ref: bool,
    /// `fg.bNulls`: true if explicit "NULLS FIRST/LAST"
    pub b_nulls: bool,
    /// `fg.bUsed`: this column used in a SF_NestedFrom subquery
    pub b_used: bool,
    /// `fg.bUsingTerm`: term from the USING clause of a NestedFrom
    pub b_using_term: bool,
    /// `fg.bNoExpand`: term is an auxiliary in NestedFrom and should not be expanded by "*"
    pub b_no_expand: bool,
    /// `u.x.iOrderByCol`: for ORDER BY, column number in result set
    pub i_order_by_col: u16,
    /// `u.x.iAlias`: index into Parse.aAlias[] for zName
    pub i_alias: u16,
    /// `u.iConstExprReg`: register in which Expr value is cached (Parse.pConstExpr only)
    pub i_const_expr_reg: i32,
}

/// `struct ExprList`
#[derive(Debug, Clone, Default)]
pub struct ExprList {
    pub a: Vec<ExprListItem>,
}

impl ExprList {
    /// `pList->nExpr`
    #[inline]
    pub fn n_expr(&self) -> i32 {
        self.a.len() as i32
    }
}

/// `struct IdList`
#[derive(Debug, Clone, Default)]
pub struct IdList {
    /// `a[i].zName`
    pub a: Vec<String>,
}

/// `struct Subquery`
#[derive(Debug, Clone, Default)]
pub struct Subquery {
    /// A SELECT statement used in place of a table name
    pub p_select: SelectId,
    /// Address of subroutine to initialize a subquery
    pub addr_fill_sub: i32,
    /// Register holding return address of addrFillSub
    pub reg_return: i32,
    /// Registers holding results of a co-routine
    pub reg_result: i32,
}

/// `struct SrcItem`
#[derive(Debug, Clone, Default)]
pub struct SrcItem {
    /// Name of the table
    pub z_name: Option<String>,
    /// The "B" part of a "A AS B" phrase. zName is the "A"
    pub z_alias: Option<String>,
    /// Table object for zName
    pub p_s_tab: TableId,
    /// `fg.jointype`: type of join between this table and the previous (`JT_*`)
    pub jointype: u8,
    pub not_indexed: bool,
    pub is_indexed_by: bool,
    pub is_subquery: bool,
    pub is_tab_func: bool,
    pub is_correlated: bool,
    pub is_materialized: bool,
    pub via_coroutine: bool,
    pub is_recursive: bool,
    pub from_ddl: bool,
    pub is_cte: bool,
    pub not_cte: bool,
    pub is_using: bool,
    pub is_on: bool,
    pub is_synth_using: bool,
    pub is_nested_from: bool,
    pub rowid_used: bool,
    pub fixed_schema: bool,
    pub had_schema: bool,
    pub from_exists: bool,
    /// The VDBE cursor number used to access this table
    pub i_cursor: i32,
    /// Bit N set if column N used
    pub col_used: Bitmask,
    /// `u1.zIndexedBy`
    pub z_indexed_by: Option<String>,
    /// `u1.pFuncArg`
    pub p_func_arg: ExprListId,
    /// `u1.nRow`
    pub n_row: u32,
    /// `u3.pOn`
    pub p_on: ExprId,
    /// `u3.pUsing`
    pub p_using: IdListId,
    /// `u4.zDatabase`
    pub z_database: Option<String>,
    /// `u4.pSubq`
    pub p_subq: Option<Box<Subquery>>,
}

/// `struct OnOrUsing`
#[derive(Debug, Clone, Copy, Default)]
pub struct OnOrUsing {
    pub p_on: ExprId,
    pub p_using: IdListId,
}

/// `struct SrcList`
#[derive(Debug, Clone, Default)]
pub struct SrcList {
    pub a: Vec<SrcItem>,
}

impl SrcList {
    #[inline]
    pub fn n_src(&self) -> i32 {
        self.a.len() as i32
    }
}

/// `struct NameContext`
#[derive(Debug, Clone, Default)]
pub struct NameContext {
    /// One or more tables used to resolve names
    pub p_src_list: SrcListId,
    /// `uNC.pEList`: optional list of result-set columns (NC_UEList)
    pub p_e_list: ExprListId,
    /// `uNC.pAggInfo`: information about aggregates at this level (NC_UAggInfo)
    pub p_agg_info: AggInfoId,
    /// `uNC.iBaseReg`: for TK_REGISTER when parsing RETURNING (NC_UBaseReg)
    pub i_base_reg: i32,
    /// Next outer name context. NULL for outermost
    pub p_next: NcId,
    /// Number of names resolved by this context
    pub n_ref: i32,
    /// Number of errors encountered while resolving names
    pub n_nc_err: i32,
    /// Zero or more NC_* flags
    pub nc_flags: i32,
    /// Number of nested selects using this NC
    pub n_nested_select: u32,
    /// SELECT statement for any window functions
    pub p_win_select: SelectId,
}

/// `struct Select`
#[derive(Debug, Clone, Default)]
pub struct Select {
    /// One of: TK_UNION TK_ALL TK_INTERSECT TK_EXCEPT
    pub op: u8,
    /// Estimated number of result rows
    pub n_select_row: LogEst,
    /// Various SF_* values
    pub sel_flags: u32,
    /// Memory registers holding LIMIT & OFFSET counters
    pub i_limit: i32,
    pub i_offset: i32,
    /// Unique identifier number for this SELECT
    pub sel_id: u32,
    /// The fields of the result
    pub p_e_list: ExprListId,
    /// The FROM clause
    pub p_src: SrcListId,
    /// The WHERE clause
    pub p_where: ExprId,
    /// The GROUP BY clause
    pub p_group_by: ExprListId,
    /// The HAVING clause
    pub p_having: ExprId,
    /// The ORDER BY clause
    pub p_order_by: ExprListId,
    /// Prior select in a compound select statement
    pub p_prior: SelectId,
    /// Next select to the left in a compound
    pub p_next: SelectId,
    /// LIMIT expression. NULL means not used.
    pub p_limit: ExprId,
    /// WITH clause attached to this select. Or NULL.
    pub p_with: WithId,
    /// List of window functions
    pub p_win: WindowId,
    /// List of named window definitions
    pub p_win_defn: WindowId,
}

/// `struct SelectDest`
#[derive(Debug, Clone, Default)]
pub struct SelectDest {
    /// How to dispose of the results. One of SRT_*
    pub e_dest: u8,
    /// A parameter used by the eDest disposal method
    pub i_sd_parm: i32,
    /// A second parameter for the eDest disposal method
    pub i_sd_parm2: i32,
    /// Base register where results are written
    pub i_sdst: i32,
    /// Number of registers allocated
    pub n_sdst: i32,
    /// Affinity used for SRT_Set (one byte per column)
    pub z_aff_sdst: Option<Vec<u8>>,
    /// Key columns for SRT_Queue and SRT_DistQueue
    pub p_order_by: ExprListId,
}

/// `struct Cte`
#[derive(Debug, Clone, Default)]
pub struct Cte {
    pub z_name: String,
    /// List of explicit column names, or NULL
    pub p_cols: ExprListId,
    /// The definition of this CTE
    pub p_select: SelectId,
    /// Error message for circular references
    pub z_cte_err: Option<&'static str>,
    /// The MATERIALIZED flag (`M10d_*`)
    pub e_m10d: u8,
}

/// `struct With`
#[derive(Debug, Clone, Default)]
pub struct With {
    /// Belongs to the outermost Select of a view
    pub b_view: i32,
    /// Containing WITH clause, or NULL
    pub p_outer: WithId,
    pub a: Vec<Cte>,
}

/// `struct Window` (the fields built by the parser; the rest belongs to `window.c`).
#[derive(Debug, Clone, Default)]
pub struct Window {
    pub z_name: Option<String>,
    pub z_base: Option<String>,
    pub p_partition: ExprListId,
    pub p_order_by: ExprListId,
    pub e_frm_type: u8,
    pub e_start: u8,
    pub e_end: u8,
    pub b_implicit_frame: u8,
    pub e_exclude: u8,
    pub p_start: ExprId,
    pub p_end: ExprId,
    pub p_next_win: WindowId,
    pub p_filter: ExprId,
    pub p_owner: ExprId,
}

/// `struct AggInfo_col`
#[derive(Debug, Clone, Default)]
pub struct AggInfoCol {
    pub p_tab: TableId,
    pub p_c_expr: ExprId,
    pub i_table: i32,
    pub i_column: i32,
    pub i_sorter_column: i32,
}

/// `struct AggInfo_func`
#[derive(Debug, Clone, Default)]
pub struct AggInfoFunc {
    /// Expression encoding the function
    pub p_f_expr: ExprId,
    /// The aggregate function implementation
    pub p_func: Option<Arc<FuncDef>>,
    /// Ephemeral table used to enforce DISTINCT
    pub i_distinct: i32,
    /// Address of OP_OpenEphemeral
    pub i_dist_addr: i32,
    /// Ephemeral table to implement ORDER BY
    pub i_ob_tab: i32,
    /// iOBTab has payload columns separate from key
    pub b_ob_payload: bool,
    /// Enforce uniqueness on iOBTab keys
    pub b_ob_unique: bool,
    /// Transfer subtype info through sorter
    pub b_use_subtype: bool,
}

/// `struct AggInfo`
#[derive(Debug, Clone, Default)]
pub struct AggInfo {
    pub direct_mode: u8,
    pub use_sorting_idx: u8,
    pub n_sorting_column: u32,
    pub sorting_idx: i32,
    pub sorting_idx_p_tab: i32,
    pub i_first_reg: i32,
    pub p_group_by: ExprListId,
    pub a_col: Vec<AggInfoCol>,
    pub n_accumulator: i32,
    pub a_func: Vec<AggInfoFunc>,
    pub sel_id: u32,
}

impl AggInfo {
    #[inline]
    pub fn n_column(&self) -> i32 {
        self.a_col.len() as i32
    }
    #[inline]
    pub fn n_func(&self) -> i32 {
        self.a_func.len() as i32
    }
    /// `AggInfoColumnReg(A,I)`
    #[inline]
    pub fn column_reg(&self, i: i32) -> i32 {
        self.i_first_reg + i
    }
    /// `AggInfoFuncReg(A,I)`
    #[inline]
    pub fn func_reg(&self, i: i32) -> i32 {
        self.i_first_reg + self.n_column() + i
    }
}

/// `struct Column`
#[derive(Debug, Clone, Default)]
pub struct Column {
    /// Name of this column
    pub z_cn_name: String,
    /// An OE_ code for handling a NOT NULL constraint
    pub not_null: u8,
    /// One of the standard types (`COLTYPE_*`)
    pub e_c_type: u8,
    /// One of the SQLITE_AFF_... values
    pub affinity: u8,
    /// Est size of value in this column. sizeof(INT)==1
    pub sz_est: u8,
    /// 1-based index of DEFAULT. 0 means "none"
    pub i_dflt: u16,
    /// Boolean properties (`COLFLAG_*`)
    pub col_flags: u16,
    /// The declared type (stored after zCnName in C when COLFLAG_HASTYPE)
    pub z_type: Option<String>,
    /// The collating sequence name (stored after the type in C when COLFLAG_HASCOLL)
    pub z_coll: Option<String>,
}

/// `struct Table` as far as the V1 code generator reads it. Tables come from one adapter
/// over the engine's schema (see [`Host`]); later steps add indexes, triggers, checks, ….
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub z_name: String,
    pub a_col: Vec<Column>,
    /// Root BTree page for this table
    pub tnum: u32,
    pub n_tab_ref: u32,
    /// Mask of TF_* values
    pub tab_flags: u32,
    /// If not negative, use aCol[iPKey] as the rowid
    pub i_p_key: i16,
    /// Number of columns that are not VIRTUAL
    pub n_nv_col: i16,
    pub n_row_log_est: LogEst,
    pub sz_tab_row: LogEst,
    pub key_conf: u8,
    /// 0: normal, 1: virtual, 2: view (`TABTYP_*`)
    pub e_tab_type: u8,
    /// `u.view.pSelect`
    pub p_select: SelectId,
    /// Index of the schema (database) that contains this table
    pub i_db: i32,
}

impl Table {
    #[inline]
    pub fn n_col(&self) -> i16 {
        self.a_col.len() as i16
    }
}

/// `struct Walker`: context of a tree walk. The union `u` of C is the set of optional fields
/// below; each walker uses the ones its callbacks need.
#[derive(Clone)]
pub struct Walker {
    /// Callback for expressions
    pub x_expr_callback: Option<fn(&mut Parse, &mut Walker, ExprId) -> i32>,
    /// Callback for SELECTs
    pub x_select_callback: Option<fn(&mut Parse, &mut Walker, SelectId) -> i32>,
    /// Second callback for SELECTs
    pub x_select_callback2: Option<fn(&mut Parse, &mut Walker, SelectId)>,
    /// `pParse==0` in C for walkers that must not use the parser (see `exprNodeIsConstant`)
    pub has_parse: bool,
    /// Number of subqueries
    pub walker_depth: i32,
    /// A small processing code
    pub e_code: u16,
    /// Use-dependent flags
    pub m_w_flags: u16,
    /// `u.pNC`
    pub p_nc: NcId,
    /// `u.n` / `u.iCur` / `u.sz`
    pub n: i32,
    /// `u.pSrcList`
    pub p_src_list: SrcListId,
    /// `u.pGroupBy`
    pub p_group_by: ExprListId,
    /// `u.pSelect`
    pub p_select: SelectId,
}

impl Default for Walker {
    fn default() -> Self {
        Walker {
            x_expr_callback: None,
            x_select_callback: None,
            x_select_callback2: None,
            has_parse: true,
            walker_depth: 0,
            e_code: 0,
            m_w_flags: 0,
            p_nc: NcId::NULL,
            n: 0,
            p_src_list: SrcListId::NULL,
            p_group_by: ExprListId::NULL,
            p_select: SelectId::NULL,
        }
    }
}

/// One database of the connection (`struct Db` + the parts of `Schema` / `Btree` read by the
/// code generator).
#[derive(Debug, Clone, Default)]
pub struct DbInfo {
    /// `zDbSName`: "main", "temp" or the ATTACH name
    pub z_db_s_name: String,
    /// `pBt!=0`: the database file has been opened (the temp database is opened lazily)
    pub has_bt: bool,
    /// `sqlite3BtreeSharable(pBt)`
    pub sharable: bool,
    /// `sqlite3BtreeIsReadonly(pBt)`
    pub read_only: bool,
    /// `pSchema->schema_cookie`
    pub schema_cookie: i32,
    /// `pSchema->iGeneration`
    pub i_generation: i32,
}

/// What the code generator asks of the engine: the schema adapter and the registries of
/// application-defined functions and collations.
pub trait Host {
    /// Application-defined functions named `name` (`db->aFunc`), any arity. Built-in functions
    /// are known to the code generator itself.
    fn user_functions(&self, name: &str) -> Vec<Arc<FuncDef>>;
    /// The registered name of the application-defined collating sequence `name`
    /// (`sqlite3FindCollSeq`; the lookup ignores ASCII case). BINARY, NOCASE and RTRIM are
    /// known to the code generator itself.
    fn find_collation(&self, name: &str) -> Option<String>;
}

/// A [`Host`] with no application-defined functions or collations.
pub struct NoHost;
impl Host for NoHost {
    fn user_functions(&self, _name: &str) -> Vec<Arc<FuncDef>> {
        Vec::new()
    }
    fn find_collation(&self, _name: &str) -> Option<String> {
        None
    }
}

/// The fields of `sqlite3 *db` the code generator reads.
pub struct Db<'a> {
    /// `flags`: flags settable by pragmas (`SQLITE_ShortColNames`, `SQLITE_DqsDML`, …)
    pub flags: u64,
    /// `mDbFlags`: `DBFLAG_*`
    pub m_db_flags: u32,
    /// `dbOptFlags`: disabled optimizations
    pub db_opt_flags: u32,
    /// `enc`: text encoding
    pub enc: u8,
    /// `aLimit[]`
    pub a_limit: [i32; SQLITE_N_LIMIT as usize],
    /// `init.busy`
    pub init_busy: bool,
    /// `autoCommit`
    pub auto_commit: bool,
    /// `aDb[]` / `nDb`
    pub a_db: Vec<DbInfo>,
    /// The engine side
    pub host: &'a dyn Host,
}

impl<'a> Db<'a> {
    /// A connection with C's defaults: UTF-8, default limits, one opened "main" database and a
    /// not yet opened "temp" database, all optimizations enabled.
    pub fn new(host: &'a dyn Host) -> Db<'a> {
        let mut a_limit = [0i32; SQLITE_N_LIMIT as usize];
        a_limit[SQLITE_LIMIT_LENGTH as usize] = 1_000_000_000;
        a_limit[SQLITE_LIMIT_SQL_LENGTH as usize] = 1_000_000_000;
        a_limit[SQLITE_LIMIT_COLUMN as usize] = 2000;
        a_limit[SQLITE_LIMIT_EXPR_DEPTH as usize] = 1000;
        a_limit[SQLITE_LIMIT_COMPOUND_SELECT as usize] = 500;
        a_limit[SQLITE_LIMIT_VDBE_OP as usize] = 250_000_000;
        a_limit[SQLITE_LIMIT_FUNCTION_ARG as usize] = 1000;
        a_limit[SQLITE_LIMIT_ATTACHED as usize] = 10;
        a_limit[SQLITE_LIMIT_LIKE_PATTERN_LENGTH as usize] = 50000;
        a_limit[SQLITE_LIMIT_VARIABLE_NUMBER as usize] = 32766;
        a_limit[SQLITE_LIMIT_TRIGGER_DEPTH as usize] = 1000;
        a_limit[SQLITE_LIMIT_WORKER_THREADS as usize] = 0;
        a_limit[SQLITE_LIMIT_PARSER_DEPTH as usize] = 2500;
        Db {
            flags: SQLITE_ShortColNames | SQLITE_EnableTrigger | SQLITE_EnableView | SQLITE_CacheSpill
                | SQLITE_TrustedSchema | SQLITE_DqsDML | SQLITE_DqsDDL | SQLITE_AutoIndex | SQLITE_Comments,
            m_db_flags: 0,
            db_opt_flags: 0,
            enc: sqliters_vdbe::vdbeaux::SQLITE_UTF8,
            a_limit,
            init_busy: false,
            auto_commit: true,
            a_db: vec![
                DbInfo { z_db_s_name: "main".into(), has_bt: true, ..DbInfo::default() },
                DbInfo { z_db_s_name: "temp".into(), has_bt: false, ..DbInfo::default() },
            ],
            host,
        }
    }
    /// `OptimizationDisabled(db, mask)`
    #[inline]
    pub fn optimization_disabled(&self, mask: u64) -> bool {
        self.db_opt_flags as u64 & mask != 0
    }
    /// `OptimizationEnabled(db, mask)`
    #[inline]
    pub fn optimization_enabled(&self, mask: u64) -> bool {
        self.db_opt_flags as u64 & mask == 0
    }
}

/// Result codes used inside the code generator.
pub const SQLITE_OK: i32 = 0;
pub const SQLITE_ERROR: i32 = 1;
pub const SQLITE_NOMEM: i32 = 7;
pub const SQLITE_TOOBIG: i32 = 18;
pub const SQLITE_DONE: i32 = 101;

/// `struct Parse`: the state of the parser and code generator for one statement, and the
/// owner of the parse-tree arenas.
pub struct Parse<'a> {
    /// The main database structure
    pub db: Db<'a>,
    /// An error message
    pub z_err_msg: Option<String>,
    /// `db->errByteOffset` of the error (-1: none)
    pub err_byte_offset: i32,
    /// An engine for executing database bytecode (`pVdbe`); `has_vdbe` is `pVdbe!=0`
    pub v: Vdbe,
    pub has_vdbe: bool,
    /// Return code from execution
    pub rc: i32,
    /// Est number of iterations of a query (10*log2(N))
    pub n_query_loop: LogEst,
    /// Number of nested calls to the parser/code generator
    pub nested: u8,
    /// Number of temporary registers in aTempReg[]
    pub n_temp_reg: u8,
    /// True if statement may modify/insert multiple rows
    pub is_multi_write: bool,
    /// SQLITE_PREPARE_* flags
    pub prep_flags: u8,
    /// Nesting level for RIGHT JOIN body subroutines
    pub within_rj_subrtn: u8,
    /// mini Bloom filter on available SubrtnSig.selId
    pub m_subrtn_sig: u8,
    /// True if statement may throw an ABORT exception
    pub may_abort: bool,
    /// Need to invoke convertCompoundSelectToSubquery()
    pub has_compound: bool,
    /// Has a correlated "EXISTS (SELECT ....)" expression
    pub b_has_exists: bool,
    /// TRUE after OP_ColumnName has been issued to pVdbe
    pub col_names_set: bool,
    /// True if statement contains WITH
    pub b_has_with: bool,
    /// OK to factor out constants
    pub ok_const_factor: bool,
    /// Causes schema cookie check after an error
    pub check_schema: bool,
    /// Size of the temporary register block
    pub n_range_reg: i32,
    /// First register in temporary register block
    pub i_range_reg: i32,
    /// Number of errors seen
    pub n_err: i32,
    /// Number of previously allocated VDBE cursors
    pub n_tab: i32,
    /// Number of memory cells used so far
    pub n_mem: i32,
    /// Table associated with an index on expr, or negative of the base register during check-constraint eval
    pub i_self_tab: i32,
    /// Number of nested SELECT statements and/or VIEWs
    pub n_nest_sel: i32,
    /// Constant expressions
    pub p_const_expr: ExprListId,
    /// Start a write transaction on these databases
    pub write_mask: u32,
    /// Bitmask of schema verified databases
    pub cookie_mask: u32,
    /// Max args to xUpdate and xFilter vtab methods
    pub n_max_arg: i32,
    /// Number of SELECT stmts. Counter for Select.selId
    pub n_select: i32,
    /// Table triggers are being coded for
    pub p_trigger_tab: TableId,
    /// Holding area for temporary registers
    pub a_temp_reg: [i32; 8],
    /// The last token parsed
    pub s_last_token: Token,
    /// Number of '?' variables seen in the SQL so far
    pub n_var: i32,
    /// Variable Number Bitmap Cache
    pub a_vnbmc: [u64; SQLITE_VNBMC as usize],
    /// True if the EXPLAIN flag is found on the query
    pub explain: u8,
    /// PARSE_MODE_XXX constant
    pub e_parse_mode: u8,
    /// Expression tree height of current sub-select
    pub n_height: i32,
    /// Address of current OP_Explain opcode
    pub addr_explain: i32,
    /// Mapping between variable names and numbers
    pub p_v_list: VList,
    /// Current WITH clause, or NULL
    pub p_with: WithId,

    // ---- the statement text (C: zTail points at it; token offsets are relative to it) ----
    pub z_sql: String,
    /// `pParse->zTail` after `sqlite3RunParser()`: byte offset of the first character past
    /// the statement
    pub z_tail: usize,

    // ---- arenas (index 0 of each is the NULL object) ----
    pub a_expr: Vec<Expr>,
    pub a_list: Vec<ExprList>,
    pub a_select: Vec<Select>,
    pub a_src: Vec<SrcList>,
    pub a_idlist: Vec<IdList>,
    pub a_with: Vec<With>,
    pub a_win: Vec<Window>,
    pub a_agg: Vec<AggInfo>,
    pub a_tab: Vec<Table>,
    /// `NameContext` objects (C allocates them on the stack)
    pub a_nc: Vec<NameContext>,
    /// Collating sequences handed out so far (so that equal names share one object)
    pub a_coll: Vec<Arc<CollSeq>>,
    /// Functions handed out so far, by (lower-case name, nArg of the FuncDef)
    pub a_func: Vec<Arc<FuncDef>>,
    /// `SubrtnSig` objects of the coded IN subroutines (C finds them by scanning the program
    /// for OP_BeginSubrtn with P4_SUBRTNSIG; the P4 copy is updated when `bComplete` is set)
    pub a_subrtn_sig_addr: Vec<i32>,

    /// Verification aid: when `Some`, the tree dumps of [`crate::treedump`] are collected here.
    pub tree_dump: Option<Vec<String>>,
    /// `db->suppressErr`: do not record error messages (while an ORDER BY term of a compound
    /// SELECT is tentatively resolved)
    pub suppress_err: bool,

    /// Set when the statement needs a part of C that is not ported yet: the statement is then
    /// handled by the engine's old code. The text names the first missing piece.
    pub unsupported: Option<&'static str>,
}

impl<'a> Parse<'a> {
    pub fn new(db: Db<'a>, z_sql: &str) -> Parse<'a> {
        Parse {
            db,
            z_err_msg: None,
            err_byte_offset: -1,
            v: Vdbe::new(),
            has_vdbe: false,
            rc: SQLITE_OK,
            n_query_loop: 0,
            nested: 0,
            n_temp_reg: 0,
            is_multi_write: false,
            prep_flags: 0,
            within_rj_subrtn: 0,
            m_subrtn_sig: 0,
            may_abort: false,
            has_compound: false,
            b_has_exists: false,
            col_names_set: false,
            b_has_with: false,
            ok_const_factor: false,
            check_schema: false,
            n_range_reg: 0,
            i_range_reg: 0,
            n_err: 0,
            n_tab: 0,
            n_mem: 0,
            i_self_tab: 0,
            n_nest_sel: 0,
            p_const_expr: ExprListId::NULL,
            write_mask: 0,
            cookie_mask: 0,
            n_max_arg: 0,
            n_select: 0,
            p_trigger_tab: TableId::NULL,
            a_temp_reg: [0; 8],
            s_last_token: Token::EMPTY,
            n_var: 0,
            a_vnbmc: [0; SQLITE_VNBMC as usize],
            explain: 0,
            e_parse_mode: PARSE_MODE_NORMAL as u8,
            n_height: 0,
            addr_explain: 0,
            p_v_list: VList::default(),
            p_with: WithId::NULL,
            z_sql: z_sql.to_owned(),
            z_tail: 0,
            a_expr: vec![Expr::default()],
            a_list: vec![ExprList::default()],
            a_select: vec![Select::default()],
            a_src: vec![SrcList::default()],
            a_idlist: vec![IdList::default()],
            a_with: vec![With::default()],
            a_win: vec![Window::default()],
            a_agg: vec![AggInfo::default()],
            a_tab: vec![Table::default()],
            a_nc: vec![NameContext::default()],
            a_coll: Vec::new(),
            a_func: Vec::new(),
            a_subrtn_sig_addr: Vec::new(),
            tree_dump: None,
            suppress_err: false,
            unsupported: None,
        }
    }

    // ---- arena access ----
    #[inline]
    pub fn e(&self, id: ExprId) -> &Expr {
        &self.a_expr[id.ix()]
    }
    #[inline]
    pub fn em(&mut self, id: ExprId) -> &mut Expr {
        debug_assert!(id.nn());
        &mut self.a_expr[id.ix()]
    }
    #[inline]
    pub fn l(&self, id: ExprListId) -> &ExprList {
        &self.a_list[id.ix()]
    }
    #[inline]
    pub fn lm(&mut self, id: ExprListId) -> &mut ExprList {
        debug_assert!(id.nn());
        &mut self.a_list[id.ix()]
    }
    #[inline]
    pub fn s(&self, id: SelectId) -> &Select {
        &self.a_select[id.ix()]
    }
    #[inline]
    pub fn sm(&mut self, id: SelectId) -> &mut Select {
        debug_assert!(id.nn());
        &mut self.a_select[id.ix()]
    }
    #[inline]
    pub fn src(&self, id: SrcListId) -> &SrcList {
        &self.a_src[id.ix()]
    }
    #[inline]
    pub fn srcm(&mut self, id: SrcListId) -> &mut SrcList {
        debug_assert!(id.nn());
        &mut self.a_src[id.ix()]
    }
    #[inline]
    pub fn nc(&self, id: NcId) -> &NameContext {
        &self.a_nc[id.ix()]
    }
    #[inline]
    pub fn ncm(&mut self, id: NcId) -> &mut NameContext {
        debug_assert!(id.nn());
        &mut self.a_nc[id.ix()]
    }
    #[inline]
    pub fn tab(&self, id: TableId) -> &Table {
        &self.a_tab[id.ix()]
    }
    #[inline]
    pub fn agg(&self, id: AggInfoId) -> &AggInfo {
        &self.a_agg[id.ix()]
    }
    #[inline]
    pub fn aggm(&mut self, id: AggInfoId) -> &mut AggInfo {
        debug_assert!(id.nn());
        &mut self.a_agg[id.ix()]
    }
    /// Allocate a new expression node.
    pub fn new_expr(&mut self, e: Expr) -> ExprId {
        self.a_expr.push(e);
        ExprId(self.a_expr.len() as u32 - 1)
    }
    pub fn new_list(&mut self, l: ExprList) -> ExprListId {
        self.a_list.push(l);
        ExprListId(self.a_list.len() as u32 - 1)
    }
    pub fn new_select(&mut self, s: Select) -> SelectId {
        self.a_select.push(s);
        SelectId(self.a_select.len() as u32 - 1)
    }
    pub fn new_src(&mut self, s: SrcList) -> SrcListId {
        self.a_src.push(s);
        SrcListId(self.a_src.len() as u32 - 1)
    }
    pub fn new_nc(&mut self, nc: NameContext) -> NcId {
        self.a_nc.push(nc);
        NcId(self.a_nc.len() as u32 - 1)
    }
    pub fn new_agg(&mut self, a: AggInfo) -> AggInfoId {
        self.a_agg.push(a);
        AggInfoId(self.a_agg.len() as u32 - 1)
    }

    /// The text of token `t` ("" for an empty or NULL token).
    pub fn tok_str(&self, t: &Token) -> &str {
        if t.z == Token::NO_Z || t.n == 0 {
            ""
        } else {
            let a = t.z as usize;
            let b = (a + (t.n & 0x7fff_ffff) as usize).min(self.z_sql.len());
            &self.z_sql[a..b]
        }
    }

    /// `sqlite3ErrorMsg(pParse, zFormat, ...)` with an already formatted message and no token
    /// argument: the error has no byte offset.
    pub fn error_msg(&mut self, msg: impl Into<String>) {
        self.error_msg_ofs(msg, -1);
    }
    /// `sqlite3ErrorMsg()` whose format contains `%T` for token `t`: the byte offset of the
    /// error is the start of the token (`sqlite3RecordErrorByteOffset`).
    pub fn error_msg_tok(&mut self, msg: impl Into<String>, t: &Token) {
        let ofs = if t.z != Token::NO_Z && t.n != 0 && (t.z as usize) < self.z_sql.len() {
            t.z as i32
        } else {
            -1
        };
        self.error_msg_ofs(msg, ofs);
    }
    /// `sqlite3ErrorMsg()` whose format contains `%#T` for expression `e`
    /// (`sqlite3RecordErrorOffsetOfExpr`).
    pub fn error_msg_expr(&mut self, msg: impl Into<String>, e: ExprId) {
        self.error_msg_ofs(msg, -1);
        self.record_error_offset_of_expr(e);
    }
    fn error_msg_ofs(&mut self, msg: impl Into<String>, ofs: i32) {
        // db->errByteOffset = -2; ... if( db->errByteOffset<-1 ) db->errByteOffset = -1;
        self.err_byte_offset = ofs;
        if self.suppress_err {
            // db->suppressErr: the message is dropped and nErr is not incremented
            return;
        }
        self.n_err += 1;
        self.z_err_msg = Some(msg.into());
        self.rc = SQLITE_ERROR;
        self.p_with = WithId::NULL;
    }
    /// `sqlite3RecordErrorOffsetOfExpr(db, pExpr)`
    pub fn record_error_offset_of_expr(&mut self, mut e: ExprId) {
        while e.nn() {
            let x = self.e(e);
            if x.has(EP_Reduced | EP_TokenOnly) {
                return;
            }
            if x.has(EP_OuterON | EP_InnerON) || x.w <= 0 {
                e = x.p_left;
            } else {
                break;
            }
        }
        if e.is_null() {
            return;
        }
        if self.e(e).has(EP_FromDDL) {
            return;
        }
        self.err_byte_offset = self.e(e).w;
    }

    /// The statement needs something that is not ported yet. Code generation is abandoned and
    /// the engine's old path handles the statement. (Counts as an error so that the C-style
    /// `if( pParse->nErr ) return` checks unwind quickly; the caller looks at `unsupported`
    /// before it looks at the error.)
    pub fn not_supported(&mut self, what: &'static str) {
        if self.unsupported.is_none() {
            self.unsupported = Some(what);
        }
        self.n_err += 1;
        if self.rc == SQLITE_OK || self.rc == SQLITE_DONE {
            self.rc = SQLITE_ERROR;
        }
    }

    /// `IN_RENAME_OBJECT`
    #[inline]
    pub fn in_rename_object(&self) -> bool {
        self.e_parse_mode >= PARSE_MODE_RENAME as u8
    }

    /// `ENC(db)`
    #[inline]
    pub fn enc(&self) -> u8 {
        self.db.enc
    }
}
