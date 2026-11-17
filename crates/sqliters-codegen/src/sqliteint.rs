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
define_id!(IndexId);
define_id!(UpsertId);
define_id!(TriggerId);

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
    /// C token bytes when an internal string bound is not valid UTF-8.
    pub z_token_raw: Option<Box<[u8]>>,
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
    pub fn tok_bytes(&self) -> &[u8] {
        self.z_token_raw.as_deref().unwrap_or_else(|| self.tok().as_bytes())
    }
    pub fn set_token_bytes(&mut self, bytes: Vec<u8>) {
        match String::from_utf8(bytes) {
            Ok(text) => { self.z_token = Some(text); self.z_token_raw = None; }
            Err(error) => {
                self.z_token = Some(String::from_utf8_lossy(error.as_bytes()).into_owned());
                self.z_token_raw = Some(error.into_bytes().into_boxed_slice());
            }
        }
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
    /// `u2.pIBIndex`: index structure corresponding to u1.zIndexedBy
    pub p_ib_index: IndexId,
    /// `u2.pCteUse`: CTE usage info when `is_cte` is true (index into `Parse::a_cte_use`, +1)
    pub p_cte_use: u32,
    /// `u4.pSchema` when `fixed_schema`: index of the database the item is fixed to
    pub i_fixed_db: i32,
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
    /// `uNC.pUpsert`: the ON CONFLICT clause information (NC_UUpsert)
    pub p_upsert: UpsertId,
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
    /// `pUse`: usage information for this CTE (index into `Parse::a_cte_use`, +1; 0 = NULL)
    pub p_use: u32,
}

/// `struct CteUse`: bookkeeping about all uses of one CTE of a query.
#[derive(Debug, Clone, Default)]
pub struct CteUse {
    /// Number of users of this CTE
    pub n_use: i32,
    /// Start of subroutine to compute materialization
    pub addr_m9e: i32,
    /// Return address register for addrM9e subroutine
    pub reg_rtn: i32,
    /// Ephemeral table holding the materialization
    pub i_cur: i32,
    /// Estimated number of rows in the table
    pub n_row_est: LogEst,
    /// The MATERIALIZED flag
    pub e_m10d: u8,
}

/// `struct IndexedExpr`: an expression that is the value of a column of an index in use
/// (`Parse.pIdxEpr`) or a column constrained by a partial-index WHERE (`Parse.pIdxPartExpr`).
#[derive(Debug, Clone, Default)]
pub struct IndexedExpr {
    /// The expression contained in the index
    pub p_expr: ExprId,
    /// The data cursor associated with the index
    pub i_data_cur: i32,
    /// The index cursor
    pub i_idx_cur: i32,
    /// The index column that contains value of pExpr
    pub i_idx_col: i32,
    /// True if we need an OP_IfNullRow check
    pub b_maybe_null_row: bool,
    /// Affinity of the pExpr expression
    pub aff: u8,
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

/// `Window.ppThis`: where the pointer to this object in a `Select.pWin` list lives (C keeps a
/// `Window**`; here the owner of that pointer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WinPpThis {
    /// `ppThis==0`: not linked into a `Select.pWin` list
    #[default]
    None,
    /// `&pSel->pWin`
    Select(SelectId),
    /// `&pPrev->pNextWin`
    Win(WindowId),
}

