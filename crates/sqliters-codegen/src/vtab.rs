//! Port of the parts of `vtab.c` the code generator uses: the parser actions of CREATE VIRTUAL
//! TABLE (`sqlite3VtabBeginParse()` ... `sqlite3VtabFinishParse()`, schema load only), the
//! constructor call (`sqlite3VtabCallConnect()`, `vtabCallConstructor()`,
//! `sqlite3_declare_vtab()`), eponymous tables (`sqlite3VtabEponymousTableInit()`),
//! `sqlite3GetVTable()`, `sqlite3VtabOverloadFunction()`, `sqlite3VtabMakeWritable()`, and
//! the shadow-table helpers of build.c. The module objects live on the engine side: the
//! generator reaches them through [`crate::Host`]; `sqlite3_index_info` is [`IndexInfo`].

use sqliters_types::Value;

use crate::consts::*;
use crate::sqliteint::*;

/// `SQLITE_INDEX_CONSTRAINT_*`: virtual table constraint operator codes
pub const SQLITE_INDEX_CONSTRAINT_EQ: u8 = 2;
pub const SQLITE_INDEX_CONSTRAINT_GT: u8 = 4;
pub const SQLITE_INDEX_CONSTRAINT_LE: u8 = 8;
pub const SQLITE_INDEX_CONSTRAINT_LT: u8 = 16;
pub const SQLITE_INDEX_CONSTRAINT_GE: u8 = 32;
pub const SQLITE_INDEX_CONSTRAINT_MATCH: u8 = 64;
pub const SQLITE_INDEX_CONSTRAINT_LIKE: u8 = 65;
pub const SQLITE_INDEX_CONSTRAINT_GLOB: u8 = 66;
pub const SQLITE_INDEX_CONSTRAINT_REGEXP: u8 = 67;
pub const SQLITE_INDEX_CONSTRAINT_NE: u8 = 68;
pub const SQLITE_INDEX_CONSTRAINT_ISNOT: u8 = 69;
pub const SQLITE_INDEX_CONSTRAINT_ISNOTNULL: u8 = 70;
pub const SQLITE_INDEX_CONSTRAINT_ISNULL: u8 = 71;
pub const SQLITE_INDEX_CONSTRAINT_IS: u8 = 72;
pub const SQLITE_INDEX_CONSTRAINT_LIMIT: u8 = 73;
pub const SQLITE_INDEX_CONSTRAINT_OFFSET: u8 = 74;
pub const SQLITE_INDEX_CONSTRAINT_FUNCTION: i32 = 150;

/// `SQLITE_INDEX_SCAN_*`: virtual table scan flags
pub const SQLITE_INDEX_SCAN_UNIQUE: i32 = 0x0000_0001;
pub const SQLITE_INDEX_SCAN_HEX: i32 = 0x0000_0002;

/// Result codes of the virtual-table interface
pub const SQLITE_LOCKED: i32 = 6;
pub const SQLITE_NOTFOUND: i32 = 12;
pub const SQLITE_CONSTRAINT: i32 = 19;
pub const SQLITE_MISUSE: i32 = 21;

/// `SQLITE_BIG_DBL`
pub const SQLITE_BIG_DBL: f64 = 1e99;

/// `struct sqlite3_index_constraint`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexConstraint {
    /// Column constrained. -1 for ROWID
    pub i_column: i32,
    /// Constraint operator (`SQLITE_INDEX_CONSTRAINT_*`)
    pub op: u8,
    /// True if this constraint is usable
    pub usable: bool,
    /// Used internally - xBestIndex should ignore
    pub i_term_offset: i32,
}

/// `struct sqlite3_index_orderby`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexOrderBy {
    /// Column number
    pub i_column: i32,
    /// True for DESC. False for ASC.
    pub desc: bool,
}

/// `struct sqlite3_index_constraint_usage`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexConstraintUsage {
    /// if >0, constraint is part of argv to xFilter
    pub argv_index: i32,
    /// Do not code a test for this constraint
    pub omit: bool,
}

