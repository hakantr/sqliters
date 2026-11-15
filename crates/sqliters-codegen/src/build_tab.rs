//! Schema objects exactly as C builds them: the part of `build.c` that the parser calls for
//! `CREATE TABLE` / `CREATE INDEX` / `CREATE VIEW` (`sqlite3StartTable`, `sqlite3AddColumn`,
//! `sqlite3AddNotNull`, `sqlite3AddDefaultValue`, `sqlite3AddPrimaryKey`,
//! `sqlite3AddCheckConstraint`, `sqlite3AddCollateType`, `sqlite3AddGenerated`,
//! `sqlite3CreateForeignKey`, `sqlite3DeferForeignKey`, `sqlite3EndTable`,
//! `convertToWithoutRowidTable`, `sqlite3CreateIndex`, `sqlite3CreateView`), the schema
//! loader of `prepare.c` (`sqlite3Init`, `sqlite3InitOne`, `sqlite3InitCallback`), the
//! statistics loader of `analyze.c` (`sqlite3AnalysisLoad`, `analysisLoader`,
//! `decodeIntArray`) and `sqlite3ValueFromExpr` of `vdbemem.c`.
//!
//! This is schema *loading*: every function runs with `db->init.busy` set, over the SQL text
//! stored in `sqlite_schema`, exactly as `sqlite3InitCallback()` does, so the `Table` /
//! `Column` / `Index` objects of the generator are C's by construction. Code generation for
//! the DDL statements themselves is not ported: outside of schema loading the grammar rules
//! report the statement as unsupported.

use sqliters_types::Value;

use crate::callback::*;
use crate::consts::*;
use crate::expr::*;
use crate::schema::*;
use crate::select::sqlite3_log_est;
use crate::sqliteint::*;
use crate::tk::*;
use crate::util::sqlite3_dequote;

/// One row of `sqlite_schema` (`sqlite_temp_schema`), as `sqlite3InitCallback()` receives it.
#[derive(Debug, Clone, Default)]
pub struct SchemaRow {
    /// `type`: "table", "index", "view" or "trigger"
    pub ty: String,
    pub name: String,
    pub tbl_name: String,
    pub rootpage: u32,
    /// `sql`: NULL for the automatic indexes
    pub sql: Option<String>,
}

/// `sqlite3DequoteToken(p)`: if the input token p is quoted, try to adjust the token to
/// remove the quotes. This might or might not work. If the quoted token contains embedded
/// quotes, then leave the token unchanged.
fn sqlite3_dequote_token(p: &Parse, t: &mut Token) {
    if t.n < 2 || t.z == Token::NO_Z {
        return;
    }
    let b = p.tok_str(t).as_bytes();
    let is_quote = |c: u8| matches!(c, b'"' | b'\'' | b'[' | b'`');
    if !is_quote(b[0]) {
        return;
    }
    for &c in &b[1..b.len() - 1] {
        if is_quote(c) {
            return;
        }
    }
    t.n -= 2;
    t.z += 1;
}

/// True for the bytes `sqlite3Isspace()` accepts.
#[inline]
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `sqlite3DbSpanDup(db, zStart, zEnd)`: the text between two offsets of the statement, with
/// leading and trailing whitespace removed.
fn span_dup(p: &Parse, mut z_start: usize, z_end: usize) -> String {
    let b = p.z_sql.as_bytes();
    let z_end = z_end.min(b.len());
    while z_start < z_end && is_space(b[z_start]) {
        z_start += 1;
    }
    let mut n = z_end.saturating_sub(z_start);
    while n > 0 && is_space(b[z_start + n - 1]) {
        n -= 1;
    }
    String::from_utf8_lossy(&b[z_start..z_start + n]).into_owned()
}

/// `sqlite3TwoPartName(pParse, pName1, pName2, pUnqual)`: the table or view or trigger name
/// is passed to this routine via tokens pName1 and pName2. This routine sets `*unqual` to the
/// token that holds the unqualified name and returns the index of the database (-1 on error).
fn sqlite3_two_part_name(p: &mut Parse, name1: &Token, name2: &Token, unqual: &mut Token) -> i32 {
    if name2.n > 0 {
        if p.db.init_busy {
            p.error_msg("corrupt database");
            return -1;
        }
        *unqual = *name2;
        let z = sqlite3_dequote(p.tok_str(name1));
        let i_db = p.db.a_db.iter().position(|d| d.z_db_s_name.eq_ignore_ascii_case(&z));
        match i_db {
            Some(i) => i as i32,
            None => {
                let t = *name1;
                let msg = format!("unknown database {}", p.tok_str(&t));
                p.error_msg_tok(msg, &t);
                -1
            }
        }
    } else {
        *unqual = *name1;
        p.init_i_db as i32
    }
}

/// `sqlite3CheckObjectName(pParse, zName, zType, zTblName)`: this routine is used to check if
/// the UTF-8 string zName is a legal unqualified name for a new schema object (table, index,
/// view or trigger). While the schema is loaded, the name, type and table name of the object
/// must be the ones of the `sqlite_schema` row being parsed.
fn sqlite3_check_object_name(p: &mut Parse, z_name: &str, z_type: &str, z_tbl_name: &str) -> bool {
    if (p.db.flags & SQLITE_WriteSchema) != 0 && (p.db.flags & SQLITE_Defensive) == 0 {
        // sqlite3WritableSchema(db)
        return false;
    }
    if p.db.init_busy {
        if !z_type.eq_ignore_ascii_case(&p.init_az[0])
            || !z_name.eq_ignore_ascii_case(&p.init_az[1])
            || !z_tbl_name.eq_ignore_ascii_case(&p.init_az[2])
        {
            p.error_msg(""); // corruptSchema() will supply the error
            return true;
        }
    } else if p.nested == 0 && z_name.len() >= 7 && z_name.as_bytes()[..7].eq_ignore_ascii_case(b"sqlite_") {
        // (only reached by sqlite3_declare_vtab(): other statements that create objects are
        // not ported. sqlite3ReadOnlyShadowTables(): SQLITE_Defensive is never set for the
        // generator.)
        p.error_msg(format!("object name reserved for internal use: {z_name}"));
        return true;
    }
    false
}

/// `sqlite3FindIndex(db, zName, zDb)`: locate the in-memory structure that describes a
/// particular index given the name of that index and the name of the database that contains
/// the index. Return NULL if not found.
pub fn sqlite3_find_index(p: &Parse, z_name: &str, z_db: Option<&str>) -> IndexId {
    let key = z_name.to_ascii_lowercase();
    let n_db = p.db.a_db.len();
    // OMIT_TEMPDB==0: search TEMP before MAIN
    for i in 0..n_db {
        let j = if i < 2 { i ^ 1 } else { i };
        if let Some(zd) = z_db
            && !zd.eq_ignore_ascii_case(&p.db.a_db[j].z_db_s_name)
        {
            continue;
        }
        if let Some(sch) = p.a_schema.get(j)
            && let Some(&ix) = sch.idx_hash.get(&key)
        {
            return ix;
        }
    }
    IndexId::NULL
}

/// `sqlite3StartTable(pParse, pName1, pName2, isTemp, isView, isVirtual, noErr)`: begin
/// constructing a new table representation in memory. This is the first of several action
/// routines that get called in response to a CREATE TABLE statement. In particular, this
/// routine is called after seeing tokens "CREATE" and "TABLE" and the table name. The isTemp
/// flag is true if the table should be stored in the auxiliary database file instead of in
/// the main database file. This is normally the case when the "TEMP" or "TEMPORARY" keyword
/// occurs in between CREATE and TABLE.
///
/// The new table record is initialized and put in pParse->pNewTable. As more of the CREATE
/// TABLE statement is parsed, additional action routines will be called to add more
/// information to this record. At the end of the CREATE TABLE statement, the
/// sqlite3EndTable() routine is called to complete the construction of the new table record.
pub fn sqlite3_start_table(p: &mut Parse, name1: &Token, name2: &Token, is_temp: bool, is_view: bool, _is_virtual: bool, _no_err: bool) {
    if !p.db.init_busy && !p.in_declare_vtab() {
        // (code generation for CREATE TABLE / CREATE VIEW is a later step)
        p.not_supported("statement kind (grammar rule not ported)");
        return;
    }
    let i_db: i32;
    let z_name: String;
    let mut name = Token::EMPTY;
    if p.db.init_busy && p.init_new_tnum == 1 {
        // Special case: Parsing the sqlite_schema or sqlite_temp_schema schema
        i_db = p.init_i_db as i32;
        z_name = (if i_db == 1 { "sqlite_temp_master" } else { "sqlite_master" }).to_owned();
        name = *name1;
    } else {
        // The common case
        let mut d = sqlite3_two_part_name(p, name1, name2, &mut name);
        if d < 0 {
            return;
        }
        if is_temp && name2.n > 0 && d != 1 {
            // If creating a temp table, the name may not be qualified. Unless the database
            // name is "temp" anyway.
            p.error_msg("temporary table name must be unqualified");
            return;
        }
        if is_temp {
            d = 1;
        }
        i_db = d;
        z_name = sqlite3_dequote(p.tok_str(&name));
    }
    p.s_name_token = name;
    if p.init_i_db == 1 && p.db.init_busy {
        // if( db->init.iDb==1 ) isTemp = 1;  (only read by the authorizer)
    }
    if sqlite3_check_object_name(p, &z_name, if is_view { "view" } else { "table" }, &z_name) {
        p.check_schema = true;
        return;
    }
    // (sqlite3AuthCheck: the engine authorizes. !IN_SPECIAL_PARSE name-collision checks:
    // skipped while the schema is loaded, db->init.busy)
    let tab = p.new_tab(Table {
        z_name,
        i_p_key: -1,
        i_db,
        n_tab_ref: 1,
        n_row_log_est: 200, // sqlite3LogEst(1048576)
        ..Table::default()
    });
    p.p_new_table = tab;
}

/// `sqlite3AddColumn(pParse, sName, sType)`: add a new column to the table currently being
/// constructed.
///
/// The parser calls this routine once for each column declaration in a CREATE TABLE
/// statement. sqlite3StartTable() gets called first to get things going. Then this routine is
/// called for each column.
pub fn sqlite3_add_column(p: &mut Parse, mut s_name: Token, mut s_type: Token) {
    let tab = p.p_new_table;
    if tab.is_null() {
        return;
    }
    let mut e_type = COLTYPE_CUSTOM as u8;
    let mut sz_est: u8 = 1;
    let mut affinity = SQLITE_AFF_BLOB;
    if p.tab(tab).a_col.len() as i32 + 1 > p.db.a_limit[SQLITE_LIMIT_COLUMN as usize] {
        let msg = format!("too many columns on {}", p.tab(tab).z_name);
        p.error_msg(msg);
        return;
    }
    sqlite3_dequote_token(p, &mut s_name);

    // Because keywords GENERATE ALWAYS can be converted into identifiers by the parser, we
    // need to check to see if the datatype ends with "GENERATED ALWAYS".
    if s_type.n >= 16 {
        let t = p.tok_str(&s_type).as_bytes().to_vec();
        if t[t.len() - 6..].eq_ignore_ascii_case(b"always") {
            let mut n = t.len() - 6;
            while n > 0 && is_space(t[n - 1]) {
                n -= 1;
            }
            if n >= 9 && t[n - 9..n].eq_ignore_ascii_case(b"generated") {
                n -= 9;
                while n > 0 && is_space(t[n - 1]) {
                    n -= 1;
                }
            }
            s_type.n = n as u32;
        }
    }

    // Check for standard typenames. For standard typenames we will set the Column.eType
    // field rather than storing the typename after the column name, in order to save space.
    if s_type.n >= 3 {
        sqlite3_dequote_token(p, &mut s_type);
        let t = p.tok_str(&s_type).to_owned();
        for (i, (name, aff)) in STD_TYPE.iter().enumerate() {
            if t.eq_ignore_ascii_case(name) {
                s_type.n = 0;
                e_type = i as u8 + 1;
                affinity = *aff;
                if affinity <= SQLITE_AFF_TEXT {
                    sz_est = 5;
                }
                break;
            }
        }
    }

    let z = sqlite3_dequote(p.tok_str(&s_name));
    if !p.tab(tab).a_col.is_empty() && crate::resolve::sqlite3_column_index(p, tab, &z) >= 0 {
        p.error_msg(format!("duplicate column name: {z}"));
        return;
    }
    let mut col = Column { z_cn_name: z, ..Column::default() };
    // (sqlite3ColumnPropertiesFromName: SQLITE_ENABLE_HIDDEN_COLUMNS is not compiled)

    if s_type.n == 0 {
        // If there is no type specified, columns have the default affinity 'BLOB' with a
        // default size of 4 bytes.
        col.affinity = affinity;
        col.e_c_type = e_type;
        col.sz_est = sz_est;
    } else {
        let z_type = sqlite3_dequote(p.tok_str(&s_type));
        let (aff, sz) = sqlite3_affinity_type_sz(&z_type);
        col.affinity = aff;
        col.sz_est = sz;
        col.z_type = Some(z_type);
        col.col_flags |= COLFLAG_HASTYPE as u16;
    }
    let t = p.tabm(tab);
    t.a_col.push(col);
    t.n_nv_col += 1;
    p.cr_constraint_name = Token::EMPTY;
}