/// `struct Window`
#[derive(Debug, Clone, Default)]
pub struct Window {
    /// Name of window (may be NULL)
    pub z_name: Option<String>,
    /// Name of base window for chaining (may be NULL)
    pub z_base: Option<String>,
    /// PARTITION BY clause
    pub p_partition: ExprListId,
    /// ORDER BY clause
    pub p_order_by: ExprListId,
    /// TK_RANGE, TK_GROUPS, TK_ROWS, or 0
    pub e_frm_type: u8,
    /// UNBOUNDED, CURRENT, PRECEDING or FOLLOWING
    pub e_start: u8,
    /// UNBOUNDED, CURRENT, PRECEDING or FOLLOWING
    pub e_end: u8,
    /// True if frame was implicitly specified
    pub b_implicit_frame: u8,
    /// TK_NO, TK_CURRENT, TK_TIES, TK_GROUP, or 0
    pub e_exclude: u8,
    /// Expression for "<expr> PRECEDING"
    pub p_start: ExprId,
    /// Expression for "<expr> FOLLOWING"
    pub p_end: ExprId,
    /// Pointer to this object in Select.pWin list
    pub pp_this: WinPpThis,
    /// Next window function belonging to this SELECT
    pub p_next_win: WindowId,
    /// The FILTER expression
    pub p_filter: ExprId,
    /// The function
    pub p_w_func: Option<Arc<FuncDef>>,
    /// Partition buffer or Peer buffer
    pub i_eph_csr: i32,
    /// Accumulator
    pub reg_accum: i32,
    /// Interim result
    pub reg_result: i32,
    /// Function cursor (used by min/max)
    pub csr_app: i32,
    /// Function register (also used by min/max)
    pub reg_app: i32,
    /// Array of registers for PARTITION BY values
    pub reg_part: i32,
    /// Expression object this window is attached to
    pub p_owner: ExprId,
    /// Number of columns in buffer table
    pub n_buffer_col: i32,
    /// Offset of first argument for this function
    pub i_arg_col: i32,
    /// Register containing constant value 1
    pub reg_one: i32,
    pub reg_start_rowid: i32,
    pub reg_end_rowid: i32,
    /// Defer evaluation of window function arguments due to the SQLITE_SUBTYPE flag
    pub b_expr_args: u8,
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

/// `struct FKey`: each foreign key constraint of a table (the child table).
#[derive(Debug, Clone, Default)]
pub struct FKey {
    /// `zTo`: name of the table that the key points to (aka: parent)
    pub z_to: String,
    /// `aCol[]`: (iFrom: index of column in the child table, zCol: name of the column in zTo,
    /// NULL to use the PRIMARY KEY)
    pub a_col: Vec<(i32, Option<String>)>,
    /// True if constraint checking is deferred till COMMIT
    pub is_deferred: bool,
    /// ON DELETE and ON UPDATE actions, respectively
    pub a_action: [u8; 2],
}

/// `struct Upsert`: the ON CONFLICT clause of an UPSERT (upsert.c).
#[derive(Debug, Clone, Default)]
pub struct Upsert {
    /// Optional description of conflict target
    pub p_upsert_target: ExprListId,
    /// WHERE clause for partial index targets
    pub p_upsert_target_where: ExprId,
    /// The SET clause from an ON CONFLICT UPDATE
    pub p_upsert_set: ExprListId,
    /// WHERE clause for the ON CONFLICT UPDATE
    pub p_upsert_where: ExprId,
    /// Next ON CONFLICT clause in the list
    pub p_next_upsert: UpsertId,
    /// True for DO UPDATE. False for DO NOTHING
    pub is_do_update: bool,
    /// True if 2nd or later with same pUpsertIdx
    pub is_dup: bool,
    /// UNIQUE constraint specified by pUpsertTarget
    pub p_upsert_idx: IndexId,
    /// Table to be updated
    pub p_upsert_src: SrcListId,
    /// First register of range holding data
    pub reg_data: i32,
    /// Index of the data cursor
    pub i_data_cur: i32,
    /// Index of the first index cursor
    pub i_idx_cur: i32,
}

/// `struct TriggerStep`: one step of a trigger program (or the RETURNING pseudo-step).
#[derive(Debug, Clone, Default)]
pub struct TriggerStep {
    /// One of TK_DELETE, TK_UPDATE, TK_INSERT, TK_SELECT, TK_RETURNING
    pub op: u8,
    /// OE_Rollback etc.
    pub orconf: u8,
    /// SELECT statement or RHS of INSERT INTO SELECT ...
    pub p_select: SelectId,
    /// Table to insert into, update or delete from (with the FROM clause of UPDATE)
    pub p_src: SrcListId,
    /// The WHERE clause for DELETE or UPDATE steps
    pub p_where: ExprId,
    /// SET clause for UPDATE, or RETURNING clause
    pub p_expr_list: ExprListId,
    /// Column names for INSERT
    pub p_id_list: IdListId,
    /// Upsert clauses on an INSERT
    pub p_upsert: UpsertId,
    /// Original SQL text of this command
    pub z_span: Option<String>,
}

/// `struct Trigger`: a trigger (or the RETURNING pseudo-trigger `retTrig`).
#[derive(Debug, Clone, Default)]
pub struct Trigger {
    /// The name of the trigger
    pub z_name: Option<String>,
    /// The table or view to which the trigger applies
    pub table: String,
    /// One of TK_DELETE, TK_UPDATE, TK_INSERT (TK_RETURNING until first use)
    pub op: u8,
    /// One of TRIGGER_BEFORE, TRIGGER_AFTER
    pub tr_tm: u8,
    /// This trigger implements a RETURNING clause
    pub b_returning: bool,
    /// The WHEN clause of the expression (may be NULL)
    pub p_when: ExprId,
    /// If this is an UPDATE OF <column-list> trigger, the <column-list> is stored here
    pub p_columns: IdListId,
    /// `pSchema`: schema containing the trigger (index into `db->aDb[]`)
    pub i_schema: i32,
    /// `pTabSchema`: schema containing the table (index into `db->aDb[]`, -1: none)
    pub i_tab_schema: i32,
    /// Link list of trigger program steps
    pub step_list: Vec<TriggerStep>,
}

/// `struct Returning`: information about a RETURNING clause (`pParse->u1.d.pReturning`).
#[derive(Debug, Clone, Default)]
pub struct Returning {
    /// List of expressions to return
    pub p_return_el: ExprListId,
    /// The RETURNING trigger (`retTrig`)
    pub ret_trig: TriggerId,
    /// Transient table holding RETURNING results
    pub i_ret_cur: i32,
    /// Number of in pReturnEL after expansion
    pub n_ret_col: i32,
    /// Register array for holding a row of RETURNING
    pub i_ret_reg: i32,
}

/// `struct AutoincInfo`: information about an AUTOINCREMENT table.
#[derive(Debug, Clone, Default)]
pub struct AutoincInfo {
    /// Table this info block refers to
    pub p_tab: TableId,
    /// Index in sqlite3.aDb[] of database holding pTab
    pub i_db: i32,
    /// Memory register holding the rowid counter
    pub reg_ctr: i32,
}

/// `struct TriggerPrg`: a trigger sub-program coded for one (trigger, ON CONFLICT) pair.
#[derive(Debug, Clone)]
pub struct TriggerPrg {
    /// Trigger this program was coded from
    pub p_trigger: TriggerId,
    /// Default ON CONFLICT policy
    pub orconf: i32,
    /// Program implementing pTrigger/orconf (index into `Parse::a_sub_program`)
    pub i_program: usize,
    /// Masks of old.*, new.* columns accessed
    pub a_colmask: [u32; 2],
}

/// The in-memory schema of one database (`struct Schema`: `tblHash`, `idxHash`) as the
/// generator loads it (`build_tab::sqlite3_init`).
#[derive(Debug, Clone, Default)]
pub struct SchemaState {
    /// `DB_SchemaLoaded`
    pub loaded: bool,
    /// `tblHash`: all tables and views, by lower-case name
    pub tbl_hash: std::collections::HashMap<String, TableId>,
    /// The iteration order of C's `tblHash` (hash.c buckets; REINDEX and ANALYZE walk it)
    pub tbl_order: crate::trigger::CHash<TableId>,
    /// `idxHash`: all (named) indices, by lower-case name
    pub idx_hash: std::collections::HashMap<String, IndexId>,
    /// Objects whose SQL text the generator could not load (a construct that is not ported):
    /// lower-case name of the object and of its table -> the reason reported
    pub bad: std::collections::HashMap<String, &'static str>,
    /// Lower-case names of the tables that have triggers (rows of type "trigger" seen so far)
    pub triggers: Vec<String>,
    /// `pSeqTab`: the sqlite_sequence table used by AUTOINCREMENT
    pub p_seq_tab: TableId,
    /// `trigHash`: the triggers of this schema (C's hash: iteration order and buckets)
    pub trig_hash: crate::trigger::TrigHash,
    /// Number of "trigger" rows of sqlite_schema seen while loading
    pub n_trigger_rows: usize,
    /// Triggers whose SQL text the generator could not load: lower-case table name -> reason
    pub bad_trig: std::collections::HashMap<String, &'static str>,
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
    /// Index of the schema (database) that contains this table (`pSchema`; -1: none)
    pub i_db: i32,
    /// `pIndex`: the indexes of the table in the order of C's linked list
    pub p_index: Vec<IndexId>,
    /// `pCheck` of a view: the explicit column names of `CREATE VIEW v(a,b,..)`
    pub p_check: ExprListId,
    pub has_trigger: bool,
    /// `u.tab.pDfltList`: DEFAULT clauses on various columns, or the AS clauses of generated
    /// columns (`Column.iDflt` is an index into this list, plus one)
    pub p_dflt_list: ExprListId,
    /// `u.tab.pFKey`: linked list of all foreign keys in this table, most recent first
    pub p_fkey: Vec<FKey>,
    /// C's `nCol<0` marker of a view: its column names are being computed, or its SELECT is
    /// being expanded (detects circular view definitions)
    pub view_busy: bool,
    /// `u.vtab.azArg[]` of a virtual table (`u.vtab.nArg` is the length): the module name, the
    /// database name (set by the constructor), the table name and the module arguments
    pub vtab_args: Vec<Option<String>>,
    /// `u.vtab.p`: the connection's `VTable` of a virtual table, once connected
    pub vtab: Option<VTable>,
    /// `pTrigger`: list of triggers on this object, most recent first
    pub p_trigger: Vec<TriggerId>,
    /// `u.tab.addColOffset`: offset in CREATE TABLE stmt to add a new column
    pub add_col_offset: i32,
}

/// `struct VTable`, the part the code generator reads: the instance of a virtual table that
/// the connection uses (`sqlite3GetVTable()`). The module object itself stays on the engine
/// side; `id` names it.
#[derive(Debug, Clone, Default)]
pub struct VTable {
    /// The identity of the `sqlite3_vtab` instance (`P4_VTAB` lists it as `vtab:%p`)
    pub id: u64,
    /// `pMod->zName`
    pub module_name: String,
    /// `bConstraint`: true if constraints are supported
    pub b_constraint: bool,
    /// `bAllSchemas`: true if might use any attached schema
    pub b_all_schemas: bool,
    /// `eVtabRisk`: riskiness of allowing hacker access (`SQLITE_VTABRISK_*`)
    pub e_vtab_risk: u8,
    /// `pMod->pModule->xFindFunction!=0`
    pub has_find_function: bool,
    /// `pMod->pModule->xUpdate!=0`
    pub has_update: bool,
    /// `pModule->xRename!=0` (None: the engine does not know for this module)
    pub has_rename: Option<bool>,
}

/// `SQLITE_VTABRISK_*`: allowed values for VTable.eVtabRisk
pub const SQLITE_VTABRISK_Low: u8 = 0;
pub const SQLITE_VTABRISK_Normal: u8 = 1;
pub const SQLITE_VTABRISK_High: u8 = 2;

/// A virtual-table module of the connection (`Module` / `sqlite3_module`), as far as the code
/// generator needs it.
#[derive(Debug, Clone, Default)]
pub struct VtabModule {
    /// `zName`: the name the module was registered with
    pub name: String,
    /// `xCreate==0 || xCreate==xConnect`: the module can have an eponymous table
    pub eponymous: bool,
    /// The table-name suffixes for which `xShadowName` returns true (empty if the module has
    /// no `xShadowName` or `iVersion<3`)
    pub shadow_names: Vec<String>,
}

/// The outcome of [`Host::vtab_connect`] (`xConnect` through `vtabCallConstructor()`).
#[derive(Debug, Clone)]
pub enum VtabConnect {
    /// The constructor succeeded: the instance and the text it passed to
    /// `sqlite3_declare_vtab()` (`None`: it declared no schema)
    Ok { vtab: VTable, declare: Option<String> },
    /// The constructor failed with this message (C's `*pzErr`)
    Err(String),
    /// The engine cannot connect the table for the generator
    Unsupported,
}

impl Table {
    #[inline]
    pub fn n_col(&self) -> i16 {
        self.a_col.len() as i16
    }
    /// `HasRowid(X)`
    #[inline]
    pub fn has_rowid(&self) -> bool {
        self.tab_flags & TF_WithoutRowid == 0
    }
    /// `VisibleRowid(X)`
    #[inline]
    pub fn visible_rowid(&self) -> bool {
        self.tab_flags & TF_NoVisibleRowid == 0
    }
    /// `IsView(X)`
    #[inline]
    pub fn is_view(&self) -> bool {
        self.e_tab_type == TABTYP_VIEW as u8
    }
    /// `IsVirtual(X)`
    #[inline]
    pub fn is_virtual(&self) -> bool {
        self.e_tab_type == TABTYP_VTAB as u8
    }
    /// `IsOrdinaryTable(X)`
    #[inline]
    pub fn is_ordinary(&self) -> bool {
        self.e_tab_type == TABTYP_NORM as u8
    }
}

/// `struct Index`: an SQL index (or the PRIMARY KEY of a WITHOUT ROWID table, or an automatic
/// index built by the query planner).
#[derive(Debug, Clone, Default)]
pub struct Index {
    /// Name of this index
    pub z_name: String,
    /// Which columns are used by this index. 1st is 0 (`XN_ROWID`, `XN_EXPR`)
    pub ai_column: Vec<i16>,
    /// From ANALYZE: Est. rows selected by each column
    pub ai_row_log_est: Vec<LogEst>,
    /// The SQL table being indexed
    pub p_table: TableId,
    /// String defining the affinity of each column (computed on first use)
    pub z_col_aff: Option<Vec<u8>>,
    /// for each column: True==DESC, False==ASC
    pub a_sort_order: Vec<u8>,
    /// Array of collation sequence names for index
    pub az_coll: Vec<String>,
    /// For each entry of `az_coll`: true if C's pointer is the static `sqlite3StrBINARY`
    /// (the KeyInfo then has a NULL collation, shown as an empty name in EXPLAIN)
    pub a_coll_static: Vec<bool>,
    /// WHERE clause for partial indices
    pub p_part_idx_where: ExprId,
    /// Column expressions
    pub a_col_expr: ExprListId,
    /// DB Page containing root of this index
    pub tnum: u32,
    /// Estimated average row size in bytes
    pub sz_idx_row: LogEst,
    /// Number of columns forming the key
    pub n_key_col: u16,
    /// Nr columns in btree. Can be 2*Table.nCol
    pub n_column: u16,
    /// OE_Abort, OE_Ignore, OE_Replace, or OE_None
    pub on_error: u8,
    /// 0:Normal 1:UNIQUE, 2:PRIMARY KEY, 3:IPK
    pub idx_type: u8,
    /// Use this index for == or IN queries only
    pub b_unordered: bool,
    /// True if UNIQUE and NOT NULL for all columns
    pub uniq_not_null: bool,
    /// True if this is a covering index
    pub is_covering: bool,
    /// Do not try to use skip-scan if true
    pub no_skip_scan: bool,
    /// aiRowLogEst values come from sqlite_stat1
    pub has_stat1: bool,
    /// Do not use this index to optimize queries
    pub b_no_query: bool,
    /// True if the bba7b69f9849b5bf bug applies
    pub b_asc_key_bug: bool,
    /// `isResized`: resizeIndexObject() reallocated the arrays (WITHOUT ROWID conversion). The
    /// new `aiRowLogEst[]` has nColumn entries, so C's read of `aiRowLogEst[nColumn]` lands on
    /// `aiColumn[0]`, which follows it in the same allocation.
    pub is_resized: bool,
    /// `pSchema`: the database of the index (index into `db->aDb[]`)
    pub i_db: i32,
    /// Index references one or more VIRTUAL columns
    pub b_has_v_col: bool,
    /// Index contains an expression, either a literal expression or a reference to a VIRTUAL column
    pub b_has_expr: bool,
    /// Unindexed columns in pTab
    pub col_not_idxed: Bitmask,
}

impl Index {
    /// `IsPrimaryKeyIndex(X)`
    #[inline]
    pub fn is_primary_key(&self) -> bool {
        self.idx_type == SQLITE_IDXTYPE_PRIMARYKEY as u8
    }
    /// `IsUniqueIndex(X)`
    #[inline]
    pub fn is_unique(&self) -> bool {
        self.on_error != OE_None as u8
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
    /// `u.pIdxCover->pIdx` / `u.pCovIdxCk->pIdx`: an index
    pub p_idx: IndexId,
    /// Second integer of a callback-specific structure (`CoveringIndexCheck.iTabCur`, …)
    pub i2: i32,
    /// Flags of a callback-specific structure (`CoveringIndexCheck.bExpr` / `bUnidx`, …)
    pub b1: bool,
    pub b2: bool,
    /// `u.pExpr` / a table of a callback-specific structure
    pub p_expr: ExprId,
    pub p_tab: TableId,
    /// An expression array of a callback-specific structure (`WhereConst.apExpr`, …)
    pub v_expr: Vec<ExprId>,
    /// A context stack of a callback-specific structure (`CheckOnCtx` chain: pSrc, iJoin,
    /// bFuncArg; `SubstContext`-like integer triples)
    pub v_ctx: Vec<(SrcListId, i32, bool)>,
    /// A flag mask of a callback-specific structure (`WhereConst.mExcludeOn`, …)
    pub m: u32,
    /// An integer array of a callback-specific structure (`u.aiCol`: the cursor map of
    /// renumberCursors())
    pub v_int: Vec<i32>,
    /// `u.pRewrite->pWin` (window.c `WindowRewrite`)
    pub p_win: WindowId,
    /// `u.pRewrite->pSub` (window.c `WindowRewrite`)
    pub p_sub: ExprListId,
    /// `xSelectCallback2` is `sqlite3WalkWinDefnDummyCallback` or `sqlite3SelectPopWith`:
    /// sqlite3WalkSelectExpr() also walks `Select.pWinDefn`
    pub walk_win_defn: bool,
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
            p_idx: IndexId::NULL,
            i2: 0,
            b1: false,
            b2: false,
            p_expr: ExprId::NULL,
            p_tab: TableId::NULL,
            v_expr: Vec::new(),
            v_ctx: Vec::new(),
            m: 0,
            v_int: Vec::new(),
            p_win: WindowId::NULL,
            p_sub: ExprListId::NULL,
            walk_win_defn: false,
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
    /// `pSchema->file_format`
    pub file_format: u8,
}

/// What the code generator asks of the engine: the schema adapter and the registries of
/// application-defined functions and collations.
pub trait Host {
    /// sqlite3VdbeGetBoundValue(pReprepare, iVar, SQLITE_AFF_BLOB).
    /// Initial preparation has no previous statement; NULL is returned as None.
    fn bound_value(&self, _i_var: usize) -> Option<sqliters_types::Value> { None }

