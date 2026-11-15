//! `whereInt.h`: the structures of the query planner (`where.c`, `whereexpr.c`,
//! `wherecode.c`).
//!
//! C links these objects with pointers into arrays that are reallocated as they grow. Here a
//! `WhereClause` lives in the arena `WhereInfo::a_wc` (index 0 is `pWInfo->sWC`; the
//! sub-clauses of OR and AND terms follow) and a `WhereTerm *` is a [`TermRef`] — the clause
//! and the index of the term in it. A `WhereLoop *` is an index into `WhereInfo::loops`
//! (C's linked list `pWInfo->pLoops`, in list order).
#![allow(non_upper_case_globals)]

use crate::sqliteint::*;

/// Index of a `WhereClause` in `WhereInfo::a_wc`.
pub type WcId = usize;

/// A `WhereTerm *`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TermRef {
    pub wc: u32,
    pub i: u32,
}

impl TermRef {
    #[inline]
    pub fn new(wc: WcId, i: usize) -> TermRef {
        TermRef { wc: wc as u32, i: i as u32 }
    }
}

// WhereTerm.wtFlags
pub const TERM_DYNAMIC: u16 = 0x0001; // Need to call sqlite3ExprDelete(db, pExpr)
pub const TERM_VIRTUAL: u16 = 0x0002; // Added by the optimizer. Do not code
pub const TERM_CODED: u16 = 0x0004; // This term is already coded
pub const TERM_COPIED: u16 = 0x0008; // Has a child
pub const TERM_ORINFO: u16 = 0x0010; // Need to free the WhereTerm.u.pOrInfo object
pub const TERM_ANDINFO: u16 = 0x0020; // Need to free the WhereTerm.u.pAndInfo obj
pub const TERM_OK: u16 = 0x0040; // Used during OR-clause processing
pub const TERM_VNULL: u16 = 0x0080; // Manufactured x>NULL or x<=NULL term
pub const TERM_LIKEOPT: u16 = 0x0100; // Virtual terms from the LIKE optimization
pub const TERM_LIKECOND: u16 = 0x0200; // Conditionally this LIKE operator term
pub const TERM_LIKE: u16 = 0x0400; // The original LIKE operator
pub const TERM_IS: u16 = 0x0800; // Term.pExpr is an IS operator
pub const TERM_VARSELECT: u16 = 0x1000; // Term.pExpr contains a correlated sub-query
pub const TERM_HEURTRUTH: u16 = 0x2000; // Heuristic truthProb used
pub const TERM_HIGHTRUTH: u16 = 0; // Only used with STAT4
pub const TERM_SLICE: u16 = 0x8000; // One slice of a row-value/vector comparison

// Bitmasks for the operators on WhereTerm objects (WhereTerm.eOperator)
pub const WO_IN: u16 = 0x0001;
pub const WO_EQ: u16 = 0x0002;
pub const WO_LT: u16 = WO_EQ << (crate::tk::TK_LT - crate::tk::TK_EQ);
pub const WO_LE: u16 = WO_EQ << (crate::tk::TK_LE - crate::tk::TK_EQ);
pub const WO_GT: u16 = WO_EQ << (crate::tk::TK_GT - crate::tk::TK_EQ);
pub const WO_GE: u16 = WO_EQ << (crate::tk::TK_GE - crate::tk::TK_EQ);
pub const WO_AUX: u16 = 0x0040; // Op useful to virtual tables only
pub const WO_IS: u16 = 0x0080;
pub const WO_ISNULL: u16 = 0x0100;
pub const WO_OR: u16 = 0x0200; // Two or more OR-connected terms
pub const WO_AND: u16 = 0x0400; // Two or more AND-connected terms
pub const WO_EQUIV: u16 = 0x0800; // Of the form A==B, both columns
pub const WO_NOOP: u16 = 0x1000; // This term does not restrict search space
pub const WO_ROWVAL: u16 = 0x2000; // A row-value term
pub const WO_ALL: u16 = 0x3fff; // Mask of all possible WO_* values
pub const WO_SINGLE: u16 = 0x01ff; // Mask of all non-compound WO_* values

/// `N_OR_COST`
pub const N_OR_COST: usize = 3;
/// `SQLITE_QUERY_PLANNER_LIMIT`
pub const SQLITE_QUERY_PLANNER_LIMIT: u32 = 20000;
/// `SQLITE_QUERY_PLANNER_LIMIT_INCR`
pub const SQLITE_QUERY_PLANNER_LIMIT_INCR: u32 = 1000;