/// `sqlite3AddNotNull(pParse, onError)`: this routine is called by the parser while in the
/// middle of parsing a CREATE TABLE statement. A "NOT NULL" constraint has been seen on a
/// column. This routine sets the notNull flag on the column currently under construction.
pub fn sqlite3_add_not_null(p: &mut Parse, on_error: i32) {
    let tab = p.p_new_table;
    if tab.is_null() || p.tab(tab).a_col.is_empty() {
        return;
    }
    let n_col = p.tab(tab).a_col.len();
    {
        let t = p.tabm(tab);
        t.a_col[n_col - 1].not_null = on_error as u8;
        t.tab_flags |= TF_HasNotNull;
    }

    // Set the uniqNotNull flag on any UNIQUE or PK indexes already created on this column.
    if p.tab(tab).a_col[n_col - 1].col_flags & COLFLAG_UNIQUE as u16 != 0 {
        for ix in p.tab(tab).p_index.clone() {
            if p.idx(ix).ai_column[0] as usize == n_col - 1 {
                p.idxm(ix).uniq_not_null = true;
            }
        }
    }
}

/// `sqlite3ColumnSetExpr(pParse, pTab, pCol, pExpr)`: set the expression associated with a
/// column. This is usually the DEFAULT value, but might also be the expression that computes
/// the value for a generated column.
fn sqlite3_column_set_expr(p: &mut Parse, tab: TableId, i_col: usize, e: ExprId) {
    let list = p.tab(tab).p_dflt_list;
    let i_dflt = p.tab(tab).a_col[i_col].i_dflt as usize;
    if i_dflt == 0 || list.is_null() || p.l(list).a.len() < i_dflt {
        let n = if list.is_null() { 1 } else { p.l(list).a.len() + 1 };
        p.tabm(tab).a_col[i_col].i_dflt = n as u16;
        let nl = sqlite3_expr_list_append(p, list, e);
        p.tabm(tab).p_dflt_list = nl;
    } else {
        p.lm(list).a[i_dflt - 1].p_expr = e;
    }
}

/// `sqlite3ColumnExpr(pTab, pCol)`: return the expression associated with a column. The
/// expression might be the DEFAULT clause or the AS clause of a generated column. Return
/// NULL if the column has no associated expression.
pub fn sqlite3_column_expr(p: &Parse, tab: TableId, i_col: usize) -> ExprId {
    let t = p.tab(tab);
    let i_dflt = t.a_col[i_col].i_dflt as usize;
    if i_dflt == 0 || !t.is_ordinary() || t.p_dflt_list.is_null() || p.l(t.p_dflt_list).a.len() < i_dflt {
        return ExprId::NULL;
    }
    p.l(t.p_dflt_list).a[i_dflt - 1].p_expr
}

/// `sqlite3AddDefaultValue(pParse, pExpr, zStart, zEnd)`: the expression is the default value
/// for the most recently added column of the table currently under construction.
///
/// Default value expressions must be constant. Raise an exception if this is not the case.
///
/// This routine is called by the parser while in the middle of parsing a CREATE TABLE
/// statement.
pub fn sqlite3_add_default_value(p: &mut Parse, e: ExprId, z_start: usize, z_end: usize) {
    let tab = p.p_new_table;
    if tab.is_null() {
        return;
    }
    let is_init = p.db.init_busy && p.init_i_db != 1;
    let i_col = p.tab(tab).a_col.len() - 1;
    if sqlite3_expr_is_constant_or_function(p, e, is_init as u8) == 0 {
        let msg = format!("default value of column [{}] is not constant", p.tab(tab).a_col[i_col].z_cn_name);
        p.error_msg(msg);
    } else if p.tab(tab).a_col[i_col].col_flags & COLFLAG_GENERATED as u16 != 0 {
        p.error_msg("cannot use DEFAULT on a generated column");
    } else {
        // A copy of pExpr is used instead of the original, as pExpr contains tokens that
        // point to volatile memory.
        let x = Expr { op: TK_SPAN, z_token: Some(span_dup(p, z_start, z_end)), p_left: e, flags: EP_Skip, ..Expr::default() };
        let x = p.new_expr(x);
        let dflt = sqlite3_expr_dup(p, x, EXPRDUP_REDUCE);
        sqlite3_column_set_expr(p, tab, i_col, dflt);
    }
}

/// `makeColumnPartOfPrimaryKey(pParse, pCol)`: make sure the given column is flagged as a
/// part of the PRIMARY KEY; generated columns cannot be.
fn make_column_part_of_primary_key(p: &mut Parse, tab: TableId, i_col: usize) {
    p.tabm(tab).a_col[i_col].col_flags |= COLFLAG_PRIMKEY as u16;
    if p.tab(tab).a_col[i_col].col_flags & COLFLAG_GENERATED as u16 != 0 {
        p.error_msg("generated columns cannot be part of the PRIMARY KEY");
    }
}

/// `sqlite3StringToId(p)`: backwards compatibility hack: a string in the position of a column
/// name of a PRIMARY KEY / UNIQUE / index list is treated as an identifier.
fn sqlite3_string_to_id(p: &mut Parse, e: ExprId) {
    if p.e(e).op == TK_STRING {
        p.em(e).op = TK_ID;
    } else if p.e(e).op == TK_COLLATE && p.e(p.e(e).p_left).op == TK_STRING {
        let l = p.e(e).p_left;
        p.em(l).op = TK_ID;
    }
}

/// `sqlite3HasExplicitNulls(pParse, pList)`: if expression list pList contains an expression
/// that was parsed with an explicit "NULLS FIRST" or "NULLS LAST" clause, leave an error in
/// pParse and return non-zero.
fn sqlite3_has_explicit_nulls(p: &mut Parse, list: ExprListId) -> bool {
    if list.nn() {
        for i in 0..p.l(list).a.len() {
            if p.l(list).a[i].b_nulls {
                let sf = p.l(list).a[i].sort_flags;
                let msg = format!("unsupported use of NULLS {}", if sf == 0 || sf == 3 { "FIRST" } else { "LAST" });
                p.error_msg(msg);
                return true;
            }
        }
    }
    false
}

/// `sqlite3AddPrimaryKey(pParse, pList, onError, autoInc, sortOrder)`: designate the PRIMARY
/// KEY for the table. pList is a list of names of columns that form the primary key. If pList
/// is NULL, then the most recently added column of the table is the primary key.
///
/// A table can have at most one primary key. If the table already has a primary key (and this
/// is the second primary key) then create an error.
///
/// If the PRIMARY KEY is on a single column whose datatype is INTEGER, then we will try to use
/// that column as the rowid. Set the Table.iPKey field of the table under construction to be
/// the index of the INTEGER PRIMARY KEY column. Table.iPKey is set to -1 if there is no
/// INTEGER PRIMARY KEY.
///
/// If the key is not an INTEGER PRIMARY KEY, then create a unique index for the key. No index
/// is created for INTEGER PRIMARY KEYs.
pub fn sqlite3_add_primary_key(p: &mut Parse, list: ExprListId, on_error: i32, auto_inc: i32, sort_order: i32) {
    let tab = p.p_new_table;
    if tab.is_null() {
        return;
    }
    if p.tab(tab).tab_flags & TF_HasPrimaryKey != 0 {
        let msg = format!("table \"{}\" has more than one primary key", p.tab(tab).z_name);
        p.error_msg(msg);
        return;
    }
    p.tabm(tab).tab_flags |= TF_HasPrimaryKey;
    let mut i_col: i32 = -1;
    let mut have_col = false;
    let n_term;
    if list.is_null() {
        i_col = p.tab(tab).a_col.len() as i32 - 1;
        have_col = true;
        make_column_part_of_primary_key(p, tab, i_col as usize);
        n_term = 1;
    } else {
        n_term = p.l(list).a.len();
        for i in 0..n_term {
            let ce = sqlite3_expr_skip_collate(p, p.l(list).a[i].p_expr);
            sqlite3_string_to_id(p, ce);
            if p.e(ce).op == TK_ID {
                let name = p.e(ce).tok().to_owned();
                i_col = crate::resolve::sqlite3_column_index(p, tab, &name);
                if i_col >= 0 {
                    have_col = true;
                    make_column_part_of_primary_key(p, tab, i_col as usize);
                }
            }
        }
    }
    // (pCol keeps pointing at the last column found, iCol is the result of the last lookup)
    if n_term == 1
        && have_col
        && i_col >= 0
        && p.tab(tab).a_col[i_col as usize].e_c_type == COLTYPE_INTEGER as u8
        && sort_order != SQLITE_SO_DESC
    {
        {
            let t = p.tabm(tab);
            t.i_p_key = i_col as i16;
            t.key_conf = on_error as u8;
            if auto_inc != 0 {
                t.tab_flags |= TF_Autoincrement;
            }
        }
        if list.nn() {
            p.i_pk_sort_order = p.l(list).a[0].sort_flags;
        }
        let _ = sqlite3_has_explicit_nulls(p, list);
    } else if auto_inc != 0 {
        p.error_msg("AUTOINCREMENT is only allowed on an INTEGER PRIMARY KEY");
    } else {
        sqlite3_create_index(p, None, None, SrcListId::NULL, list, on_error, None, ExprId::NULL, sort_order, false, SQLITE_IDXTYPE_PRIMARYKEY as u8);
    }
}

/// `sqlite3AddCheckConstraint(pParse, pCheckExpr, zStart, zEnd)`: add a new CHECK constraint
/// to the table currently under construction.
pub fn sqlite3_add_check_constraint(p: &mut Parse, check: ExprId, z_start: usize, z_end: usize) {
    let tab = p.p_new_table;
    let read_only = p.db.a_db.get(p.init_i_db).is_some_and(|d| d.read_only);
    if tab.nn() && !p.in_declare_vtab() && !read_only {
        let l0 = p.tab(tab).p_check;
        let l = sqlite3_expr_list_append(p, l0, check);
        p.tabm(tab).p_check = l;
        if p.cr_constraint_name.n != 0 {
            let t = p.cr_constraint_name;
            sqlite3_expr_list_set_name(p, l, &t, 1);
        } else {
            let b = p.z_sql.as_bytes();
            let mut s = z_start + 1;
            while s < b.len() && is_space(b[s]) {
                s += 1;
            }
            let mut e = z_end;
            while e > s && is_space(b[e - 1]) {
                e -= 1;
            }
            let t = Token { z: s as u32, n: (e - s) as u32 };
            sqlite3_expr_list_set_name(p, l, &t, 1);
        }
    }
}

/// `sqlite3AddCollateType(pParse, pToken)`: set the collation function of the most recently
/// parsed table column to the CollSeq given.
pub fn sqlite3_add_collate_type(p: &mut Parse, tok: &Token) {
    let tab = p.p_new_table;
    if tab.is_null() {
        return;
    }
    let i = p.tab(tab).a_col.len() - 1;
    let z_coll = sqlite3_dequote(p.tok_str(tok));
    // sqlite3LocateCollSeq(): while the schema is loaded (initbusy) sqlite3FindCollSeq() is
    // called with create==1, so the collation always "exists"
    if p.db.init_busy || sqlite3_locate_coll_seq(p, &z_coll).is_some() {
        {
            // sqlite3ColumnSetColl(db, &p->aCol[i], zColl)
            let c = &mut p.tabm(tab).a_col[i];
            c.z_coll = Some(z_coll.clone());
            c.col_flags |= COLFLAG_HASCOLL as u16;
        }
        // If the column is declared as "<name> PRIMARY KEY COLLATE <type>", then an index
        // may have been created on this column before the collation type was added. Correct
        // this if it is the case.
        for ix in p.tab(tab).p_index.clone() {
            if p.idx(ix).ai_column[0] as usize == i {
                let x = p.idxm(ix);
                x.az_coll[0] = z_coll.clone();
                x.a_coll_static[0] = false;
            }
        }
    }
}