/// `struct sqlite3_index_info`: the object passed to xBestIndex (inputs, then outputs).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexInfo {
    /// Inputs: `aConstraint[]` (`nConstraint` is the length)
    pub a_constraint: Vec<IndexConstraint>,
    /// Inputs: `aOrderBy[]` (`nOrderBy` is the length)
    pub a_order_by: Vec<IndexOrderBy>,
    /// Outputs: `aConstraintUsage[]`
    pub a_constraint_usage: Vec<IndexConstraintUsage>,
    /// Number used to identify the index
    pub idx_num: i32,
    /// String, possibly obtained from sqlite3_malloc
    pub idx_str: Option<String>,
    /// Free idxStr using sqlite3_free() if true
    pub need_to_free_idx_str: bool,
    /// True if output is already ordered
    pub order_by_consumed: bool,
    /// Estimated cost of using this index
    pub estimated_cost: f64,
    /// Estimated number of rows returned
    pub estimated_rows: i64,
    /// Mask of SQLITE_INDEX_SCAN_* flags
    pub idx_flags: i32,
    /// Input: Mask of columns used by statement
    pub col_used: u64,
}

/// The calls a module makes back into the core while its xBestIndex runs, answered from
/// `HiddenIndexInfo` (where.c): `sqlite3_vtab_in()`, `sqlite3_vtab_rhs_value()`,
/// `sqlite3_vtab_distinct()`, `sqlite3_vtab_collation()`.
pub trait IndexInfoHidden {
    /// `sqlite3_vtab_in(pIdxInfo, iCons, bHandle)`
    fn vtab_in(&mut self, i_cons: i32, b_handle: i32) -> bool;
    /// `sqlite3_vtab_rhs_value(pIdxInfo, iCons, &pVal)`: the return code and the value
    fn rhs_value(&mut self, i_cons: i32) -> (i32, Option<Value>);
    /// `sqlite3_vtab_distinct(pIdxInfo)`
    fn distinct(&self) -> i32;
    /// `sqlite3_vtab_collation(pIdxInfo, iCons)`
    fn collation(&mut self, i_cons: i32) -> Option<String>;
}

/// `sqlite3ErrStr(rc)` for the codes a virtual-table method returns.
pub fn sqlite3_err_str(rc: i32) -> &'static str {
    const A_MSG: [&str; 29] = [
        "not an error",
        "SQL logic error",
        "",
        "access permission denied",
        "query aborted",
        "database is locked",
        "database table is locked",
        "out of memory",
        "attempt to write a readonly database",
        "interrupted",
        "disk I/O error",
        "database disk image is malformed",
        "unknown operation",
        "database or disk is full",
        "unable to open database file",
        "locking protocol",
        "",
        "database schema has changed",
        "string or blob too big",
        "constraint failed",
        "datatype mismatch",
        "bad parameter or other API misuse",
        "",
        "authorization denied",
        "",
        "column index out of range",
        "file is not a database",
        "notification message",
        "warning message",
    ];
    match rc {
        516 => "abort due to ROLLBACK", // SQLITE_ABORT_ROLLBACK
        100 => "another row available", // SQLITE_ROW
        101 => "no more rows available", // SQLITE_DONE
        _ => {
            let r = (rc & 0xff) as usize;
            match A_MSG.get(r) {
                Some(m) if !m.is_empty() => m,
                _ => "unknown error",
            }
        }
    }
}

/// `addModuleArgument(pParse, pTable, zArg)`: add a new module argument to
/// pTable->u.vtab.azArg[].
fn add_module_argument(p: &mut Parse, tab: TableId, z_arg: Option<String>) {
    let n_arg = p.tab(tab).vtab_args.len() as i32;
    if n_arg + 3 >= p.db.a_limit[SQLITE_LIMIT_COLUMN as usize] {
        let name = p.tab(tab).z_name.clone();
        p.error_msg(format!("too many columns on {name}"));
    }
    p.tabm(tab).vtab_args.push(z_arg);
}