/// Allowed values for WhereLoopBuider.bldFlags
pub const SQLITE_BLDF1_INDEXED: u8 = 0x0001; // An index is used
pub const SQLITE_BLDF1_UNIQUE: u8 = 0x0002; // All keys of a UNIQUE index used
pub const SQLITE_BLDF2_2NDPASS: u8 = 0x0004; // Second builder pass needed

/// `struct WhereOrInfo`: a WhereTerm with eOperator==WO_OR has its u.pOrInfo pointer set to
/// information for future processing.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhereOrInfo {
    /// Decomposition into subterms
    pub wc: WcId,
    /// Bitmask of all indexable tables in the clause
    pub indexable: Bitmask,
}

/// `struct WhereTerm`: the query generator uses an array of instances of this structure to
/// help it analyze the subexpressions of the WHERE clause. Each WHERE clause subexpression is
/// separated from the others by AND operators, usually, or sometimes subexpressions separated
/// by OR.
#[derive(Debug, Clone, Default)]
pub struct WhereTerm {
    /// Pointer to the subexpression that is this term
    pub p_expr: ExprId,
    /// Probability of truth for this expression
    pub truth_prob: LogEst,
    /// TERM_xxx bit flags. See below
    pub wt_flags: u16,
    /// A WO_xx value describing <op>
    pub e_operator: u16,
    /// Number of children that must disable us
    pub n_child: u8,
    /// Op for vtab MATCH/LIKE/GLOB/REGEXP terms
    pub e_match_op: u8,
    /// Disable pWC->a[iParent] when this term disabled
    pub i_parent: i32,
    /// Cursor number of X in "X <op> <expr>"
    pub left_cursor: i32,
    /// `u.x.leftColumn`: column number of X in "X <op> <expr>"
    pub left_column: i32,
    /// `u.x.iField`: field in (?,?,?) IN (SELECT...) vector
    pub i_field: i32,
    /// `u.pOrInfo`: extra information if (eOperator & WO_OR)!=0
    pub p_or_info: Option<WhereOrInfo>,
    /// `u.pAndInfo`: extra information if (eOperator& WO_AND)!=0 (the clause of the subterms)
    pub p_and_info: Option<WcId>,
    /// Bitmask of tables used by pExpr->pRight
    pub prereq_right: Bitmask,
    /// Bitmask of tables referenced by pExpr
    pub prereq_all: Bitmask,
}

/// `struct WhereClause`: an instance of the following structure holds all information about a
/// WHERE clause. Mostly this is a collection of separate WhereTerms.
#[derive(Debug, Clone, Default)]
pub struct WhereClause {
    /// Outer conjunction
    pub p_outer: Option<WcId>,
    /// Split operator. TK_AND or TK_OR
    pub op: u8,
    /// True if any a[].eOperator is WO_OR
    pub has_or: bool,
    /// Number of terms through the last non-Virtual
    pub n_base: i32,
    /// Each a[] describes a term of the WHERE clause
    pub a: Vec<WhereTerm>,
}

impl WhereClause {
    #[inline]
    pub fn n_term(&self) -> i32 {
        self.a.len() as i32
    }
}

/// `struct WhereMaskSet`: an instance of the following structure keeps track of a mapping
/// between VDBE cursor numbers and bits of the bitmasks in WhereTerm.
#[derive(Debug, Clone)]
pub struct WhereMaskSet {
    /// Used by sqlite3WhereExprUsage()
    pub b_var_select: bool,
    /// Number of assigned cursor values
    pub n: i32,
    /// Cursor assigned to each bit
    pub ix: [i32; BMS as usize],
}

impl Default for WhereMaskSet {
    fn default() -> Self {
        WhereMaskSet { b_var_select: false, n: 0, ix: [0; BMS as usize] }
    }
}

/// `struct WhereScan`: an instance of the WhereScan object is used as an iterator for
/// locating terms in the WHERE clause that are useful to the query planner.
#[derive(Debug, Clone, Default)]
pub struct WhereScan {
    /// Original, innermost WhereClause
    pub p_orig_wc: WcId,
    /// WhereClause currently being scanned
    pub p_wc: Option<WcId>,
    /// Required collating sequence, if not NULL
    pub z_coll_name: Option<String>,
    /// Search for this index expression
    pub p_idx_expr: ExprId,
    /// Resume scanning at this->pWC->a[this->k]
    pub k: i32,
    /// Acceptable operators
    pub op_mask: u32,
    /// Must match this affinity, if zCollName!=NULL
    pub idxaff: u8,
    /// Current slot in aiCur[] and aiColumn[]
    pub i_equiv: u8,
    /// Number of entries in aiCur[] and aiColumn[]
    pub n_equiv: u8,
    /// Cursors in the equivalence class
    pub ai_cur: [i32; 11],
    /// Corresponding column number in the eq-class
    pub ai_column: [i16; 11],
}