/// `sqlite3AddGenerated(pParse, pExpr, pType)`: change the most recently parsed column to be
/// a GENERATED ALWAYS AS column.
pub fn sqlite3_add_generated(p: &mut Parse, mut e: ExprId, ty: Option<&Token>) {
    let tab = p.p_new_table;
    if tab.is_null() {
        // generated column in an CREATE TABLE IF NOT EXISTS that already exists
        return;
    }
    let i_col = p.tab(tab).a_col.len() - 1;
    if p.in_declare_vtab() {
        p.error_msg("virtual tables cannot use computed columns");
        return;
    }
    let mut e_type = COLFLAG_VIRTUAL as u16;
    let mut bad = p.tab(tab).a_col[i_col].i_dflt > 0;
    if !bad && let Some(t) = ty {
        let z = p.tok_str(t);
        if z.eq_ignore_ascii_case("virtual") {
            // no-op
        } else if z.eq_ignore_ascii_case("stored") {
            e_type = COLFLAG_STORED as u16;
        } else {
            bad = true;
        }
    }
    if bad {
        let msg = format!("error in generated column \"{}\"", p.tab(tab).a_col[i_col].z_cn_name);
        p.error_msg(msg);
        return;
    }
    {
        let t = p.tabm(tab);
        if e_type == COLFLAG_VIRTUAL as u16 {
            t.n_nv_col -= 1;
        }
        t.a_col[i_col].col_flags |= e_type;
        t.tab_flags |= e_type as u32;
    }
    if p.tab(tab).a_col[i_col].col_flags & COLFLAG_PRIMKEY as u16 != 0 {
        make_column_part_of_primary_key(p, tab, i_col); // For the error message
    }
    if e.nn() && p.e(e).op == TK_ID {
        // The value of a generated column needs to be a real expression, not just a
        // reference to another column, in order for covering index optimizations to work
        // correctly. So if the value is not an expression, turn it into one by adding a unary
        // "+" operator.
        e = sqlite3_p_expr(p, TK_UPLUS, e, ExprId::NULL);
    }
    if e.nn() && p.e(e).op != TK_RAISE {
        let aff = p.tab(tab).a_col[i_col].affinity;
        p.em(e).aff_expr = aff;
    }
    sqlite3_column_set_expr(p, tab, i_col, e);
}

/// `sqlite3CreateForeignKey(pParse, pFromCol, pTo, pToCol, flags)`: this routine is called to
/// create a new foreign key on the table currently under construction. pFromCol determines
/// which columns in the current table point to the foreign key. If pFromCol==0 then connect
/// the key to the last column inserted. pTo is the name of the table referred to (a.k.a the
/// "parent" table). pToCol is a list of tables in the parent pTo that the foreign key points
/// to. flags contains all information about the conflict resolution algorithms specified in
/// the ON DELETE, ON UPDATE and ON INSERT clauses.
///
/// An FKey structure is created and added to the table currently under construction in the
/// pParse->pNewTable field.
///
/// The foreign key is set for IMMEDIATE processing. A subsequent call to
/// sqlite3DeferForeignKey() might change this to DEFERRED.
pub fn sqlite3_create_foreign_key(p: &mut Parse, from_col: ExprListId, to: &Token, to_col: ExprListId, flags: i32) {
    let tab = p.p_new_table;
    if tab.is_null() || p.in_declare_vtab() {
        return;
    }
    let n_col;
    if from_col.is_null() {
        let i_col = p.tab(tab).a_col.len() as i32 - 1;
        if i_col < 0 {
            return;
        }
        if to_col.nn() && p.l(to_col).a.len() != 1 {
            let msg = format!(
                "foreign key on {} should reference only one column of table {}",
                p.tab(tab).a_col[i_col as usize].z_cn_name,
                p.tok_str(to)
            );
            p.error_msg_tok(msg, to);
            return;
        }
        n_col = 1;
    } else if to_col.nn() && p.l(to_col).a.len() != p.l(from_col).a.len() {
        p.error_msg("number of columns in foreign key does not match the number of columns in the referenced table");
        return;
    } else {
        n_col = p.l(from_col).a.len();
    }
    let mut fk = FKey {
        z_to: sqlite3_dequote(p.tok_str(to)),
        a_col: Vec::with_capacity(n_col),
        is_deferred: false,
        a_action: [(flags & 0xff) as u8, ((flags >> 8) & 0xff) as u8],
    };
    if from_col.is_null() {
        fk.a_col.push((p.tab(tab).a_col.len() as i32 - 1, None));
    } else {
        for i in 0..n_col {
            let name = p.l(from_col).a[i].z_e_name.clone().unwrap_or_default();
            let j = p.tab(tab).a_col.iter().position(|c| c.z_cn_name.eq_ignore_ascii_case(&name));
            match j {
                Some(j) => fk.a_col.push((j as i32, None)),
                None => {
                    p.error_msg(format!("unknown column \"{name}\" in foreign key definition"));
                    return;
                }
            }
        }
    }
    if to_col.nn() {
        for i in 0..n_col {
            fk.a_col[i].1 = p.l(to_col).a[i].z_e_name.clone();
        }
    }
    // Link the foreign key to the table as the last step: pFKey->pNextFrom = p->u.tab.pFKey
    p.tabm(tab).p_fkey.insert(0, fk);
}

/// `sqlite3DeferForeignKey(pParse, isDeferred)`: this routine is called when an INITIALLY
/// IMMEDIATE or INITIALLY DEFERRED clause is seen as part of a foreign key definition. The
/// isDeferred parameter is 1 for INITIALLY DEFERRED and 0 for INITIALLY IMMEDIATE. The
/// behavior of the most recently created foreign key is adjusted accordingly.
pub fn sqlite3_defer_foreign_key(p: &mut Parse, is_deferred: i32) {
    let tab = p.p_new_table;
    if tab.is_null() || !p.tab(tab).is_ordinary() {
        return;
    }
    if let Some(fk) = p.tabm(tab).p_fkey.first_mut() {
        fk.is_deferred = is_deferred != 0;
    }
}

/// `hasColumn(aiCol, nCol, x)`: return true if any of the first nCol entries of aiCol[] is x.
fn has_column(ai_col: &[i16], n_col: usize, x: i16) -> bool {
    ai_col[..n_col].contains(&x)
}

/// `isDupColumn(pIdx, nKey, pPk, iCol)`: return true if any of the first nKey entries of
/// index pIdx exactly match the iCol-th entry of pPk. pPk is always a WITHOUT ROWID PRIMARY
/// KEY index and pIdx is an index on the same table.
fn is_dup_column(idx: &Index, n_key: usize, pk_ai_column: i16, pk_coll: &str) -> bool {
    (0..n_key).any(|i| idx.ai_column[i] == pk_ai_column && idx.az_coll[i].eq_ignore_ascii_case(pk_coll))
}

/// `recomputeColumnsNotIndexed(pIdx)`: recompute the colNotIdxed field of the Index.
fn recompute_columns_not_indexed(p: &mut Parse, ix: IndexId) {
    let tab = p.idx(ix).p_table;
    let mut m: Bitmask = 0;
    for j in (0..p.idx(ix).n_column as usize).rev() {
        let x = p.idx(ix).ai_column[j] as i32;
        if x >= 0 && (p.tab(tab).a_col[x as usize].col_flags & COLFLAG_VIRTUAL as u16) == 0 && x < BMS - 1 {
            m |= 1u64 << x;
        }
    }
    p.idxm(ix).col_not_idxed = !m;
}

/// `estimateTableWidth(pTab)`: estimate the total row width for a table.
fn estimate_table_width(p: &mut Parse, tab: TableId) {
    let t = p.tabm(tab);
    let mut w: u32 = t.a_col.iter().map(|c| c.sz_est as u32).sum();
    if t.i_p_key < 0 {
        w += 1;
    }
    t.sz_tab_row = sqlite3_log_est(w as u64 * 4);
}

/// `estimateIndexWidth(pIdx)`: estimate the average size of a row for an index.
fn estimate_index_width(p: &mut Parse, ix: IndexId) {
    let tab = p.idx(ix).p_table;
    let mut w: u32 = 0;
    for i in 0..p.idx(ix).n_column as usize {
        let x = p.idx(ix).ai_column[i];
        w += if x < 0 { 1 } else { p.tab(tab).a_col[x as usize].sz_est as u32 };
    }
    p.idxm(ix).sz_idx_row = sqlite3_log_est(w as u64 * 4);
}

/// `resizeIndexObject(pParse, pIdx, N)`: resize an Index object to hold N columns total.
/// The new `aiRowLogEst[]` holds the first nKeyCol+1 values in an area of N entries that is
/// directly followed by `aiColumn[]` (see `Index.is_resized`).
fn resize_index_object(p: &mut Parse, ix: IndexId, n: usize) {
    let x = p.idxm(ix);
    if x.n_column as usize >= n {
        return;
    }
    let n_key = x.n_key_col as usize;
    // memcpy(.., pIdx->azColl / aiColumn / aSortOrder, nColumn entries); the rest is zero
    let nc = x.n_column as usize;
    x.az_coll.truncate(nc);
    x.a_coll_static.truncate(nc);
    x.ai_column.truncate(nc);
    x.a_sort_order.truncate(nc);
    x.ai_row_log_est.truncate(n_key + 1);
    x.ai_row_log_est.resize(n, 0);
    x.az_coll.resize(n, "BINARY".to_owned());
    x.a_coll_static.resize(n, true);
    x.ai_column.resize(n, 0);
    x.a_sort_order.resize(n, 0);
    x.n_column = n as u16;
    x.is_resized = true;
}