    /// A connection/pager value read at prepare time by pragma.c. `None` means that
    /// the host does not represent this state; the generator must not approximate it.
    fn pragma_int(&self, _i_db: usize, _name: &str) -> Option<i64> {
        None
    }
    /// A lossless UTF-8 prepare-time pager string. None leaves the handler unsupported.
    fn pragma_text(&self, _i_db: usize, _name: &str) -> Option<String> { None }
    /// C's live aCollSeq hash order, including entries whose comparator was removed.
    fn pragma_collations(&self) -> Option<Vec<String>> { None }
    /// pragma.c changes these values during prepare, including under EXPLAIN.
    fn note_pragma_effect(&self, _effect: crate::pragma::PrepareEffect) {}
    /// `sqlite3ReadSchema()` -> `sqlite3Init()` did work: C's connection has its schemas loaded
    /// from now on, also when the statement fails afterwards (the engine mirrors that state).
    fn note_schema_loaded(&self) {}
    /// `sqlite3OpenTempDatabase()` opened the temp database (`db->aDb[1].pBt`) while the
    /// statement was prepared: C's connection has it open from now on.
    fn note_temp_opened(&self) {}
    /// The history of C's `Schema.tblHash` of database `i_db` since its last load when it is
    /// not a fresh load of the current objects (see `DbSlot::c_tbl_hash` of the engine): the
    /// names of that load in rowid order, then the names inserted (`true`) and removed.
    /// C's ALTER TABLE ran `renameTestSchema()` after its last load of the statistics of
    /// database `i_db`: the tables with a CREATE INDEX have `nRowLogEst` of at least 99.
    fn rowest_clamped(&self, _i_db: usize) -> bool {
        false
    }
    /// The tables of database `i_db` whose `nRowLogEst` a CREATE INDEX statement raised to 99
    /// (`sqlite3DefaultRowEst()` on C's live table) after C last read the statistics.
    fn rowest_clamped_tables(&self, _i_db: usize) -> Vec<String> {
        Vec::new()
    }
    /// The prepare of a CREATE INDEX statement raised the `nRowLogEst` of C's live table
    /// `table` of database `i_db` to 99 (see [`Host::rowest_clamped_tables`]).
    fn note_rowest_clamped(&self, _i_db: usize, _table: &str) {}
    fn tbl_hash_log(&self, _i_db: usize) -> Option<(Vec<String>, Vec<(bool, String)>)> {
        None
    }
    /// Application-defined functions named `name` (`db->aFunc`), any arity. Built-in functions
    /// are known to the code generator itself.
    fn user_functions(&self, name: &str) -> Vec<Arc<FuncDef>>;
    /// The registered name of the application-defined collating sequence `name`
    /// (`sqlite3FindCollSeq`; the lookup ignores ASCII case). BINARY, NOCASE and RTRIM are
    /// known to the code generator itself.
    fn find_collation(&self, name: &str) -> Option<String>;
    /// The rows of `sqlite_schema` of database `i_db` in rowid order ("SELECT*FROM
    /// sqlite_schema ORDER BY rowid" of `sqlite3InitOne()`): the generator builds its schema
    /// objects from the SQL text of these rows exactly as `sqlite3InitCallback()` does.
    fn schema_rows(&self, _i_db: usize) -> Vec<crate::build_tab::SchemaRow> {
        Vec::new()
    }
    /// The rows of `sqlite_stat1` of database `i_db` (tbl, idx, stat) in the order C reads
    /// them ("SELECT tbl,idx,stat FROM sqlite_stat1" of `sqlite3AnalysisLoad()`).
    fn stat1_rows(&self, _i_db: usize) -> Vec<(String, Option<String>, String)> {
        Vec::new()
    }
    /// A reason why statements that use table `name` of database `i_db` must be reported as
    /// unsupported: state of C's connection that the schema text does not show (statistics C
    /// keeps from an earlier ANALYZE).
    fn table_unsupported(&self, _i_db: usize, _name: &str) -> Option<&'static str> {
        None
    }
    /// True if table `name` (lower case) of database `i_db` was created by a CREATE TABLE
    /// statement of this connection after C last ran `sqlite3AnalysisLoad()` on that database
    /// (schema load or ANALYZE): C's Index objects of the table then still have the row
    /// estimates `sqlite3CreateIndex()` computed, which differ from the ones of a schema load
    /// when `convertToWithoutRowidTable()` removed redundant PRIMARY KEY columns afterwards.
    fn created_since_load(&self, _i_db: usize, _name: &str) -> bool {
        false
    }
    /// `sqlite3HashFind(&db->aModule, zName)`: the virtual-table module registered under
    /// `name` (ASCII case is ignored). With `lazy`, also the modules `sqlite3LocateTable()`
    /// registers on first use (`sqlite3PragmaVtabRegister()`, `sqlite3JsonVtabRegister()`).
    fn vtab_module(&self, _name: &str, _lazy: bool) -> Option<VtabModule> {
        None
    }
    /// `vtabCallConstructor(db, pTab, pMod, pMod->pModule->xConnect, &zErr)`: connect the
    /// virtual table `table` of database `i_db` (an eponymous table when `eponymous`) built
    /// with module `module` and the arguments `args` (`azArg[]`, the database name already
    /// in `args[1]`).
    fn vtab_connect(&self, _i_db: usize, _table: &str, _module: &str, _args: &[String], _eponymous: bool) -> VtabConnect {
        VtabConnect::Unsupported
    }
    /// `pVtab->pModule->xBestIndex(pVtab, p)` of the instance `vtab_id`: the return code
    /// (`SQLITE_OK`, `SQLITE_CONSTRAINT`, ...) and `pVtab->zErrMsg`. `hidden` answers the
    /// `sqlite3_vtab_in()` / `_rhs_value()` / `_distinct()` / `_collation()` calls the module
    /// makes during the call.
    fn vtab_best_index(
        &self,
        _vtab_id: u64,
        _info: &mut crate::vtab::IndexInfo,
        _hidden: &mut dyn crate::vtab::IndexInfoHidden,
    ) -> (i32, Option<String>) {
        (1, Some("virtual table not supported".to_owned()))
    }
    /// `pMod->xFindFunction(pVtab, nArg, zName, &xSFunc, &pArg)` of the instance `vtab_id`: 0
    /// if the module does not overload the function, else its return value.
    fn vtab_find_function(&self, _vtab_id: u64, _n_arg: i32, _name: &str) -> i32 {
        0
    }
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
    /// sqlite3.errByteOffset, retained when prepare fails without sqlite3ErrorMsg.
    pub err_byte_offset: i32,
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
    /// True if C's connection would still have to read the schema (`sqlite3Init()` runs at the
    /// first `sqlite3ReadSchema()` of the statement and resets `db->errByteOffset`)
    pub schema_not_loaded: bool,
    /// `db->mDbFlags & DBFLAG_EncodingFixed`: some `sqlite_schema` row was read by this
    /// connection. While clear, `cmd ::= select` calls sqlite3ReadSchema() itself.
    pub encoding_fixed: bool,
    /// `PRAGMA case_sensitive_like = x` was run on the connection: like() was re-registered
    /// (`sqlite3RegisterLikeFunctions`)
    pub case_sensitive_like: bool,
    /// `xAuth!=0`: an authorizer callback is registered
    pub x_auth: bool,
    /// `nAnalysisLimit`: PRAGMA analysis_limit
    pub n_analysis_limit: i32,
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
        a_limit[SQLITE_LIMIT_TRIGGER_STEPS as usize] = 65000;
        Db {
            err_byte_offset: -1,
            flags: SQLITE_ShortColNames | SQLITE_EnableTrigger | SQLITE_EnableView | SQLITE_CacheSpill
                | SQLITE_TrustedSchema | SQLITE_DqsDML | SQLITE_DqsDDL | SQLITE_AutoIndex | SQLITE_Comments,
            m_db_flags: 0,
            db_opt_flags: 0,
            enc: sqliters_vdbe::vdbeaux::SQLITE_UTF8,
            a_limit,
            init_busy: false,
            auto_commit: true,
            a_db: vec![
                DbInfo { z_db_s_name: "main".into(), has_bt: true, file_format: 4, ..DbInfo::default() },
                DbInfo { z_db_s_name: "temp".into(), has_bt: false, file_format: 4, ..DbInfo::default() },
            ],
            schema_not_loaded: false,
            encoding_fixed: false,
            case_sensitive_like: false,
            x_auth: false,
            n_analysis_limit: 0,
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

    /// `Index` objects (schema indexes of the tables in use, automatic indexes, the fake
    /// PRIMARY KEY index of `whereLoopAddBtree`)
    pub a_idx: Vec<Index>,
    /// `CteUse` objects
    pub a_cte_use: Vec<CteUse>,
    /// `pIdxEpr`: list of expressions used by active indexes (C: linked list, newest first;
    /// here the newest is the last element)
    pub p_idx_epr: Vec<IndexedExpr>,
    /// `pIdxPartExpr`: expressions constrained by a partial index in use
    pub p_idx_part_expr: Vec<IndexedExpr>,
    /// The schema tables already converted to `Table` objects, by (database, lower-case name)
    /// The schemas of the connection as loaded by `sqlite3Init()` (one per `db->aDb[]`)
    pub a_schema: Vec<SchemaState>,
    /// `pNewTable`: a table being constructed by CREATE TABLE
    pub p_new_table: TableId,
    /// `u1.cr.constraintName`: name of the constraint currently being parsed
    pub cr_constraint_name: Token,
    /// `u1.cr.regRowid`: register holding rowid of CREATE TABLE entry
    pub cr_reg_rowid: i32,
    /// `u1.cr.regRoot`: register holding root page number for new objects
    pub cr_reg_root: i32,
    /// `u1.cr.addrCrTab`: address of the OP_CreateBtree opcode on CREATE TABLE
    pub cr_addr_cr_tab: i32,
    /// `DbFixer.zType` / `DbFixer.pName` of the fixer in use (non-init statements): the kind
    /// and the raw name text of the schema object for the error messages of `attach.c`
    pub fix_type: &'static str,
    pub fix_name: String,
    /// `DbFixer.pName` as a token of the statement text (`%T` records the error offset at it;
    /// EMPTY when C's token points outside the statement)
    pub fix_name_tok: Token,
    /// `iPkSortOrder`: ASC or DESC for INTEGER PRIMARY KEY
    pub i_pk_sort_order: u8,
    /// `sNameToken`: token with unqualified schema object name
    pub s_name_token: Token,
    /// `db->init.iDb`: which db file is being initialized
    pub init_i_db: usize,
    /// `db->init.newTnum`: rootpage of table being initialized
    pub init_new_tnum: u32,
    /// `db->init.azInit`: type, name and tbl_name of the sqlite_schema row being loaded
    pub init_az: [String; 3],
    /// Why the last schema row failed to load (see `build_tab::init_parse`)
    pub init_fail: Option<&'static str>,
    /// Diagnostics: the schema rows that failed to load, with the error or the missing piece
    pub init_errors: Vec<String>,
    /// `u1.d.pReturning` etc. are not ported; `nTableLock`/`aTableLock` only matter with a
    /// shared cache.
    /// Depth of nested `sqlite3Select()` calls for which `OP_Explain` nodes are open
    /// (`addrExplain` is the parent node of new `OP_Explain` rows)
    pub explain_dummy: i32,
    /// Verification aid: when `Some`, the WHERE-term and WhereLoop dumps of where.c are
    /// collected here.
    pub where_dump: Option<Vec<String>>,

    /// Verification aid: when `Some`, the tree dumps of [`crate::treedump`] are collected here.
    pub tree_dump: Option<Vec<String>>,
    /// `db->suppressErr`: do not record error messages (while an ORDER BY term of a compound
    /// SELECT is tentatively resolved)
    pub suppress_err: bool,

    /// Set when the statement needs a part of C that is not ported yet: the statement is then
    /// handled by the engine's old code. The text names the first missing piece.
    pub unsupported: Option<&'static str>,

    /// `sArg`: complete text of a module argument (CREATE VIRTUAL TABLE)
    pub s_arg: Token,
    /// `apVtabLock[]` / `nVtabLock`: pointer to virtual tables needing locking
    pub ap_vtab_lock: Vec<TableId>,
    /// `pMod->pEpoTab` of the modules of the connection: the eponymous virtual table of each
    /// module (by lower-case module name) created so far
    pub epo_tab: std::collections::HashMap<String, TableId>,
    /// `db->pVtabCtx` chain: the tables whose constructor is running
    pub vtab_ctx: Vec<TableId>,

    // ---- DML, triggers, RETURNING (step V5) ----
    /// `Upsert` objects
    pub a_upsert: Vec<Upsert>,
    /// `Trigger` objects (of the schemas and the RETURNING pseudo-trigger)
    pub a_trigger: Vec<Trigger>,
    /// `bReturning`: coding a RETURNING trigger
    pub b_returning: bool,
    /// `u1.d.pReturning`: the RETURNING clause
    pub p_returning: Option<Returning>,
    /// `pAinc`: information about AUTOINCREMENT counters (C's list order: newest first)
    pub p_ainc: Vec<AutoincInfo>,
    /// `usesAinc`: the statement uses AUTOINCREMENT
    pub uses_ainc: bool,
    /// `eTriggerOp`: TK_UPDATE, TK_INSERT or TK_DELETE
    pub e_trigger_op: u8,
    /// `eOrconf`: default ON CONFLICT policy for trigger steps
    pub e_orconf: u8,
    /// `oldmask`: mask of old.* columns referenced
    pub oldmask: u32,
    /// `newmask`: mask of new.* columns referenced
    pub newmask: u32,
    /// `disableTriggers`: true to disable triggers
    pub disable_triggers: bool,
    /// `pNewTrigger != 0`: a CREATE TRIGGER statement is being parsed
    pub p_new_trigger: bool,
    /// `pTriggerPrg`: linked list of coded triggers (newest first)
    pub p_trigger_prg: Vec<TriggerPrg>,
    /// `pToplevel != 0`: this Parse codes a trigger sub-program (see `trigger.rs`)
    pub is_nested_trigger: bool,
    /// Trigger steps built by the parser (`TriggerStep` objects; ids are index+1)
    pub tstep_pool: Vec<TriggerStep>,
    /// `trigger_cmd_list` values of the parser: lists of step ids (ids are index+1)
    pub tstep_lists: Vec<Vec<u32>>,
    /// The trigger being built by CREATE TRIGGER (`pNewTrigger`) at schema load
    pub new_trigger: Option<Trigger>,
    /// The trigger sub-programs of the statement (`TriggerPrg.pProgram`), in link order
    pub sub_programs: Vec<crate::trigger::SubProgSlot>,
    /// Number of trigger sub-parses currently open (`pOuterParse` chain length)
    pub trigger_depth: i32,
    /// `FKey.apTrigger[]` of the statement: (child table, index of the foreign key in its
    /// `p_fkey`, ON DELETE action trigger, ON UPDATE action trigger)
    pub fk_action_triggers: Vec<(TableId, usize, Option<TriggerId>, Option<TriggerId>)>,
    /// The swapped-out state of the enclosing Parses while trigger sub-programs are coded
    /// (index 0: the top-level Parse)
    pub sub_parse_stack: Vec<crate::trigger::SubParseState>,
    /// `sqlite3VdbeReprepareOnBind()` calls made where the Parse is not mutable
    /// (`exprCompareVariable()`): `expmask` bits for the current program, merged into `v`
    /// before the program changes hands
    pub pending_expmask: std::cell::Cell<u32>,
}

impl<'a> Parse<'a> {
    pub fn new(db: Db<'a>, z_sql: &str) -> Parse<'a> {
        let err_byte_offset = db.err_byte_offset;
        Parse {
            db,
            z_err_msg: None,
            err_byte_offset,
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
            a_idx: vec![Index::default()],
            a_cte_use: Vec::new(),
            p_idx_epr: Vec::new(),
            p_idx_part_expr: Vec::new(),
            a_schema: Vec::new(),
            p_new_table: TableId::NULL,
            cr_constraint_name: Token::EMPTY,
            cr_reg_rowid: 0,
            cr_reg_root: 0,
            cr_addr_cr_tab: 0,
            fix_type: "",
            fix_name: String::new(),
            fix_name_tok: Token::EMPTY,
            i_pk_sort_order: 0,
            s_name_token: Token::EMPTY,
            init_i_db: 0,
            init_new_tnum: 0,
            init_az: [String::new(), String::new(), String::new()],
            init_fail: None,
            init_errors: Vec::new(),
            explain_dummy: 0,
            where_dump: None,
            tree_dump: None,
            suppress_err: false,
            unsupported: None,
            s_arg: Token::EMPTY,
            ap_vtab_lock: Vec::new(),
            epo_tab: std::collections::HashMap::new(),
            vtab_ctx: Vec::new(),
            a_upsert: vec![Upsert::default()],
            a_trigger: vec![Trigger::default()],
            b_returning: false,
            p_returning: None,
            p_ainc: Vec::new(),
            uses_ainc: false,
            e_trigger_op: 0,
            e_orconf: 0,
            oldmask: 0,
            newmask: 0,
            disable_triggers: false,
            p_new_trigger: false,
            p_trigger_prg: Vec::new(),
            is_nested_trigger: false,
            tstep_pool: Vec::new(),
            tstep_lists: Vec::new(),
            new_trigger: None,
            sub_programs: Vec::new(),
            trigger_depth: 0,
            fk_action_triggers: Vec::new(),
            sub_parse_stack: Vec::new(),
            pending_expmask: std::cell::Cell::new(0),
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
    pub fn w(&self, id: WindowId) -> &Window {
        &self.a_win[id.ix()]
    }
    #[inline]
    pub fn wm(&mut self, id: WindowId) -> &mut Window {
        debug_assert!(id.nn());
        &mut self.a_win[id.ix()]
    }
    pub fn new_win(&mut self, w: Window) -> WindowId {
        self.a_win.push(w);
        WindowId(self.a_win.len() as u32 - 1)
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
    pub fn tabm(&mut self, id: TableId) -> &mut Table {
        debug_assert!(id.nn());
        &mut self.a_tab[id.ix()]
    }
    #[inline]
    pub fn idx(&self, id: IndexId) -> &Index {
        &self.a_idx[id.ix()]
    }
    #[inline]
    pub fn idxm(&mut self, id: IndexId) -> &mut Index {
        debug_assert!(id.nn());
        &mut self.a_idx[id.ix()]
    }
    pub fn new_tab(&mut self, t: Table) -> TableId {
        self.a_tab.push(t);
        TableId(self.a_tab.len() as u32 - 1)
    }
    pub fn new_idx(&mut self, i: Index) -> IndexId {
        self.a_idx.push(i);
        IndexId(self.a_idx.len() as u32 - 1)
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
    #[inline]
    pub fn up(&self, id: UpsertId) -> &Upsert {
        &self.a_upsert[id.ix()]
    }
    #[inline]
    pub fn upm(&mut self, id: UpsertId) -> &mut Upsert {
        debug_assert!(id.nn());
        &mut self.a_upsert[id.ix()]
    }
    pub fn new_upsert(&mut self, u: Upsert) -> UpsertId {
        self.a_upsert.push(u);
        UpsertId(self.a_upsert.len() as u32 - 1)
    }
    #[inline]
    pub fn trig(&self, id: TriggerId) -> &Trigger {
        &self.a_trigger[id.ix()]
    }
    #[inline]
    pub fn trigm(&mut self, id: TriggerId) -> &mut Trigger {
        debug_assert!(id.nn());
        &mut self.a_trigger[id.ix()]
    }
    pub fn new_trigger(&mut self, t: Trigger) -> TriggerId {
        self.a_trigger.push(t);
        TriggerId(self.a_trigger.len() as u32 - 1)
    }
    /// `sqlite3IsToplevel(pParse)`
    #[inline]
    pub fn is_toplevel(&self) -> bool {
        !self.is_nested_trigger
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

    /// `IN_DECLARE_VTAB`
    #[inline]
    pub fn in_declare_vtab(&self) -> bool {
        self.e_parse_mode == PARSE_MODE_DECLARE_VTAB as u8
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