/// `sqlite3VtabBeginParse(pParse, pName1, pName2, pModuleName, ifNotExists)`: the parser calls
/// this routine when it first sees a CREATE VIRTUAL TABLE statement. The module name has been
/// parsed, but the optional list of parameters that follow the module name are still pending.
pub fn sqlite3_vtab_begin_parse(p: &mut Parse, name1: &Token, name2: &Token, module_name: &Token, if_not_exists: bool) {
    if !p.db.init_busy {
        // (code generation for CREATE VIRTUAL TABLE is a later step)
        p.not_supported("statement kind (grammar rule not ported)");
        return;
    }
    crate::build_tab::sqlite3_start_table(p, name1, name2, false, false, true, if_not_exists);
    let tab = p.p_new_table;
    if tab.is_null() {
        return;
    }
    p.tabm(tab).e_tab_type = TABTYP_VTAB as u8;

    let z_mod = crate::build::sqlite3_name_from_token(p, Some(module_name));
    add_module_argument(p, tab, z_mod);
    add_module_argument(p, tab, None);
    let z_name = p.tab(tab).z_name.clone();
    add_module_argument(p, tab, Some(z_name));
    // pParse->sNameToken.n = &pModuleName->z[pModuleName->n] - pParse->sNameToken.z
    if p.s_name_token.z != Token::NO_Z && module_name.z != Token::NO_Z {
        p.s_name_token.n = module_name.z + module_name.n - p.s_name_token.z;
    }
    // (sqlite3AuthCheck(SQLITE_CREATE_VTABLE): the engine authorizes)
}

/// `addArgumentToVtab(pParse)`: this routine takes the module argument that has been
/// accumulating in pParse->zArg[] and appends it to the list of arguments on the virtual table
/// currently under construction in pParse->pTable.
fn add_argument_to_vtab(p: &mut Parse) {
    if p.s_arg.z != Token::NO_Z && p.p_new_table.nn() {
        let a = p.s_arg.z as usize;
        let b = (a + p.s_arg.n as usize).min(p.z_sql.len());
        let z = p.z_sql[a..b].to_owned();
        let tab = p.p_new_table;
        add_module_argument(p, tab, Some(z));
    }
}

/// `sqlite3VtabFinishParse(pParse, pEnd)`: the parser calls this routine after the CREATE
/// VIRTUAL TABLE statement has been completely parsed.
pub fn sqlite3_vtab_finish_parse(p: &mut Parse, _end: Option<&Token>) {
    let tab = p.p_new_table;
    if tab.is_null() {
        return;
    }
    add_argument_to_vtab(p);
    p.s_arg = Token::EMPTY;
    if p.tab(tab).vtab_args.is_empty() {
        return;
    }

    // If the CREATE VIRTUAL TABLE statement is being entered for the first time (in other
    // words if the virtual table is actually being created now instead of just being read
    // out of sqlite_schema) then do additional initialization work and store the statement
    // text in the sqlite_schema table.
    if !p.db.init_busy {
        p.not_supported("statement kind (grammar rule not ported)");
    } else {
        // If we are rereading the sqlite_schema table create the in-memory record of the
        // table.
        sqlite3_mark_all_shadow_tables_of(p, tab);
        let i_db = p.tab(tab).i_db as usize;
        let key = p.tab(tab).z_name.to_ascii_lowercase();
        p.a_schema[i_db].tbl_hash.insert(key, tab);
        p.p_new_table = TableId::NULL;
    }
}

/// `sqlite3VtabArgInit(pParse)`: the parser calls this routine when it sees the first token of
/// an argument to the module name in a CREATE VIRTUAL TABLE statement.
pub fn sqlite3_vtab_arg_init(p: &mut Parse) {
    add_argument_to_vtab(p);
    p.s_arg = Token::EMPTY;
}