/// `convertToWithoutRowidTable(pParse, pTab)`: this routine runs at the end of parsing a
/// CREATE TABLE statement that has a WITHOUT ROWID clause. The job of this routine is to
/// convert both the internal schema representation and the generated VDBE code so that the
/// table is really a WITHOUT ROWID table.
///
/// Changes include:
///
///     (1)  Set all columns of the PRIMARY KEY schema object to be NOT NULL.
///     (2)  Convert P3 parameter of the OP_CreateBtree from BTREE_INTKEY into BTREE_BLOBKEY.
///     (3)  Bypass the creation of the sqlite_schema table entry for the PRIMARY KEY as the
///          data btree for the table is now identified by the sqlite_schema table entry of
///          the table itself.
///     (4)  Set the Index.tnum of the PRIMARY KEY Index object in the schema to be the table
///          btree root page.
///     (5)  Add all table columns to the PRIMARY KEY Index object so that the PRIMARY KEY is
///          a covering index. The surplus columns are part of KeyInfo.nAllField and are not
///          used for sorting or lookup or uniqueness checks.
///     (6)  Replace the rowid tail on all automatically generated UNIQUE indices with the
///          PRIMARY KEY columns (for an INTEGER PRIMARY KEY this is the index created here).
fn convert_to_without_rowid_table(p: &mut Parse, tab: TableId) {
    // Mark every PRIMARY KEY column as NOT NULL (except for imposter tables)
    {
        let t = p.tabm(tab);
        for c in t.a_col.iter_mut() {
            if (c.col_flags & COLFLAG_PRIMKEY as u16) != 0 && c.not_null == OE_None as u8 {
                c.not_null = OE_Abort as u8;
            }
        }
        t.tab_flags |= TF_HasNotNull;
    }

    // Locate the PRIMARY KEY index. Or, if this table was originally an INTEGER PRIMARY KEY
    // table, create a new PRIMARY KEY index.
    let pk;
    if p.tab(tab).i_p_key >= 0 {
        let name = p.tab(tab).a_col[p.tab(tab).i_p_key as usize].z_cn_name.clone();
        let e = sqlite3_expr_alloc_str(p, TK_ID, Some(&name), 0);
        let list = sqlite3_expr_list_append(p, ExprListId::NULL, e);
        p.lm(list).a[0].sort_flags = p.i_pk_sort_order;
        p.tabm(tab).i_p_key = -1;
        let key_conf = p.tab(tab).key_conf as i32;
        sqlite3_create_index(p, None, None, SrcListId::NULL, list, key_conf, None, ExprId::NULL, 0, false, SQLITE_IDXTYPE_PRIMARYKEY as u8);
        if p.n_err != 0 {
            p.tabm(tab).tab_flags &= !TF_WithoutRowid;
            return;
        }
        pk = sqlite3_primary_key_index(p, tab);
    } else {
        pk = sqlite3_primary_key_index(p, tab);
        // Remove all redundant columns from the PRIMARY KEY. For example, change
        // "PRIMARY KEY(a,b,a,b,c,b,c,d)" into just "PRIMARY KEY(a,b,c,d)". Later code assumes
        // the PRIMARY KEY contains no repeated columns.
        let x = p.idxm(pk);
        let mut j = 1;
        for i in 1..x.n_key_col as usize {
            let (c, coll) = (x.ai_column[i], x.az_coll[i].clone());
            if is_dup_column(x, j, c, &coll) {
                x.n_column -= 1;
            } else {
                x.az_coll[j] = x.az_coll[i].clone();
                x.a_coll_static[j] = x.a_coll_static[i];
                x.a_sort_order[j] = x.a_sort_order[i];
                x.ai_column[j] = x.ai_column[i];
                j += 1;
            }
        }
        x.n_key_col = j as u16;
    }
    let n_pk;
    {
        let tnum = p.tab(tab).tnum;
        let x = p.idxm(pk);
        x.is_covering = true;
        x.uniq_not_null = true;
        x.n_column = x.n_key_col;
        n_pk = x.n_key_col as usize;
        // The root page of the PRIMARY KEY is the table root page
        x.tnum = tnum;
    }

    // Update the in-memory representation of all UNIQUE indices by converting the final
    // rowid column into one or more columns of the PRIMARY KEY.
    let (pk_cols, pk_colls, pk_static, pk_sort) = {
        let x = p.idx(pk);
        (x.ai_column[..n_pk].to_vec(), x.az_coll[..n_pk].to_vec(), x.a_coll_static[..n_pk].to_vec(), x.a_sort_order[..n_pk].to_vec())
    };
    for ix in p.tab(tab).p_index.clone() {
        if p.idx(ix).is_primary_key() {
            continue;
        }
        let n_key = p.idx(ix).n_key_col as usize;
        let n = (0..n_pk).filter(|&i| !is_dup_column(p.idx(ix), n_key, pk_cols[i], &pk_colls[i])).count();
        if n == 0 {
            // This index is a superset of the primary key
            p.idxm(ix).n_column = n_key as u16;
            continue;
        }
        resize_index_object(p, ix, n_key + n);
        let mut j = n_key;
        for i in 0..n_pk {
            if !is_dup_column(p.idx(ix), n_key, pk_cols[i], &pk_colls[i]) {
                let x = p.idxm(ix);
                x.ai_column[j] = pk_cols[i];
                x.az_coll[j] = pk_colls[i].clone();
                x.a_coll_static[j] = pk_static[i];
                if pk_sort[i] != 0 {
                    // See ticket https://sqlite.org/src/info/bba7b69f9849b5bf
                    x.b_asc_key_bug = true;
                }
                j += 1;
            }
        }
    }

    // Add all table columns to the PRIMARY KEY index
    let n_col = p.tab(tab).a_col.len();
    let n_extra = (0..n_col)
        .filter(|&i| !has_column(&p.idx(pk).ai_column, n_pk, i as i16) && (p.tab(tab).a_col[i].col_flags & COLFLAG_VIRTUAL as u16) == 0)
        .count();
    resize_index_object(p, pk, n_pk + n_extra);
    let mut j = n_pk;
    for i in 0..n_col {
        if !has_column(&p.idx(pk).ai_column, j, i as i16) && (p.tab(tab).a_col[i].col_flags & COLFLAG_VIRTUAL as u16) == 0 {
            let z_coll = sqlite3_column_coll(&p.tab(tab).a_col[i]).map(|z| z.to_owned());
            let x = p.idxm(pk);
            x.ai_column[j] = i as i16;
            x.a_coll_static[j] = z_coll.is_none();
            x.az_coll[j] = z_coll.unwrap_or_else(|| "BINARY".to_owned());
            j += 1;
        }
    }
    recompute_columns_not_indexed(p, pk);
}

/// `sqlite3EndTable(pParse, pCons, pEnd, tabOpts, pSelect)`: this routine is called to report
/// the final ")" that terminates a CREATE TABLE statement.
///
/// The table structure that other action routines have been building is added to the
/// internal hash tables, assuming no errors have occurred.
///
/// An entry for the table is made in the schema table on disk, unless this is a temporary
/// table or db->init.busy==1. When db->init.busy==1 it means we are reading the sqlite_schema
/// table on the disk. So do not write to the disk again. Extract the root page number for the
/// table from the db->init.newTnum field. (The page number should have been put there by the
/// sqliteInit.c routine.)
pub fn sqlite3_end_table(p: &mut Parse, has_end: bool, tab_opts: u32, select: SelectId) {
    if !has_end && select.is_null() {
        return;
    }
    let tab = p.p_new_table;
    if tab.is_null() {
        return;
    }
    if select.is_null() && crate::vtab::sqlite3_shadow_table_name(p, &p.tab(tab).z_name.clone()) {
        p.tabm(tab).tab_flags |= TF_Shadow;
    }

    // If the db->init.busy is 1 it means we are reading the SQL off the "sqlite_schema" or
    // "sqlite_temp_schema" table on the disk. So do not write to the disk again. Extract the
    // root page number for the table from the db->init.newTnum field.
    if p.db.init_busy {
        if select.nn() || (!p.tab(tab).is_ordinary() && p.init_new_tnum != 0) {
            p.error_msg("");
            return;
        }
        let tnum = p.init_new_tnum;
        let t = p.tabm(tab);
        t.tnum = tnum;
        if tnum == 1 {
            t.tab_flags |= TF_Readonly;
        }
    }

    // Special processing for tables that include the STRICT keyword:
    //
    //   *  Do not allow custom column datatypes. Every column must have a datatype that is
    //      one of INT, INTEGER, REAL, TEXT, ANY or BLOB.
    //
    //   *  If a PRIMARY KEY is defined, other than the INTEGER PRIMARY KEY, then all columns
    //      of the PRIMARY KEY must have a NOT NULL constraint.
    if tab_opts & TF_Strict != 0 {
        p.tabm(tab).tab_flags |= TF_Strict;
        for ii in 0..p.tab(tab).a_col.len() {
            let (e_c_type, col_flags) = (p.tab(tab).a_col[ii].e_c_type, p.tab(tab).a_col[ii].col_flags);
            if e_c_type == COLTYPE_CUSTOM as u8 {
                let t = p.tab(tab);
                let msg = if col_flags & COLFLAG_HASTYPE as u16 != 0 {
                    format!(
                        "unknown datatype for {}.{}: \"{}\"",
                        t.z_name,
                        t.a_col[ii].z_cn_name,
                        sqlite3_column_type(&t.a_col[ii], Some("")).unwrap_or("")
                    )
                } else {
                    format!("missing datatype for {}.{}", t.z_name, t.a_col[ii].z_cn_name)
                };
                p.error_msg(msg);
                return;
            } else if e_c_type == COLTYPE_ANY as u8 {
                p.tabm(tab).a_col[ii].affinity = SQLITE_AFF_BLOB;
            }
            if (col_flags & COLFLAG_PRIMKEY as u16) != 0
                && p.tab(tab).i_p_key as i32 != ii as i32
                && p.tab(tab).a_col[ii].not_null == OE_None as u8
            {
                let t = p.tabm(tab);
                t.a_col[ii].not_null = OE_Abort as u8;
                t.tab_flags |= TF_HasNotNull;
            }
        }
    }

    // Special processing for WITHOUT ROWID Tables
    if tab_opts & TF_WithoutRowid != 0 {
        if p.tab(tab).tab_flags & TF_Autoincrement != 0 {
            p.error_msg("AUTOINCREMENT not allowed on WITHOUT ROWID tables");
            return;
        }
        if p.tab(tab).tab_flags & TF_HasPrimaryKey == 0 {
            let msg = format!("PRIMARY KEY missing on table {}", p.tab(tab).z_name);
            p.error_msg(msg);
            return;
        }
        p.tabm(tab).tab_flags |= TF_WithoutRowid | TF_NoVisibleRowid;
        convert_to_without_rowid_table(p, tab);
    }

    // Resolve names in all CHECK constraint expressions.
    if p.tab(tab).p_check.nn() {
        let check = p.tab(tab).p_check;
        crate::resolve::sqlite3_resolve_self_reference(p, tab, NC_IsCheck, ExprId::NULL, check);
        if p.n_err != 0 {
            // If errors are seen, delete the CHECK constraints now, else they might actually
            // be used if PRAGMA writable_schema=ON is set.
            p.tabm(tab).p_check = ExprListId::NULL;
        }
    }
    if p.tab(tab).tab_flags & TF_HasGenerated != 0 {
        let mut n_ng = 0;
        for ii in 0..p.tab(tab).a_col.len() {
            if p.tab(tab).a_col[ii].col_flags & COLFLAG_GENERATED as u16 != 0 {
                let x = sqlite3_column_expr(p, tab, ii);
                if crate::resolve::sqlite3_resolve_self_reference(p, tab, NC_GenCol, x, ExprListId::NULL) != 0 {
                    // If there are errors in resolving the expression, change the expression
                    // to a NULL.
                    let null = sqlite3_expr_alloc_str(p, TK_NULL, None, 0);
                    sqlite3_column_set_expr(p, tab, ii, null);
                }
            } else {
                n_ng += 1;
            }
        }
        if n_ng == 0 {
            p.error_msg("must have at least one non-generated column");
            return;
        }
    }

    // Estimate the average row size for the table and for all implied indices
    estimate_table_width(p, tab);
    for ix in p.tab(tab).p_index.clone() {
        estimate_index_width(p, ix);
    }

    // Add the table to the in-memory representation of the database.
    if p.db.init_busy {
        let i_db = p.tab(tab).i_db as usize;
        let key = p.tab(tab).z_name.to_ascii_lowercase();
        p.a_schema[i_db].tbl_hash.insert(key, tab);
        p.p_new_table = TableId::NULL;
    }
}

/// `sqlite3CreateView(pParse, pBegin, pName1, pName2, pCNames, pSelect, isTemp, noErr)`: the
/// parser calls this routine in order to create a new VIEW.
pub fn sqlite3_create_view(p: &mut Parse, name1: &Token, name2: &Token, c_names: ExprListId, select: SelectId, is_temp: bool, no_err: bool) {
    if p.n_var > 0 {
        p.error_msg("parameters are not allowed in views");
        return;
    }
    sqlite3_start_table(p, name1, name2, is_temp, true, false, no_err);
    let tab = p.p_new_table;
    if tab.is_null() || p.n_err != 0 {
        return;
    }

    // Legacy versions of SQLite allowed the use of the magic "rowid" column on a view, even
    // though views do not have rowids. SQLITE_ALLOW_ROWID_IN_VIEW is not compiled: never
    // allow rowid in view.
    p.tabm(tab).tab_flags |= TF_NoVisibleRowid;

    let i_db = p.tab(tab).i_db;
    // sqlite3FixInit(&sFix, pParse, iDb, "view", pName); sqlite3FixSelect(&sFix, pSelect)
    if fix_select(p, select, i_db) {
        return;
    }

    // Make a copy of the entire SELECT statement that defines the view. This will force all
    // the Expr.token.z values to be dynamically allocated rather than point to the input
    // string - which means that they will persist after the current sqlite3_exec() call
    // returns.
    p.sm(select).sel_flags |= SF_View;
    let dup = sqlite3_select_dup(p, select, EXPRDUP_REDUCE);
    let check = sqlite3_expr_list_dup(p, c_names, EXPRDUP_REDUCE);
    {
        let t = p.tabm(tab);
        t.p_select = dup;
        t.p_check = check;
        t.e_tab_type = TABTYP_VIEW as u8;
    }
    // Use sqlite3EndTable() to add the view to the schema table
    sqlite3_end_table(p, true, 0, SelectId::NULL);
}