/// `struct WhereLoop`: each instance of this object represents an algorithm for evaluating
/// one term of a join. Every term of the FROM clause will have at least one such WhereLoop
/// object.
#[derive(Debug, Clone, Default)]
pub struct WhereLoop {
    /// Bitmask of other loops that must run first
    pub prereq: Bitmask,
    /// Bitmask identifying table iTab
    pub mask_self: Bitmask,
    /// Position in FROM clause of table for this loop
    pub i_tab: u8,
    /// Sorting index number. 0==None
    pub i_sort_idx: u8,
    /// One-time setup cost (ex: create transient index)
    pub r_setup: LogEst,
    /// Cost of running each loop
    pub r_run: LogEst,
    /// Estimated number of output rows
    pub n_out: LogEst,
    /// `u.btree.nEq`: number of equality constraints
    pub n_eq: u16,
    /// `u.btree.nBtm`: size of BTM vector
    pub n_btm: u16,
    /// `u.btree.nTop`: size of TOP vector
    pub n_top: u16,
    /// `u.btree.nDistinctCol`: index columns used to sort for DISTINCT
    pub n_distinct_col: u16,
    /// `u.btree.pIndex`: index used, or NULL
    pub p_index: IndexId,
    /// `u.btree.pOrderBy`: ORDER BY clause if this is really a subquery
    pub p_order_by: ExprListId,
    /// WHERE_* flags describing the plan
    pub ws_flags: u32,
    /// Number of NULL aLTerm[] entries
    pub n_skip: u16,
    /// WhereTerms used (`nLTerm` is the length)
    pub a_l_term: Vec<Option<TermRef>>,
    // ---- `u.vtab` (C keeps it in a union with `u.btree`) ----
    /// `u.vtab.idxNum`: index number
    pub vt_idx_num: i32,
    /// `u.vtab.needFree`: true if sqlite3_free(idxStr) is needed
    pub vt_need_free: bool,
    /// `u.vtab.bOmitOffset`: True to let virtual table handle offset
    pub vt_b_omit_offset: bool,
    /// `u.vtab.bIdxNumHex`: Show idxNum as hex in EXPLAIN QUERY PLAN
    pub vt_b_idx_num_hex: bool,
    /// `u.vtab.isOrdered`: True if satisfies ORDER BY
    pub vt_is_ordered: i8,
    /// `u.vtab.omitMask`: Terms that may be omitted
    pub vt_omit_mask: u16,
    /// `u.vtab.idxStr`: index identifier string
    pub vt_idx_str: Option<String>,
    /// `u.vtab.mHandleIn`: Terms to handle as IN(...) instead of ==
    pub vt_m_handle_in: u32,
}

impl WhereLoop {
    #[inline]
    pub fn n_l_term(&self) -> usize {
        self.a_l_term.len()
    }
}

/// `struct WhereOrCost`: this object holds the prerequisites and the cost of running a
/// subquery on one operand of an OR operator in the WHERE clause.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhereOrCost {
    /// Prerequisites
    pub prereq: Bitmask,
    /// Cost of running this subquery
    pub r_run: LogEst,
    /// Number of outputs for this subquery
    pub n_out: LogEst,
}

/// `struct WhereOrSet`: the WhereOrSet object holds a set of possible WhereOrCosts that
/// correspond to the subquery(s) of OR-clause processing. Only the best N_OR_COST elements
/// are retained.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhereOrSet {
    /// Number of valid a[] entries
    pub n: u16,
    /// Set of best costs
    pub a: [WhereOrCost; N_OR_COST],
}

/// `struct WherePath`: each instance of this object holds a sequence of WhereLoop objects
/// that implement some or all of a query plan.
#[derive(Debug, Clone, Default)]
pub struct WherePath {
    /// Bitmask of all WhereLoop objects in this path
    pub mask_loop: Bitmask,
    /// aLoop[]s that should be reversed for ORDER BY
    pub rev_loop: Bitmask,
    /// Estimated number of rows generated by this path
    pub n_row: LogEst,
    /// Total cost of this path
    pub r_cost: LogEst,
    /// Total cost of this path ignoring sorting costs
    pub r_unsort: LogEst,
    /// No. of ORDER BY terms satisfied. -1 for unknown
    pub is_ordered: i8,
    /// Array of WhereLoop objects implementing this path
    pub a_loop: Vec<usize>,
}