/// `sqlite3VtabArgExtend(pParse, p)`: the parser calls this routine for each token after the
/// first token in an argument to the module name in a CREATE VIRTUAL TABLE statement.
pub fn sqlite3_vtab_arg_extend(p: &mut Parse, t: &Token) {
    if p.s_arg.z == Token::NO_Z {
        p.s_arg = *t;
    } else {
        p.s_arg.n = t.z + t.n - p.s_arg.z;
    }
}

/// `sqlite3GetVTable(db, pTab)`: pTab is a pointer to a Table structure representing a
/// virtual-table. Return a pointer to the VTable object used by connection db to access this
/// virtual-table, if one has been created, or NULL otherwise.
pub fn sqlite3_get_vtable<'b>(p: &'b Parse, tab: TableId) -> Option<&'b VTable> {
    p.tab(tab).vtab.as_ref()
}

/// `vtabCallConstructor(db, pTab, pMod, xConstruct, pzErr)`: invoke a virtual table
/// constructor (xConnect). Returns the result code and the error message.
fn vtab_call_constructor(p: &mut Parse, tab: TableId, module: &VtabModule) -> (i32, Option<String>) {
    // Check that the virtual-table is not already being initialized
    if p.vtab_ctx.contains(&tab) {
        let name = p.tab(tab).z_name.clone();
        return (SQLITE_LOCKED, Some(format!("vtable constructor called recursively: {name}")));
    }
    let z_module_name = p.tab(tab).z_name.clone();

    let i_db = p.tab(tab).i_db.max(0) as usize;
    let z_db = p.db.a_db[i_db].z_db_s_name.clone();
    if p.tab(tab).vtab_args.len() > 1 {
        p.tabm(tab).vtab_args[1] = Some(z_db);
    }

    // Invoke the virtual table constructor
    let args: Vec<String> = p.tab(tab).vtab_args.iter().map(|a| a.clone().unwrap_or_default()).collect();
    let eponymous = p.tab(tab).tab_flags & TF_Eponymous != 0;
    p.vtab_ctx.push(tab);
    let host = p.db.host;
    let r = host.vtab_connect(i_db, &z_module_name, &module.name, &args, eponymous);
    p.vtab_ctx.pop();

    match r {
        VtabConnect::Unsupported => {
            p.not_supported("virtual table the engine cannot connect");
            (SQLITE_ERROR, None)
        }
        VtabConnect::Err(z_err) => {
            if z_err.is_empty() {
                (SQLITE_ERROR, Some(format!("vtable constructor failed: {z_module_name}")))
            } else {
                (SQLITE_ERROR, Some(z_err))
            }
        }
        VtabConnect::Ok { vtab, declare } => {
            let Some(text) = declare else {
                return (SQLITE_ERROR, Some(format!("vtable constructor did not declare schema: {z_module_name}")));
            };
            // sqlite3_declare_vtab(), called by the constructor
            if !sqlite3_declare_vtab(p, tab, &text) {
                p.not_supported("sqlite3_declare_vtab() text the generator could not load");
                return (SQLITE_ERROR, None);
            }
            // If everything went according to plan, link the new VTable structure into the
            // linked list headed by pTab->u.vtab.p. Then loop through the columns of the
            // table to see if any of them contain the token "hidden". If so, set the Column
            // COLFLAG_HIDDEN flag and remove the token from the type string.
            p.tabm(tab).vtab = Some(vtab);
            let mut ooo_hidden = 0u32;
            for i_col in 0..p.tab(tab).a_col.len() {
                let z_type = crate::schema::sqlite3_column_type(&p.tab(tab).a_col[i_col], Some("")).unwrap_or("").to_owned();
                let zt = z_type.as_bytes();
                let n_type = zt.len();
                let mut i = 0;
                while i < n_type {
                    if i + 6 <= n_type
                        && zt[i..i + 6].eq_ignore_ascii_case(b"hidden")
                        && (i == 0 || zt[i - 1] == b' ')
                        && (i + 6 == n_type || zt[i + 6] == b' ')
                    {
                        break;
                    }
                    i += 1;
                }
                if i < n_type {
                    let n_del = 6 + usize::from(i + 6 < n_type);
                    let mut nz: Vec<u8> = Vec::with_capacity(n_type);
                    nz.extend_from_slice(&zt[..i]);
                    nz.extend_from_slice(&zt[i + n_del..]);
                    if nz.len() == i && i > 0 {
                        // zType[i-1] = '\0'
                        nz.truncate(i - 1);
                    }
                    let t = p.tabm(tab);
                    // (the type text is stored only with COLFLAG_HASTYPE: "hidden" is never
                    // a standard type name)
                    t.a_col[i_col].z_type = Some(String::from_utf8_lossy(&nz).into_owned());
                    t.a_col[i_col].col_flags |= COLFLAG_HIDDEN as u16;
                    t.tab_flags |= TF_HasHidden;
                    ooo_hidden = TF_OOOHidden;
                } else {
                    p.tabm(tab).tab_flags |= ooo_hidden;
                }
            }
            (SQLITE_OK, None)
        }
    }
}