/// `sqlite3CreateIndex(pParse, pName1, pName2, pTblName, pList, onError, pStart, pPIWhere,
/// sortOrder, ifNotExist, idxType)`: create a new index for an SQL table. pName1.pName2 is
/// the name of the index and pTblName is the name of the table that is to be indexed. Both
/// will be NULL for a primary key or an index that is created to satisfy a UNIQUE constraint.
/// If pTable and pIndex are NULL, use pParse->pNewTable as the table to be indexed.
/// pParse->pNewTable is a table that is currently being constructed by a CREATE TABLE
/// statement.
///
/// pList is a list of columns to be indexed. pList will be NULL if this is a primary key or
/// unique-constraint on the most recent column added to the table currently under
/// construction.
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_create_index(
    p: &mut Parse,
    name1: Option<&Token>,
    name2: Option<&Token>,
    tbl_name: SrcListId,
    mut list: ExprListId,
    on_error: i32,
    _start: Option<&Token>,
    pi_where: ExprId,
    sort_order: i32,
    if_not_exist: bool,
    idx_type: u8,
) {
    if p.n_err != 0 {
        return;
    }
    if p.in_declare_vtab() && idx_type != SQLITE_IDXTYPE_PRIMARYKEY as u8 {
        return;
    }
    if !p.db.init_busy {
        // (code generation for CREATE INDEX is a later step; the PRIMARY KEY of a WITHOUT
        // ROWID table declared by a virtual table is not ported either)
        p.not_supported("statement kind (grammar rule not ported)");
        return;
    }
    if sqlite3_has_explicit_nulls(p, list) {
        return;
    }

    // Find the table that is to be indexed. Return early if not found.
    let tab;
    let i_db: usize;
    let mut pk = IndexId::NULL; // PRIMARY KEY index for WITHOUT ROWID tables
    let mut name = Token::EMPTY;
    let mut have_name = false;
    if tbl_name.nn() {
        // Use the two-part index name to determine the database to search for the table.
        // 'Fix' the table name to this db before looking up the table.
        let (n1, n2) = (*name1.expect("index name"), *name2.expect("index name"));
        let d = sqlite3_two_part_name(p, &n1, &n2, &mut name);
        if d < 0 {
            return;
        }
        have_name = true;
        i_db = d as usize;
        // sqlite3FixInit(&sFix, pParse, iDb, "index", pName); sqlite3FixSrcList(&sFix, pTblName)
        if i_db != 1 {
            let it = &mut p.srcm(tbl_name).a[0];
            it.i_fixed_db = i_db as i32;
            it.from_ddl = true;
            it.fixed_schema = true;
        }
        tab = sqlite3_locate_table_item(p, 0, tbl_name, 0);
        if tab.is_null() {
            return;
        }
        if i_db == 1 && p.tab(tab).i_db != 1 {
            let msg = format!("cannot create a TEMP index on non-TEMP table \"{}\"", p.tab(tab).z_name);
            p.error_msg(msg);
            return;
        }
        if !p.tab(tab).has_rowid() {
            pk = sqlite3_primary_key_index(p, tab);
        }
    } else {
        tab = p.p_new_table;
        if tab.is_null() {
            return;
        }
        i_db = p.tab(tab).i_db as usize;
    }

    if p.tab(tab).is_view() {
        p.error_msg("views may not be indexed");
        return;
    }
    if p.tab(tab).is_virtual() {
        p.error_msg("virtual tables may not be indexed");
        return;
    }

    // Find the name of the index. Make sure there is not already another index or table with
    // the same name.
    //
    // Exception: If we are reading the names of permanent indices from the sqlite_schema
    // table (because some other process changed the schema) and one of the index names
    // collides with the name of a temporary table or index, then we will continue to process
    // this index.
    //
    // If pName==0 it means that we are dealing with a primary key or UNIQUE constraint. We
    // have to invent our own name.
    let z_name: String;
    if have_name {
        z_name = sqlite3_dequote(p.tok_str(&name));
        let tname = p.tab(tab).z_name.clone();
        if sqlite3_check_object_name(p, &z_name, "index", &tname) {
            return;
        }
        let zdb = p.db.a_db[i_db].z_db_s_name.clone();
        if sqlite3_find_index(p, &z_name, Some(&zdb)).nn() {
            if !if_not_exist {
                p.error_msg(format!("index {z_name} already exists"));
            } else {
                p.error_msg(""); // corruptSchema() will do the error
            }
            return;
        }
    } else {
        let n = p.tab(tab).p_index.len() + 1;
        z_name = format!("sqlite_autoindex_{}_{}", p.tab(tab).z_name, n);
    }

    // If pList==0, it means this routine was called to make a primary key out of the last
    // column added to the table under construction. So create a fake list to simulate this.
    if list.is_null() {
        let n_col = p.tab(tab).a_col.len();
        p.tabm(tab).a_col[n_col - 1].col_flags |= COLFLAG_UNIQUE as u16;
        let cname = p.tab(tab).a_col[n_col - 1].z_cn_name.clone();
        let e = sqlite3_expr_alloc_str(p, TK_ID, Some(&cname), 0);
        list = sqlite3_expr_list_append(p, ExprListId::NULL, e);
        sqlite3_expr_list_set_sort_order(p, list, sort_order, SQLITE_SO_UNDEFINED);
    } else {
        sqlite3_expr_list_check_length(p, list, "index");
        if p.n_err != 0 {
            return;
        }
    }

    // Allocate the index structure.
    let n_list = p.l(list).a.len();
    let n_extra_col = if pk.nn() { p.idx(pk).n_key_col as usize } else { 1 };
    let n_alloc = n_list + n_extra_col;
    let mut x = Index {
        z_name: z_name.clone(),
        p_table: tab,
        on_error: on_error as u8,
        uniq_not_null: on_error != OE_None,
        idx_type,
        n_key_col: n_list as u16,
        n_column: n_alloc as u16,
        ai_column: vec![0; n_alloc],
        az_coll: vec!["BINARY".to_owned(); n_alloc],
        a_coll_static: vec![true; n_alloc],
        a_sort_order: vec![0; n_alloc],
        // sqlite3AllocateIndexObject(): aiRowLogEst[] has nCol+1 entries
        ai_row_log_est: vec![0; n_alloc + 1],
        i_db: i_db as i32,
        ..Index::default()
    };
    if pi_where.nn() {
        crate::resolve::sqlite3_resolve_self_reference(p, tab, NC_PartIdx, pi_where, ExprListId::NULL);
        x.p_part_idx_where = pi_where;
    }

    // Check to see if we should honor DESC requests on index columns
    let sort_order_mask: u8 = if p.db.a_db[i_db].file_format >= 4 { 0xff } else { 0 };

    // Analyze the list of expressions that form the terms of the index and report any
    // errors. In the common case where the expression is exactly a table column, store that
    // column in aiColumn[]. For general expressions, populate pIndex->aColExpr and store
    // XN_EXPR (-2) in aiColumn[].
    //
    // TODO: Issue a warning if two or more columns of the index are identical.
    // TODO: Issue a warning if the table primary key is used as part of the index key.
    for i in 0..n_list {
        let le = p.l(list).a[i].p_expr;
        sqlite3_string_to_id(p, le);
        crate::resolve::sqlite3_resolve_self_reference(p, tab, NC_IdxExpr, le, ExprListId::NULL);
        if p.n_err != 0 {
            return;
        }
        let ce = sqlite3_expr_skip_collate(p, le);
        let j: i32;
        if p.e(ce).op != TK_COLUMN {
            if tab == p.p_new_table {
                p.error_msg("expressions prohibited in PRIMARY KEY and UNIQUE constraints");
                return;
            }
            if x.a_col_expr.is_null() {
                x.a_col_expr = list;
            }
            j = XN_EXPR;
            x.ai_column[i] = XN_EXPR as i16;
            x.uniq_not_null = false;
            x.b_has_expr = true;
        } else {
            let mut jj = p.e(ce).i_column as i32;
            if jj < 0 {
                jj = p.tab(tab).i_p_key as i32;
            } else {
                if p.tab(tab).a_col[jj as usize].not_null == 0 {
                    x.uniq_not_null = false;
                }
                if p.tab(tab).a_col[jj as usize].col_flags & COLFLAG_VIRTUAL as u16 != 0 {
                    x.b_has_v_col = true;
                    x.b_has_expr = true;
                }
            }
            j = jj;
            x.ai_column[i] = j as i16;
        }
        let mut z_coll: Option<String> = None;
        if p.e(le).op == TK_COLLATE {
            z_coll = p.e(le).z_token.clone();
        } else if j >= 0 {
            z_coll = sqlite3_column_coll(&p.tab(tab).a_col[j as usize]).map(|z| z.to_owned());
        }
        // if( !zColl ) zColl = sqlite3StrBINARY
        x.a_coll_static[i] = z_coll.is_none();
        x.az_coll[i] = z_coll.unwrap_or_else(|| "BINARY".to_owned());
        x.a_sort_order[i] = p.l(list).a[i].sort_flags & sort_order_mask;
    }

    // Append the table key to the end of the index. For WITHOUT ROWID tables (when pPk!=0)
    // this will be the declared PRIMARY KEY. For normal tables (when pPk==0) this will be the
    // rowid.
    let mut i = n_list;
    if pk.nn() {
        let (pk_n, pk_cols, pk_colls, pk_static, pk_sort) = {
            let k = p.idx(pk);
            let n = k.n_key_col as usize;
            (n, k.ai_column[..n].to_vec(), k.az_coll[..n].to_vec(), k.a_coll_static[..n].to_vec(), k.a_sort_order[..n].to_vec())
        };
        for j in 0..pk_n {
            if is_dup_column(&x, x.n_key_col as usize, pk_cols[j], &pk_colls[j]) {
                x.n_column -= 1;
            } else {
                x.ai_column[i] = pk_cols[j];
                x.az_coll[i] = pk_colls[j].clone();
                x.a_coll_static[i] = pk_static[j];
                x.a_sort_order[i] = pk_sort[j];
                i += 1;
            }
        }
    } else {
        x.ai_column[i] = XN_ROWID as i16;
        x.az_coll[i] = "BINARY".to_owned();
        x.a_coll_static[i] = true;
    }
    let ix = p.new_idx(x);
    {
        let mut n = p.tab(tab).n_row_log_est;
        sqlite3_default_row_est(p.idxm(ix), &mut n);
        p.tabm(tab).n_row_log_est = n;
    }
    if p.p_new_table.is_null() {
        estimate_index_width(p, ix);
    }

    // If this index contains every column of its table, then mark it as a covering index
    recompute_columns_not_indexed(p, ix);
    if tbl_name.nn() && p.idx(ix).n_column as usize >= p.tab(tab).a_col.len() {
        let mut cov = true;
        for j in 0..p.tab(tab).a_col.len() {
            if j as i32 == p.tab(tab).i_p_key as i32 {
                continue;
            }
            if sqlite3_table_column_to_index(p.idx(ix), j as i32) >= 0 {
                continue;
            }
            cov = false;
            break;
        }
        p.idxm(ix).is_covering = cov;
    }

    if tab == p.p_new_table {
        // This routine has been called to create an automatic index as a result of a PRIMARY
        // KEY or UNIQUE clause on a column definition, or a UNIQUE or PRIMARY KEY constraint
        // defined as part of a CREATE TABLE statement. Since the table has just been created,
        // it contains no data and the index initialization step can be skipped.
        //
        // If there are different collating sequences or if the columns of the constraint
        // occur in different orders, then the constraints are considered distinct and both
        // result in separate indices.
        for other in p.tab(tab).p_index.clone() {
            let (a, b) = (p.idx(other), p.idx(ix));
            if a.n_key_col != b.n_key_col {
                continue;
            }
            let n = a.n_key_col as usize;
            let mut k = 0;
            while k < n {
                if a.ai_column[k] != b.ai_column[k] {
                    break;
                }
                if !a.az_coll[k].eq_ignore_ascii_case(&b.az_coll[k]) {
                    break;
                }
                k += 1;
            }
            if k == n {
                let (ae, be) = (a.on_error as i32, b.on_error as i32);
                if ae != be {
                    // This constraint creates the same index as a previous constraint
                    // specified somewhere in the CREATE TABLE statement. However the ON
                    // CONFLICT clauses are different. If both this constraint and the
                    // previous equivalent constraint have explicit ON CONFLICT clauses this
                    // is an error. Otherwise, use the explicitly specified behavior for the
                    // index.
                    if !(ae == OE_Default || be == OE_Default) {
                        p.error_msg("conflicting ON CONFLICT clauses specified");
                    }
                    if ae == OE_Default {
                        p.idxm(other).on_error = be as u8;
                    }
                }
                if idx_type == SQLITE_IDXTYPE_PRIMARYKEY as u8 {
                    p.idxm(other).idx_type = idx_type;
                }
                // (the new Index object is dropped)
                reorder_replace_indexes(p, tab);
                return;
            }
        }
    }

    // Link the new Index structure to its table and to the other in-memory database
    // structures.
    if tbl_name.nn() {
        p.idxm(ix).tnum = p.init_new_tnum;
        // (sqlite3IndexHasDuplicateRootPage: the engine opened the file; a corrupt schema is
        // not the generator's to detect)
    }
    p.a_schema[i_db].idx_hash.insert(z_name.to_ascii_lowercase(), ix);
    p.tabm(tab).p_index.insert(0, ix);
    reorder_replace_indexes(p, tab);
}