/// `struct InLoop`: information about each nested IN operator of a loop.
#[derive(Debug, Clone, Copy, Default)]
pub struct InLoop {
    /// The VDBE cursor used by this IN operator
    pub i_cur: i32,
    /// Top of the IN loop
    pub addr_in_top: i32,
    /// Base register of multi-key index record
    pub i_base: i32,
    /// Number of prior entries in the key
    pub n_prefix: i32,
    /// IN Loop terminator. OP_Next or OP_Prev
    pub e_end_loop_op: u8,
}

/// `struct WhereRightJoin`: extra information attached to a WhereLevel that is a RIGHT JOIN.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhereRightJoin {
    /// Cursor used to determine prior matched rows
    pub i_match: i32,
    /// Bloom filter for iRJMatch
    pub reg_bloom: i32,
    /// Return register for the interior subroutine
    pub reg_return: i32,
    /// Starting address for the interior subroutine
    pub addr_subrtn: i32,
    /// The last opcode in the interior subroutine
    pub end_subrtn: i32,
}

/// `struct WhereLevel`: this object contains information needed to implement a single nested
/// loop in WHERE clause.
#[derive(Debug, Clone, Default)]
pub struct WhereLevel {
    /// Memory cell used to implement LEFT OUTER JOIN
    pub i_left_join: i32,
    /// The VDBE cursor used to access the table
    pub i_tab_cur: i32,
    /// The VDBE cursor used to access pIdx
    pub i_idx_cur: i32,
    /// Jump here to break out of the loop
    pub addr_brk: i32,
    /// Abort the query due to empty table or similar
    pub addr_halt: i32,
    /// Jump here to start the next IN combination
    pub addr_nxt: i32,
    /// Jump here for next iteration of skip-scan
    pub addr_skip: i32,
    /// Jump here to continue with the next loop cycle
    pub addr_cont: i32,
    /// First instruction of interior of the loop
    pub addr_first: i32,
    /// Beginning of the body of this loop
    pub addr_body: i32,
    /// big-null flag reg. True if a NULL-scan is needed
    pub reg_bignull: i32,
    /// Jump here for next part of big-null scan
    pub addr_bignull: i32,
    /// LIKE range processing counter register (times 2)
    pub i_like_rep_cntr: u32,
    /// LIKE range processing address
    pub addr_like_rep: i32,
    /// Bloom filter
    pub reg_filter: i32,
    /// Extra information for RIGHT JOIN
    pub p_rj: Option<WhereRightJoin>,
    /// Which entry in the FROM clause
    pub i_from: u8,
    /// Opcode, P3 & P5 of the opcode that ends the loop
    pub op: u8,
    pub p3: u8,
    pub p5: u8,
    /// Operands of the opcode used to end the loop
    pub p1: i32,
    pub p2: i32,
    /// `u.in.aInLoop`: information about each nested IN operator
    pub a_in_loop: Vec<InLoop>,
    /// `u.pCoveringIdx`: possible covering index for WHERE_MULTI_OR
    pub p_covering_idx: IndexId,
    /// The selected WhereLoop object
    pub p_w_loop: usize,
    /// FROM entries not usable at this level
    pub not_ready: Bitmask,
}

/// `struct WhereLoopBuilder`: this object is a convenience wrapper holding all information
/// needed to construct WhereLoop objects for a particular query.
#[derive(Debug, Clone, Default)]
pub struct WhereLoopBuilder {
    /// WHERE clause terms
    pub p_wc: WcId,
    /// Template WhereLoop
    pub p_new: WhereLoop,
    /// Record best loops here, if not NULL
    pub p_or_set: Option<WhereOrSet>,
    /// First set of SQLITE_BLDF_* flags
    pub bld_flags1: u8,
    /// Second set of SQLITE_BLDF_* flags
    pub bld_flags2: u8,
    /// Search limiter
    pub i_plan_limit: u32,
}