/// `sqlite3VtabCallConnect(pParse, pTab)`: this function is invoked by the parser to call the
/// xConnect() method of the virtual table pTab. If an error occurs, an error code is returned
/// and an error left in pParse.
pub fn sqlite3_vtab_call_connect(p: &mut Parse, tab: TableId) -> i32 {
    if sqlite3_get_vtable(p, tab).is_some() {
        return SQLITE_OK;
    }

    // Locate the required virtual table module
    let z_mod = p.tab(tab).vtab_args.first().cloned().flatten().unwrap_or_default();
    let host = p.db.host;
    let module = host.vtab_module(&z_mod, false);

    let rc;
    match module {
        None => {
            p.error_msg(format!("no such module: {z_mod}"));
            rc = SQLITE_ERROR;
        }
        Some(m) => {
            let (r, z_err) = vtab_call_constructor(p, tab, &m);
            rc = r;
            if p.unsupported.is_some() {
                return SQLITE_ERROR;
            }
            if rc != SQLITE_OK {
                p.error_msg(z_err.unwrap_or_default());
                p.rc = rc;
            } else {
                sqlite3_mark_all_shadow_tables_of(p, tab);
            }
        }
    }
    rc
}

/// `sqlite3_declare_vtab(db, zCreateTable)`: this function is used to set the schema of a
/// virtual table. It is only valid to call this function from within the xCreate() or
/// xConnect() of a virtual table module. Returns false if the text does not declare a table
/// the generator can load (C would fail the constructor; the engine's modules never do).
fn sqlite3_declare_vtab(p: &mut Parse, tab: TableId, z_create_table: &str) -> bool {
    // Verify that the first two keywords in the CREATE TABLE statement really are "CREATE"
    // and "TABLE". If this is not the case, then sqlite3_declare_vtab() is being misused.
    {
        let z = z_create_table.as_bytes();
        let mut pos = 0;
        for kw in [crate::tk::TK_CREATE, crate::tk::TK_TABLE] {
            let mut t;
            loop {
                let (n, tt) = crate::tokenize::sqlite3_get_token(&z[pos.min(z.len())..]);
                pos += n;
                t = tt;
                if n == 0 || (t != crate::tk::TK_SPACE && t != crate::tk::TK_COMMENT) {
                    break;
                }
            }
            if t != kw {
                return false;
            }
        }
    }
    let Some(new) = crate::build_tab::declare_vtab_parse(p, z_create_table) else {
        return false;
    };
    if p.tab(tab).a_col.is_empty() {
        let cols = std::mem::take(&mut p.tabm(new).a_col);
        let new_flags = p.tab(new).tab_flags;
        if !p.tab(new).has_rowid() {
            // (a WITHOUT ROWID virtual table: its PRIMARY KEY index is not ported)
            return false;
        }
        let t = p.tabm(tab);
        let n = cols.len() as i16;
        t.a_col = cols;
        t.n_nv_col = n;
        t.tab_flags |= new_flags & (TF_WithoutRowid | TF_NoVisibleRowid);
    }
    true
}