/// The clean-up of `sqlite3CreateIndex()` (`exit_create_index:`): ensure all REPLACE indexes
/// on pTab are at the end of the pIndex list. The list was already ordered when this routine
/// was entered, so at this point at most a single index (the newly added index) will be out
/// of order. So we have to reorder at most one index.
fn reorder_replace_indexes(p: &mut Parse, tab: TableId) {
    let mut list = p.tab(tab).p_index.clone();
    let mut i = 0;
    while i < list.len() {
        if p.idx(list[i]).on_error != OE_Replace as u8 {
            i += 1;
            continue;
        }
        while i + 1 < list.len() && p.idx(list[i + 1]).on_error != OE_Replace as u8 {
            list.swap(i, i + 1);
            i += 1;
        }
        break;
    }
    p.tabm(tab).p_index = list;
}

// ---------------------------------------------------------------------------------------------
// attach.c: sqlite3FixSelect
// ---------------------------------------------------------------------------------------------

/// `fixExprCb(p, pExpr)` (walker callback of sqlite3FixSelect; `b1` is DbFixer.bTemp).
fn fix_expr_cb(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if !w.b1 {
        p.em(e).flags |= EP_FromDDL;
    }
    if p.e(e).op == TK_VARIABLE {
        // db->init.busy: a bound parameter of a schema object reads as NULL
        p.em(e).op = TK_NULL;
    }
    WRC_Continue
}

/// `fixSelectCb(p, pSelect)` (`n` is the database of the schema object, `b1` is
/// DbFixer.bTemp): every table reference of a non-TEMP view is tied to the database of the
/// view.
fn fix_select_cb(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let src = p.s(sel).p_src;
    if src.is_null() {
        return WRC_Continue;
    }
    for i in 0..p.src(src).a.len() {
        if !w.b1 && !p.src(src).a[i].is_subquery {
            if !p.src(src).a[i].fixed_schema
                && let Some(zdb) = p.src(src).a[i].z_database.clone()
            {
                let i_db2 = p.db.a_db.iter().position(|d| d.z_db_s_name.eq_ignore_ascii_case(&zdb)).map_or(-1, |x| x as i32);
                if w.n != i_db2 {
                    let msg = format!("view {} cannot reference objects in database {}", p.init_az[1], zdb);
                    p.error_msg(msg);
                    return WRC_Abort;
                }
                let it = &mut p.srcm(src).a[i];
                it.z_database = None;
                it.not_cte = true;
                it.had_schema = true;
            }
            let it = &mut p.srcm(src).a[i];
            it.i_fixed_db = w.n;
            it.from_ddl = true;
            it.fixed_schema = true;
        }
        let (is_using, on) = (p.src(src).a[i].is_using, p.src(src).a[i].p_on);
        if !is_using && crate::walker::sqlite3_walk_expr(p, w, on) != 0 {
            return WRC_Abort;
        }
    }
    let with = p.s(sel).p_with;
    if with.nn() {
        for i in 0..p.a_with[with.ix()].a.len() {
            let s = p.a_with[with.ix()].a[i].p_select;
            if crate::walker::sqlite3_walk_select(p, w, s) != 0 {
                return WRC_Abort;
            }
        }
    }
    WRC_Continue
}

/// `sqlite3FixSelect(pFix, pSelect)` after `sqlite3FixInit(pFix, pParse, iDb, "view", pName)`.
/// Returns true on error.
fn fix_select(p: &mut Parse, sel: SelectId, i_db: i32) -> bool {
    // (xSelectCallback2 = sqlite3WalkWinDefnDummyCallback: the window definitions are walked)
    let mut w = Walker {
        x_expr_callback: Some(fix_expr_cb),
        x_select_callback: Some(fix_select_cb),
        n: i_db,
        b1: i_db == 1,
        walk_win_defn: true,
        ..Walker::default()
    };
    crate::walker::sqlite3_walk_select(p, &mut w, sel) != 0
}

// ---------------------------------------------------------------------------------------------
// prepare.c: schema loading
// ---------------------------------------------------------------------------------------------

/// Run the parser over the SQL text of one `sqlite_schema` row the way `sqlite3InitCallback()`
/// does (`sqlite3Prepare()` with `db->init.busy`, a Parse of its own in C): the schema object
/// is built in the arenas of this Parse, no code is generated, and the counters of the
/// statement being compiled are untouched. Returns false if the text does not parse.
fn init_parse(p: &mut Parse, text: &str) -> bool {
    let saved_sql = std::mem::replace(&mut p.z_sql, text.to_owned());
    let saved_tail = p.z_tail;
    let saved_last = p.s_last_token;
    let saved_rc = p.rc;
    let saved_n_select = p.n_select;
    let saved_explain = p.explain;
    let saved_exists = p.b_has_exists;
    let saved_with = p.b_has_with;
    let saved_compound = p.has_compound;
    let saved_n_var = p.n_var;
    let saved_vlist = std::mem::take(&mut p.p_v_list);
    let saved_p_with = p.p_with;
    let saved_dump = p.tree_dump.take();
    let saved_init = p.db.init_busy;
    let saved_err = p.z_err_msg.take();
    let saved_n_err = p.n_err;
    let saved_ofs = p.err_byte_offset;
    let saved_unsupported = p.unsupported.take();
    let saved_check = p.check_schema;
    let saved_nc = p.a_nc.len();
    p.n_err = 0;
    p.n_select = 0;
    p.n_var = 0;
    p.explain = 0;
    p.p_with = WithId::NULL;
    p.db.init_busy = true;
    p.p_new_table = TableId::NULL;
    p.cr_constraint_name = Token::EMPTY;
    p.i_pk_sort_order = 0;
    crate::tokenize::sqlite3_run_parser(p);
    let failed = p.z_err_msg.is_some() || p.n_err != 0 || p.unsupported.is_some();
    if failed {
        // (diagnostics for the schema dump: what the row failed with)
        let msg = format!("{}: {}", p.init_az[1], p.unsupported.map(|u| u.to_owned()).or(p.z_err_msg.clone()).unwrap_or_default());
        p.init_errors.push(msg);
    }
    p.init_fail = p.unsupported.take().or(if failed { Some("schema object whose SQL text the generator could not load") } else { None });
    p.p_new_table = TableId::NULL;
    p.a_nc.truncate(saved_nc);
    p.z_sql = saved_sql;
    p.z_tail = saved_tail;
    p.s_last_token = saved_last;
    p.rc = saved_rc;
    p.n_select = saved_n_select;
    p.explain = saved_explain;
    p.b_has_exists = saved_exists;
    p.b_has_with = saved_with;
    p.has_compound = saved_compound;
    p.n_var = saved_n_var;
    p.p_v_list = saved_vlist;
    p.p_with = saved_p_with;
    p.tree_dump = saved_dump;
    p.db.init_busy = saved_init;
    p.z_err_msg = saved_err;
    p.n_err = saved_n_err;
    p.err_byte_offset = saved_ofs;
    p.unsupported = saved_unsupported;
    p.check_schema = saved_check;
    !failed
}

/// The parse of `sqlite3_declare_vtab()`: `sqlite3RunParser()` over `text` with
/// `eParseMode = PARSE_MODE_DECLARE_VTAB` and `db->init.busy` cleared, on a parse object of
/// its own (here: the state of `p` that the parse touches is saved and restored). Returns the
/// table the statement built (`sParse.pNewTable`), or None if the parse failed.
pub fn declare_vtab_parse(p: &mut Parse, text: &str) -> Option<TableId> {
    let saved_sql = std::mem::replace(&mut p.z_sql, text.to_owned());
    let saved_tail = p.z_tail;
    let saved_last = p.s_last_token;
    let saved_rc = p.rc;
    let saved_n_select = p.n_select;
    let saved_explain = p.explain;
    let saved_exists = p.b_has_exists;
    let saved_with = p.b_has_with;
    let saved_compound = p.has_compound;
    let saved_n_var = p.n_var;
    let saved_vlist = std::mem::take(&mut p.p_v_list);
    let saved_p_with = p.p_with;
    let saved_dump = p.tree_dump.take();
    let saved_init = p.db.init_busy;
    let saved_err = p.z_err_msg.take();
    let saved_n_err = p.n_err;
    let saved_ofs = p.err_byte_offset;
    let saved_unsupported = p.unsupported.take();
    let saved_check = p.check_schema;
    let saved_nc = p.a_nc.len();
    let saved_mode = p.e_parse_mode;
    let saved_new = p.p_new_table;
    let saved_cname = p.cr_constraint_name;
    let saved_pk_order = p.i_pk_sort_order;
    let saved_name_tok = p.s_name_token;
    let saved_nested = p.nested;
    let saved_n_query_loop = p.n_query_loop;
    let saved_s_arg = p.s_arg;
    p.n_err = 0;
    p.rc = SQLITE_OK;
    p.n_select = 0;
    p.n_var = 0;
    p.explain = 0;
    p.nested = 0;
    p.p_with = WithId::NULL;
    p.e_parse_mode = PARSE_MODE_DECLARE_VTAB as u8;
    p.db.init_busy = false;
    p.n_query_loop = 1;
    p.p_new_table = TableId::NULL;
    p.cr_constraint_name = Token::EMPTY;
    p.i_pk_sort_order = 0;
    crate::tokenize::sqlite3_run_parser(p);
    let failed = p.z_err_msg.is_some() || p.n_err != 0 || p.unsupported.is_some();
    let new = if failed { None } else { Some(p.p_new_table).filter(|t| t.nn()) };
    p.p_new_table = saved_new;
    p.a_nc.truncate(saved_nc);
    p.z_sql = saved_sql;
    p.z_tail = saved_tail;
    p.s_last_token = saved_last;
    p.rc = saved_rc;
    p.n_select = saved_n_select;
    p.explain = saved_explain;
    p.b_has_exists = saved_exists;
    p.b_has_with = saved_with;
    p.has_compound = saved_compound;
    p.n_var = saved_n_var;
    p.p_v_list = saved_vlist;
    p.p_with = saved_p_with;
    p.tree_dump = saved_dump;
    p.db.init_busy = saved_init;
    p.z_err_msg = saved_err;
    p.n_err = saved_n_err;
    p.err_byte_offset = saved_ofs;
    p.unsupported = saved_unsupported;
    p.check_schema = saved_check;
    p.e_parse_mode = saved_mode;
    p.cr_constraint_name = saved_cname;
    p.i_pk_sort_order = saved_pk_order;
    p.s_name_token = saved_name_tok;
    p.nested = saved_nested;
    p.n_query_loop = saved_n_query_loop;
    p.s_arg = saved_s_arg;
    new
}