/// `struct WhereInfo`: the WHERE clause processing routine has two halves. The first part
/// does the start of the WHERE loop and the second half does the tail of the WHERE loop. An
/// instance of this structure is returned by the first half and passed into the second half
/// to give some continuity.
#[derive(Debug, Clone, Default)]
pub struct WhereInfo {
    /// List of tables in the join
    pub p_tab_list: SrcListId,
    /// The ORDER BY clause or NULL
    pub p_order_by: ExprListId,
    /// Result set of the query
    pub p_result_set: ExprListId,
    /// The entire SELECT statement containing WHERE
    pub p_select: SelectId,
    /// OP_OpenWrite cursors for the ONEPASS opt
    pub ai_cur_one_pass: [i32; 2],
    /// Jump here to continue with next record
    pub i_continue: i32,
    /// Jump here to break out of the loop
    pub i_break: i32,
    /// pParse->nQueryLoop outside the WHERE loop
    pub saved_n_query_loop: i32,
    /// Flags originally passed to sqlite3WhereBegin()
    pub wctrl_flags: u16,
    /// LIMIT if wctrlFlags has WHERE_USE_LIMIT
    pub i_limit: LogEst,
    /// Number of nested loop
    pub n_level: u8,
    /// Number of ORDER BY terms satisfied by indices
    pub n_ob_sat: i8,
    /// ONEPASS_OFF, or _SINGLE, or _MULTI
    pub e_one_pass: u8,
    /// One of the WHERE_DISTINCT_* values
    pub e_distinct: u8,
    /// Uses OP_DeferredSeek
    pub b_deferred_seek: bool,
    /// Not all WHERE terms resolved by outer loop
    pub untested_terms: bool,
    /// True if only the inner-most loop is ordered
    pub b_ordered_inner_loop: bool,
    /// True if really sorted (not just grouped)
    pub sorted: bool,
    /// True if check for star-query is complete
    pub b_star_done: bool,
    /// True if star-query heuristic is used
    pub b_star_used: bool,
    /// Estimated number of output rows
    pub n_row_out: LogEst,
    /// The very beginning of the WHERE loop
    pub i_top: i32,
    /// End of the WHERE clause itself
    pub i_end_where: i32,
    /// List of all WhereLoop objects (`pLoops`, in list order)
    pub loops: Vec<WhereLoop>,
    /// Mask of ORDER BY terms that need reversing
    pub rev_mask: Bitmask,
    /// Decomposition of the WHERE clause: `a_wc[0]` is `sWC`
    pub a_wc: Vec<WhereClause>,
    /// Map cursor numbers to bitmasks
    pub s_mask_set: WhereMaskSet,
    /// Information about each nest loop in WHERE
    pub a: Vec<WhereLevel>,
    /// Star-query heuristic: cost deltas of the loops (`rStarDelta`, indexed like `loops`)
    pub star_delta: Vec<LogEst>,
}

impl WhereInfo {
    #[inline]
    pub fn term(&self, t: TermRef) -> &WhereTerm {
        &self.a_wc[t.wc as usize].a[t.i as usize]
    }
    #[inline]
    pub fn term_mut(&mut self, t: TermRef) -> &mut WhereTerm {
        &mut self.a_wc[t.wc as usize].a[t.i as usize]
    }
    /// `sqlite3WhereOutputRowCount(pWInfo)`: return the estimated number of output rows from
    /// a WHERE clause.
    pub fn output_row_count(&self) -> LogEst {
        self.n_row_out
    }
    /// `sqlite3WhereIsDistinct(pWInfo)`: return one of the WHERE_DISTINCT_xxxxx values to
    /// indicate how this WHERE clause handles the DISTINCT keyword.
    pub fn is_distinct(&self) -> i32 {
        self.e_distinct as i32
    }
    /// `sqlite3WhereIsOrdered(pWInfo)`: return the number of ORDER BY terms that are
    /// satisfied by the WHERE clause. A return of 0 means that the output must be completely
    /// sorted. A return equal to the number of ORDER BY terms means that no sorting is
    /// needed at all. A return that is positive but less than the number of ORDER BY terms
    /// means that block sorting is required.
    pub fn is_ordered(&self) -> i32 {
        if self.n_ob_sat < 0 { 0 } else { self.n_ob_sat as i32 }
    }
    /// `sqlite3WhereContinueLabel(pWInfo)`: return the VDBE address or label to jump to in
    /// order to continue immediately to the next iteration of the WHERE loop.
    pub fn continue_label(&self) -> i32 {
        self.i_continue
    }
    /// `sqlite3WhereBreakLabel(pWInfo)`: return the VDBE address or label to jump to in order
    /// to break out of a WHERE loop.
    pub fn break_label(&self) -> i32 {
        self.i_break
    }
    /// `sqlite3WhereIsSorted(pWInfo)`: return TRUE if the WHERE loop uses the OP_DeferredSeek
    /// opcode ... no: return TRUE if rows are really sorted (not just grouped).
    pub fn is_sorted(&self) -> bool {
        self.sorted
    }
    /// `sqlite3WhereUsesDeferredSeek(pWInfo)`
    pub fn uses_deferred_seek(&self) -> bool {
        self.b_deferred_seek
    }
}