/// `sqlite3VtabEponymousTableInit(pParse, pMod)`: check to see if virtual table module pMod can
/// be have an eponymous virtual table instance. If it can, create one if one does not already
/// exist. Return non-zero if either the eponymous virtual table instance exists when this
/// routine returns or if an attempt to create it failed and an error message was left in
/// pParse.
///
/// An eponymous virtual table instance is one that is named after its module, and more
/// importantly, does not require a CREATE VIRTUAL TABLE statement in order to come into
/// existence. Eponymous virtual table instances always exist. They cannot be DROP-ed.
///
/// Any virtual table module for which xConnect and xCreate are the same method can have an
/// eponymous virtual table instance. Returns None for C's 0, else `pMod->pEpoTab` (NULL
/// after a failed attempt).
pub fn sqlite3_vtab_eponymous_table_init(p: &mut Parse, module: &VtabModule) -> Option<TableId> {
    let key = module.name.to_ascii_lowercase();
    if let Some(&t) = p.epo_tab.get(&key) {
        return Some(t);
    }
    if !module.eponymous {
        return None;
    }
    let tab = p.new_tab(Table {
        z_name: module.name.clone(),
        n_tab_ref: 1,
        e_tab_type: TABTYP_VTAB as u8,
        i_db: 0,
        i_p_key: -1,
        tab_flags: TF_Eponymous,
        ..Table::default()
    });
    p.epo_tab.insert(key.clone(), tab);
    add_module_argument(p, tab, Some(module.name.clone()));
    add_module_argument(p, tab, None);
    add_module_argument(p, tab, Some(module.name.clone()));
    let (rc, z_err) = vtab_call_constructor(p, tab, module);
    if p.unsupported.is_some() {
        return None;
    }
    if rc != SQLITE_OK {
        p.error_msg(z_err.unwrap_or_default());
        p.rc = rc;
        // sqlite3VtabEponymousTableClear(db, pMod)
        p.epo_tab.remove(&key);
        return Some(TableId::NULL);
    }
    Some(tab)
}

/// `sqlite3VtabOverloadFunction(db, pDef, nArg, pExpr)`: the first parameter (pDef) is a
/// function implementation. The second parameter (pExpr) is the first argument to this
/// function. If pExpr is a column in a virtual table, then let the virtual table
/// implementation have an opportunity to overload the function.
///
/// This routine is used to allow virtual table implementations to overload MATCH, LIKE, GLOB,
/// and REGEXP operators.
///
/// Return either the pDef argument (indicating no change) or a new FuncDef structure that is
/// marked as ephemeral using the SQLITE_FUNC_EPHEM flag.
pub fn sqlite3_vtab_overload_function(
    p: &mut Parse,
    def: std::sync::Arc<sqliters_vdbe::vdbeaux::FuncDef>,
    n_arg: i32,
    e: ExprId,
) -> std::sync::Arc<sqliters_vdbe::vdbeaux::FuncDef> {
    // Check to see the left operand is a column in a virtual table
    if e.is_null() {
        return def;
    }
    if p.e(e).op != crate::tk::TK_COLUMN {
        return def;
    }
    let tab = p.e(e).y_tab;
    if tab.is_null() {
        return def;
    }
    if !p.tab(tab).is_virtual() {
        return def;
    }
    let Some(vt) = sqlite3_get_vtable(p, tab) else { return def };
    if !vt.has_find_function {
        return def;
    }
    let id = vt.id;

    // Call the xFindFunction method on the virtual table implementation to see if the
    // implementation wants to overload this function. (Though undocumented, we have
    // historically always invoked xFindFunction with an all lower-case function name.)
    let host = p.db.host;
    let rc = host.vtab_find_function(id, n_arg, &def.name);
    if rc == 0 {
        return def;
    }

    // Create a new ephemeral function definition for the overloaded function
    let mut new = (*def).clone();
    new.func_flags |= SQLITE_FUNC_EPHEM;
    std::sync::Arc::new(new)
}