/// `sqlite3InitCallback(pInit, argc, argv, NotUsed)`: this is the callback routine for the
/// code that initializes the database. See sqlite3Init() below for additional information.
/// This routine is also called from the OP_ParseSchema opcode of the VDBE.
///
/// Each callback contains the following information:
///
///     argv[0] = type of object: "table", "index", "trigger", or "view".
///     argv[1] = name of thing being created
///     argv[2] = associated table if an index or trigger
///     argv[3] = root page number for table or index. 0 for trigger or view.
///     argv[4] = SQL text for the CREATE statement.
fn sqlite3_init_callback(p: &mut Parse, i_db: usize, row: &SchemaRow) {
    let is_create = row.sql.as_deref().is_some_and(|s| {
        let b = s.as_bytes();
        b.len() >= 2 && b[0].eq_ignore_ascii_case(&b'c') && b[1].eq_ignore_ascii_case(&b'r')
    });
    if is_create {
        let sql = row.sql.clone().unwrap_or_default();
        // The parts of C's parser that are not ported yet: a trigger is only noted on its
        // table.
        if row.ty.eq_ignore_ascii_case("trigger") {
            p.a_schema[i_db].triggers.push(row.tbl_name.to_ascii_lowercase());
            return;
        }
        // Call the parser to process a CREATE TABLE, INDEX or VIEW. But because
        // db->init.busy is set to 1, no VDBE code is generated or executed. All the parser
        // does is build the internal data structures that describe the table, index, or view.
        p.init_i_db = i_db;
        p.init_new_tnum = row.rootpage;
        p.init_az = [row.ty.clone(), row.name.clone(), row.tbl_name.clone()];
        if !init_parse(p, &sql) {
            // C loaded this row without error (the engine runs on the same file): what failed
            // is a construct that is not ported. Statements that use the object, or the
            // table of the index, are reported as unsupported.
            let why = p.init_fail.take().unwrap_or("schema object whose SQL text the generator could not load");
            p.a_schema[i_db].bad.insert(row.name.to_ascii_lowercase(), why);
            p.a_schema[i_db].bad.insert(row.tbl_name.to_ascii_lowercase(), why);
        }
        p.init_i_db = 0;
    } else if row.sql.as_deref().is_some_and(|s| !s.is_empty()) {
        // corruptSchema(pData, argv, 0)
        p.a_schema[i_db].bad.insert(row.name.to_ascii_lowercase(), "schema row that is not a CREATE statement");
    } else {
        // If the SQL column is blank it means this is an index that was created to be the
        // PRIMARY KEY or to fulfill a UNIQUE constraint for a CREATE TABLE. The index should
        // have already been created when we processed the CREATE TABLE. All we have to do
        // here is record the root page number for that index.
        let zdb = p.db.a_db[i_db].z_db_s_name.clone();
        let ix = sqlite3_find_index(p, &row.name, Some(&zdb));
        if ix.is_null() || row.rootpage < 2 {
            p.a_schema[i_db].bad.insert(row.tbl_name.to_ascii_lowercase(), "automatic index row without its index");
        } else {
            p.idxm(ix).tnum = row.rootpage;
        }
    }
}

/// `sqlite3InitOne(db, iDb, pzErrMsg, mFlags)`: attempt to read the database schema and
/// initialize internal data structures for a single database file. The index of the database
/// file is given by iDb. iDb==0 is used for the main database. iDb==1 should never be used.
/// iDb>=2 is used for auxiliary databases.
fn sqlite3_init_one(p: &mut Parse, i_db: usize) {
    // Construct the in-memory representation schema tables (sqlite_schema or
    // sqlite_temp_schema) by invoking the parser directly. The appropriate table definition
    // is created by the parser.
    let schema_tab = if i_db == 1 { "sqlite_temp_master" } else { "sqlite_master" };
    let row0 = SchemaRow {
        ty: "table".to_owned(),
        name: schema_tab.to_owned(),
        tbl_name: schema_tab.to_owned(),
        rootpage: 1,
        sql: Some("CREATE TABLE x(type text,name text,tbl_name text,rootpage int,sql text)".to_owned()),
    };
    sqlite3_init_callback(p, i_db, &row0);

    // Create a cursor to hold the database open: if( pDb->pBt==0 ){ DbSetProperty(db, 1,
    // DB_SchemaLoaded); return } — the temp database before it is opened.
    if !p.db.a_db[i_db].has_bt {
        p.a_schema[i_db].loaded = true;
        return;
    }

    // Read the schema information out of the schema tables:
    // "SELECT*FROM\"%w\".%s ORDER BY rowid"
    let host = p.db.host;
    for row in host.schema_rows(i_db) {
        sqlite3_init_callback(p, i_db, &row);
    }
    sqlite3_analysis_load(p, i_db);
    for t in std::mem::take(&mut p.a_schema[i_db].triggers) {
        if let Some(&tab) = p.a_schema[i_db].tbl_hash.get(&t) {
            p.tabm(tab).has_trigger = true;
        }
    }
    p.a_schema[i_db].loaded = true;
}

/// `sqlite3Init(db, pzErrMsg)`: initialize all database files - the main database file, the
/// file used to store temporary tables, and any additional database files created using
/// ATTACH statements. Return a success code. If an error occurs, write an error message into
/// *pzErrMsg.
///
/// After a database is initialized, the DB_SchemaLoaded bit is set in the flags field of the
/// Db structure. If the database file was of zero-length, then the DB_Empty flag is also
/// set.
pub fn sqlite3_init(p: &mut Parse) {
    let n_db = p.db.a_db.len();
    while p.a_schema.len() < n_db {
        p.a_schema.push(SchemaState::default());
    }
    // Do the main schema first
    if !p.a_schema[0].loaded {
        sqlite3_init_one(p, 0);
    }
    // All other schemas after the main schema. The "temp" schema must be last
    for i in (1..n_db).rev() {
        if !p.a_schema[i].loaded {
            sqlite3_init_one(p, i);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// analyze.c: loading sqlite_stat1
// ---------------------------------------------------------------------------------------------

/// `decodeIntArray(zIntArray, nOut, aOut, aLog, pIndex)`: the first argument points to a
/// nul-terminated string containing a list of space separated integers. Read the first nOut
/// of these into the array aLog[] as LogEst values. If the list has more entries, the
/// "unordered", "sz=N" and "noskipscan" arguments are applied to the index.
///
/// Returns (unordered, noskipscan, sz) as found in the tail when `has_index`.
fn decode_int_array(z: &str, n_out: usize, a_log: &mut [LogEst]) -> (bool, bool, Option<LogEst>) {
    let b = z.as_bytes();
    let mut pos = 0;
    let mut i = 0;
    while pos < b.len() && i < n_out {
        let mut v: u64 = 0;
        while pos < b.len() && b[pos].is_ascii_digit() {
            v = v.wrapping_mul(10).wrapping_add((b[pos] - b'0') as u64);
            pos += 1;
        }
        a_log[i] = sqlite3_log_est(v);
        i += 1;
        if pos < b.len() && b[pos] == b' ' {
            pos += 1;
        }
    }
    let (mut unordered, mut noskip, mut sz) = (false, false, None);
    while pos < b.len() {
        let rest = &z[pos..];
        if sqlite3_strglob_prefix(rest, "unordered") {
            unordered = true;
        } else if rest.len() >= 3 && rest.as_bytes()[..3].eq_ignore_ascii_case(b"sz=") && rest.as_bytes().get(3).is_some_and(|c| c.is_ascii_digit()) {
            let mut v: i64 = 0;
            for &c in &rest.as_bytes()[3..] {
                if !c.is_ascii_digit() {
                    break;
                }
                v = v.saturating_mul(10).saturating_add((c - b'0') as i64);
            }
            // sqlite3Atoi(z+3): a 32-bit integer; if( sz<2 ) sz = 2
            let mut v = if v > i32::MAX as i64 { 0 } else { v };
            if v < 2 {
                v = 2;
            }
            sz = Some(sqlite3_log_est(v as u64));
        } else if sqlite3_strglob_prefix(rest, "noskipscan") {
            noskip = true;
        }
        while pos < b.len() && b[pos] != b' ' {
            pos += 1;
        }
        while pos < b.len() && b[pos] == b' ' {
            pos += 1;
        }
    }
    (unordered, noskip, sz)
}

/// `sqlite3_strglob("<word>*", z)==0`: z starts with the word (case sensitive, as GLOB).
fn sqlite3_strglob_prefix(z: &str, word: &str) -> bool {
    z.as_bytes().starts_with(word.as_bytes())
}

/// `analysisLoader(pData, argc, argv, NotUsed)`: this callback is invoked once for each index
/// when reading the sqlite_stat1 table.
///
///     argv[0] = name of the table
///     argv[1] = name of the index (might be NULL)
///     argv[2] = results of analysis - on integer for each column
///
/// Entries for which argv[1]==NULL simply record the number of rows in the table.
fn analysis_loader(p: &mut Parse, i_db: usize, tbl: &str, idx: Option<&str>, stat: &str) {
    let zdb = p.db.a_db[i_db].z_db_s_name.clone();
    let tab = sqlite3_find_table(p, tbl, Some(&zdb));
    if tab.is_null() {
        return;
    }
    let ix = match idx {
        None => IndexId::NULL,
        Some(i) if tbl.eq_ignore_ascii_case(i) => sqlite3_primary_key_index(p, tab),
        Some(i) => sqlite3_find_index(p, i, Some(&zdb)),
    };
    if ix.nn() {
        let n_col = p.idx(ix).n_key_col as usize + 1;
        let mut a = std::mem::take(&mut p.idxm(ix).ai_row_log_est);
        if a.len() < n_col {
            a.resize(n_col, 0);
        }
        // pIndex->bUnordered = 0; pIndex->noSkipScan = 0 (decodeIntArray)
        let (un, ns, sz) = decode_int_array(stat, n_col, &mut a);
        let x = p.idxm(ix);
        x.ai_row_log_est = a;
        x.b_unordered = un;
        x.no_skip_scan = ns;
        if let Some(s) = sz {
            x.sz_idx_row = s;
        }
        x.has_stat1 = true;
        if x.p_part_idx_where.is_null() {
            let n = x.ai_row_log_est[0];
            let t = p.tabm(tab);
            t.n_row_log_est = n;
            t.tab_flags |= TF_HasStat1;
        }
    } else {
        // fakeIdx.szIdxRow = pTable->szTabRow; decodeIntArray(z, 1, 0, &pTable->nRowLogEst,
        // &fakeIdx); pTable->szTabRow = fakeIdx.szIdxRow
        let mut a = [p.tab(tab).n_row_log_est];
        let (_, _, sz) = decode_int_array(stat, 1, &mut a);
        let t = p.tabm(tab);
        t.n_row_log_est = a[0];
        if let Some(s) = sz {
            t.sz_tab_row = s;
        }
        t.tab_flags |= TF_HasStat1;
    }
}

/// `sqlite3AnalysisLoad(db, iDb)`: load the content of the sqlite_stat1 table. This routine
/// is used for reading the query planner statistics.
///
/// The schema objects are fresh here (C: on a schema load; the state C keeps across later
/// ANALYZE statements is tracked by the engine, which reports the affected tables through
/// `Host::table_unsupported`).
fn sqlite3_analysis_load(p: &mut Parse, i_db: usize) {
    // Load new statistics out of the sqlite_stat1 table
    let has_stat1 = p.a_schema[i_db].tbl_hash.get("sqlite_stat1").is_some_and(|&t| p.tab(t).is_ordinary());
    if has_stat1 {
        let host = p.db.host;
        for (tbl, idx, stat) in host.stat1_rows(i_db) {
            analysis_loader(p, i_db, &tbl, idx.as_deref(), &stat);
        }
    }
    // Set appropriate defaults on all indexes not in the sqlite_stat1 table
    let all: Vec<IndexId> = p.a_schema[i_db].idx_hash.values().copied().collect();
    for ix in all {
        if !p.idx(ix).has_stat1 {
            let tab = p.idx(ix).p_table;
            // (an object OP_ParseSchema added to a loaded schema was not there when this pass
            // last ran in C: it keeps what sqlite3CreateIndex() computed)
            let key = p.tab(tab).z_name.to_ascii_lowercase();
            if p.db.host.created_since_load(i_db, &key) {
                continue;
            }
            let mut n = p.tab(tab).n_row_log_est;
            sqlite3_default_row_est(p.idxm(ix), &mut n);
            p.tabm(tab).n_row_log_est = n;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// vdbemem.c: sqlite3ValueFromExpr
// ---------------------------------------------------------------------------------------------

fn type_affinity(aff: u8) -> sqliters_types::TypeAffinity {
    use sqliters_types::TypeAffinity as A;
    match aff {
        SQLITE_AFF_TEXT => A::Text,
        SQLITE_AFF_NUMERIC => A::Numeric,
        SQLITE_AFF_INTEGER => A::Integer,
        SQLITE_AFF_REAL => A::Real,
        _ => A::Blob,
    }
}

/// `sqlite3ValueFromExpr(db, pExpr, enc, affinity, ppVal)` / `valueFromExpr()`: create a new
/// sqlite3_value object, containing the value of pExpr.
///
/// This only works for very simple expressions that consist of one constant token (i.e.
/// "5", "5.1", "'a string'"). If the expression can be converted directly into a value, then
/// the value is returned with the affinity applied. `None` if no value can be made.
pub fn sqlite3_value_from_expr(p: &Parse, e: ExprId, affinity: u8) -> Option<Value> {
    if e.is_null() {
        return None;
    }
    let v = value_from_expr(p, e, affinity)?;
    if affinity == SQLITE_AFF_REAL
        && let Value::Integer(i) = v
        && !(-140737488355328..=140737488355327).contains(&i)
    {
        // If the integer value is too large to fit in a 6-byte integer and the affinity is
        // REAL, convert it to a real value now. In most cases an OP_RealAffinity opcode will
        // be used to convert the value to an actual real, but this opcode is omitted if the
        // values are being read directly from a table in order to create an index key.
        return Some(Value::real(i as f64));
    }
    Some(v)
}

/// `valueFromExpr()`: extract a value from the supplied expression in the manner described
/// above sqlite3ValueFromExpr().
fn value_from_expr(p: &Parse, mut e: ExprId, affinity: u8) -> Option<Value> {
    let mut op = p.e(e).op;
    while op == TK_UPLUS || op == TK_SPAN {
        e = p.e(e).p_left;
        op = p.e(e).op;
    }
    if op == TK_REGISTER {
        op = p.e(e).op2;
    }

    if op == TK_CAST {
        let z_type = p.e(e).tok().to_owned();
        let (aff, _) = sqlite3_affinity_type_sz(&z_type);
        let v = value_from_expr(p, p.e(e).p_left, aff)?;
        // sqlite3VdbeMemCast(*ppVal, aff, enc); sqlite3ValueApplyAffinity(*ppVal, affinity, enc)
        let v = sqliters_vdbe::eval_cast(v, &z_type);
        return Some(apply_affinity(v, affinity));
    }

    // Handle negative integers in a single step. This is needed in the case when the value
    // would otherwise overflow (-9223372036854775808).
    let mut neg_int: i64 = 1;
    let mut z_neg = "";
    if op == TK_UMINUS {
        let left = p.e(e).p_left;
        let lop = p.e(left).op;
        if lop == TK_INTEGER || lop == TK_FLOAT {
            let is_hex = !p.e(left).has(EP_IntValue) && {
                let t = p.e(left).tok().as_bytes();
                t.len() >= 2 && t[0] == b'0' && (t[1] & !0x20) == b'X'
            };
            if !is_hex {
                e = left;
                op = lop;
                neg_int = -1;
                z_neg = "-";
            }
        }
    }

    if op == TK_STRING || op == TK_FLOAT || op == TK_INTEGER {
        let mut v;
        if p.e(e).has(EP_IntValue) {
            v = Value::Integer(p.e(e).i_value as i64 * neg_int);
        } else {
            let tok = p.e(e).tok();
            let (i_val, rc) = if op == TK_INTEGER { crate::util::sqlite3_dec_or_hex_to_i64(tok) } else { (0, 1) };
            if op == TK_INTEGER && rc == 0 {
                v = Value::Integer(i_val.wrapping_mul(neg_int));
            } else {
                v = Value::Text(format!("{z_neg}{tok}"));
            }
        }
        if affinity == SQLITE_AFF_BLOB {
            if op == TK_FLOAT {
                // sqlite3AtoF(pVal->z, &pVal->u.r); pVal->flags = MEM_Real
                v = type_affinity(SQLITE_AFF_REAL).apply(v);
            } else if op == TK_INTEGER {
                // This case is required by -9223372036854775808 and other strings that look
                // like integers but have values too large for i64.
                v = apply_affinity(v, SQLITE_AFF_NUMERIC);
            }
        } else {
            v = apply_affinity(v, affinity);
        }
        return Some(v);
    } else if op == TK_UMINUS {
        // This branch happens for multiple negative signs. Ex: -(-5)
        let v = value_from_expr(p, p.e(e).p_left, affinity)?;
        // sqlite3VdbeMemNumerify(pVal)
        let v = match sqliters_vdbe::eval_cast(v, "NUMERIC") {
            Value::Real(r) => Value::real(-r),
            Value::Integer(i) if i == i64::MIN => Value::real(-(i64::MIN as f64)),
            Value::Integer(i) => Value::Integer(-i),
            other => other,
        };
        return Some(apply_affinity(v, affinity));
    } else if op == TK_NULL {
        return Some(Value::Null);
    } else if op == TK_BLOB {
        // "x'...'": the hex digits between the quotes
        let t = p.e(e).tok().as_bytes();
        if t.len() >= 3 {
            return Some(Value::Blob(crate::util::sqlite3_hex_to_blob(&t[2..t.len() - 1])));
        }
        return Some(Value::Blob(Vec::new()));
    } else if op == TK_TRUEFALSE {
        // pVal->u.i = pExpr->u.zToken[4]==0  ("true" has four letters);
        // sqlite3ValueApplyAffinity(pVal, affinity, enc)
        return Some(apply_affinity(Value::Integer((p.e(e).tok().len() == 4) as i64), affinity));
    }
    None
}

/// `sqlite3ValueApplyAffinity(pVal, affinity, enc)` / `applyAffinity()` of vdbe.c: processing
/// is determined by the affinity parameter:
///
/// SQLITE_AFF_INTEGER / SQLITE_AFF_REAL / SQLITE_AFF_NUMERIC: try to convert a text value into
/// an integer or floating point representation if an integer representation is not possible;
/// a floating point value that is an exact integer becomes an integer; an integer stays an
/// integer (also for REAL affinity: only `OP_Affinity` itself turns it into a real).
///
/// SQLITE_AFF_TEXT: convert a numeric value into a string representation.
///
/// SQLITE_AFF_BLOB / SQLITE_AFF_NONE: no-op.
fn apply_affinity(v: Value, affinity: u8) -> Value {
    use sqliters_types::TypeAffinity as A;
    if affinity >= SQLITE_AFF_NUMERIC {
        match v {
            Value::Integer(_) => v,
            // sqlite3VdbeIntegerAffinity(pRec) when affinity<=SQLITE_AFF_REAL
            Value::Real(_) if affinity <= SQLITE_AFF_REAL => A::Numeric.apply(v),
            // applyNumericAffinity(pRec, 1)
            Value::Text(_) => A::Numeric.apply(v),
            other => other,
        }
    } else if affinity == SQLITE_AFF_TEXT {
        match v {
            Value::Integer(_) | Value::Real(_) => A::Text.apply(v),
            other => other,
        }
    } else {
        v
    }
}

// ---------------------------------------------------------------------------------------------
// Verification aid: the schema-object dump
// ---------------------------------------------------------------------------------------------

/// Every Table / Column / Index / FKey field the generator reads, one line per object, in the
/// format of the instrumented reference library (`v3DumpSchema()` of the scratch build): the
/// schema layer of the differential proof. The lines are not ordered.
pub fn schema_dump(p: &mut Parse) -> Vec<String> {
    sqlite3_init(p);
    let mut out = Vec::new();
    for i in 0..p.a_schema.len() {
        let tabs: Vec<TableId> = p.a_schema[i].tbl_hash.values().copied().collect();
        for tab in tabs {
            let t = p.tab(tab);
            if !t.is_ordinary() {
                out.push(format!("T {} {} eTabType={} flags={:x}", i, t.z_name, t.e_tab_type, t.tab_flags & 0x200));
                continue;
            }
            let n_check = if t.p_check.nn() { p.l(t.p_check).a.len() } else { 0 };
            out.push(format!(
                "T {} {} tnum={} nCol={} nNVCol={} iPKey={} keyConf={} flags={:x} nRowLogEst={} szTabRow={} eTabType={} nIdx={} nCheck={} nFKey={}",
                i,
                t.z_name,
                t.tnum,
                t.a_col.len(),
                t.n_nv_col,
                t.i_p_key,
                t.key_conf,
                t.tab_flags & !(TF_Shadow | TF_Ephemeral | TF_Eponymous | TF_MaybeReanalyze | TF_HasStat4 | TF_OOOHidden),
                t.n_row_log_est,
                t.sz_tab_row,
                t.e_tab_type,
                t.p_index.len(),
                n_check,
                t.p_fkey.len()
            ));
            for j in 0..p.tab(tab).a_col.len() {
                let c = &p.tab(tab).a_col[j];
                let mask = (COLFLAG_PRIMKEY | COLFLAG_HIDDEN | COLFLAG_HASTYPE | COLFLAG_UNIQUE | COLFLAG_VIRTUAL | COLFLAG_STORED | COLFLAG_HASCOLL) as u16;
                let mut line = format!(
                    "C {} {} {} name={} type={} aff={} notNull={} flags={:x} szEst={} eCType={} coll={} iDflt={} dflt=",
                    i,
                    p.tab(tab).z_name,
                    j,
                    c.z_cn_name,
                    sqlite3_column_type(c, Some("-")).unwrap_or("-"),
                    c.affinity,
                    c.not_null,
                    c.col_flags & mask,
                    c.sz_est,
                    c.e_c_type,
                    sqlite3_column_coll(c).unwrap_or("-"),
                    c.i_dflt
                );
                let v = if c.i_dflt != 0 { sqlite3_value_from_expr(p, sqlite3_column_expr(p, tab, j), c.affinity) } else { None };
                match v {
                    None => line.push('-'),
                    Some(Value::Null) => line.push_str("5:"),
                    Some(Value::Integer(x)) => line.push_str(&format!("1:{x}")),
                    Some(v @ Value::Real(_)) => line.push_str(&format!("2:{}", sqliters_types::TypeAffinity::Text.apply(v).to_text_lossy())),
                    Some(Value::Text(x)) => line.push_str(&format!("3:{x}")),
                    Some(Value::Blob(b)) => {
                        line.push_str("4:");
                        for x in b {
                            line.push_str(&format!("{x:02x}"));
                        }
                    }
                }
                out.push(line);
            }
            for (n, &ix) in p.tab(tab).p_index.iter().enumerate() {
                let x = p.idx(ix);
                let fl = (x.b_unordered as u32)
                    | (x.uniq_not_null as u32) << 1
                    | (x.is_resized as u32) << 2
                    | (x.is_covering as u32) << 3
                    | (x.no_skip_scan as u32) << 4
                    | (x.has_stat1 as u32) << 5
                    | (x.b_no_query as u32) << 6
                    | (x.b_asc_key_bug as u32) << 7
                    | (x.b_has_v_col as u32) << 8
                    | (x.b_has_expr as u32) << 9;
                let nc = x.n_column as usize;
                let join = |v: Vec<String>| v.join(",");
                out.push(format!(
                    "I {} {} {} name={} tnum={} nKeyCol={} nColumn={} onError={} idxType={} fl={:x} szIdxRow={} colNotIdxed={:x} cols=[{}] sort=[{}] coll=[{}] est=[{}] part={} nExpr={}",
                    i,
                    p.tab(tab).z_name,
                    n,
                    x.z_name,
                    x.tnum,
                    x.n_key_col,
                    x.n_column,
                    x.on_error,
                    x.idx_type,
                    fl,
                    x.sz_idx_row,
                    x.col_not_idxed,
                    join(x.ai_column[..nc].iter().map(|v| v.to_string()).collect()),
                    join(x.a_sort_order[..nc].iter().map(|v| v.to_string()).collect()),
                    join((0..nc).map(|k| if x.a_coll_static[k] { "~".to_owned() } else { x.az_coll[k].clone() }).collect()),
                    join(x.ai_row_log_est[..=x.n_key_col as usize].iter().map(|v| v.to_string()).collect()),
                    x.p_part_idx_where.nn() as i32,
                    if x.a_col_expr.nn() { p.l(x.a_col_expr).a.len() } else { 0 }
                ));
            }
            for (n, fk) in p.tab(tab).p_fkey.iter().enumerate() {
                let cols: Vec<String> = fk.a_col.iter().map(|(f, t)| format!("{}:{}", f, t.as_deref().unwrap_or("-"))).collect();
                out.push(format!(
                    "F {} {} {} to={} def={} act={},{} cols=[{}]",
                    i,
                    p.tab(tab).z_name,
                    n,
                    fk.z_to,
                    fk.is_deferred as i32,
                    fk.a_action[0],
                    fk.a_action[1],
                    cols.join(",")
                ));
            }
        }
    }
    for e in &p.init_errors {
        out.push(format!("LOADERR {e}"));
    }
    out
}