/// `sqlite3VtabMakeWritable(pParse, pTab)`: make sure virtual table pTab is contained in the
/// pParse->apVirtualLock[] array so that an OP_VBegin will get generated for it. Add pTab to
/// the array if it is missing. If pTab is already in the array, this routine is a no-op.
pub fn sqlite3_vtab_make_writable(p: &mut Parse, tab: TableId) {
    // (sqlite3ParseToplevel(pParse): the generator has no nested parses of triggers yet)
    if p.ap_vtab_lock.contains(&tab) {
        return;
    }
    p.ap_vtab_lock.push(tab);
}

/// `sqlite3IsShadowTableOf(db, pTab, zName)`: return true if pTab is a virtual table and zName
/// is a shadow table name for that virtual table.
pub fn sqlite3_is_shadow_table_of(p: &Parse, tab: TableId, z_name: &str) -> bool {
    if !p.tab(tab).is_virtual() {
        return false;
    }
    let tn = p.tab(tab).z_name.as_bytes();
    let n_name = tn.len();
    let zb = z_name.as_bytes();
    if zb.len() < n_name || !zb[..n_name].eq_ignore_ascii_case(tn) {
        return false;
    }
    if zb.get(n_name) != Some(&b'_') {
        return false;
    }
    let z_mod = p.tab(tab).vtab_args.first().cloned().flatten().unwrap_or_default();
    let Some(m) = p.db.host.vtab_module(&z_mod, false) else { return false };
    let suffix = &z_name[n_name + 1..];
    m.shadow_names.iter().any(|s| s.eq_ignore_ascii_case(suffix))
}

/// `sqlite3MarkAllShadowTablesOf(db, pTab)`: table pTab is a virtual table. If it the virtual
/// table implementation exists and has an xShadowName method, then loop over all other
/// ordinary tables within the same schema looking for shadow tables of pTab, and mark any
/// shadow tables seen using the TF_Shadow flag.
pub fn sqlite3_mark_all_shadow_tables_of(p: &mut Parse, tab: TableId) {
    let z_mod = p.tab(tab).vtab_args.first().cloned().flatten().unwrap_or_default();
    let Some(m) = p.db.host.vtab_module(&z_mod, false) else { return };
    if m.shadow_names.is_empty() {
        return;
    }
    let tn = p.tab(tab).z_name.clone();
    let n_name = tn.len();
    let i_db = p.tab(tab).i_db;
    if i_db < 0 || i_db as usize >= p.a_schema.len() {
        return;
    }
    let others: Vec<TableId> = p.a_schema[i_db as usize].tbl_hash.values().copied().collect();
    for other in others {
        let o = p.tab(other);
        if !o.is_ordinary() {
            continue;
        }
        if o.tab_flags & TF_Shadow != 0 {
            continue;
        }
        let ob = o.z_name.as_bytes();
        if ob.len() > n_name
            && ob[..n_name].eq_ignore_ascii_case(tn.as_bytes())
            && ob[n_name] == b'_'
            && m.shadow_names.iter().any(|s| s.eq_ignore_ascii_case(&o.z_name[n_name + 1..]))
        {
            p.tabm(other).tab_flags |= TF_Shadow;
        }
    }
}

/// `sqlite3ShadowTableName(db, zName)`: return true if zName is a shadow table name in the
/// current database connection.
pub fn sqlite3_shadow_table_name(p: &mut Parse, z_name: &str) -> bool {
    let Some(z_tail) = z_name.rfind('_') else { return false };
    let z_copy = &z_name[..z_tail];
    let tab = crate::schema::sqlite3_find_table(p, z_copy, None);
    if tab.is_null() {
        return false;
    }
    if !p.tab(tab).is_virtual() {
        return false;
    }
    sqlite3_is_shadow_table_of(p, tab, z_name)
}
