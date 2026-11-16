//! Port of the code-generating part of `alter.c` (step V6): ALTER TABLE RENAME TO, ADD
//! COLUMN, RENAME COLUMN, DROP COLUMN, DROP CONSTRAINT, ALTER COLUMN SET / DROP NOT NULL and
//! ADD CHECK. The schema text is rewritten at run time by the SQL functions
//! (`sqlite_rename_table()`, …) that the nested statements below call; only the statements
//! themselves are coded here.

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::P4;

use crate::build::*;
use crate::consts::*;
use crate::ddl::{LEGACY_SCHEMA_TABLE, pf_big_q, pf_q, pf_w, sqlite3_nested_parse};
use crate::sqliteint::*;
use crate::tk::*;

/// `INITFLAG_Alter*`: the P5 of OP_ParseSchema after an ALTER TABLE
const INITFLAG_ALTER_RENAME: u16 = 0x0001;
const INITFLAG_ALTER_DROP: u16 = 0x0002;
const INITFLAG_ALTER_ADD: u16 = 0x0003;
const INITFLAG_ALTER_DROP_CONS: u16 = 0x0004;

/// `isAlterableTable(pParse, pTab)`: parameter zName is the name of a table that is about to
/// be altered (either with ALTER TABLE ... RENAME TO or ALTER TABLE ... ADD COLUMN). If the
/// table is a system table, this function leaves an error message in pParse->zErr (system
/// tables may not be altered) and returns non-zero.
fn is_alterable_table(p: &mut Parse, tab: TableId) -> bool {
    let t = p.tab(tab);
    let z = t.z_name.as_bytes();
    if (z.len() >= 7 && z[..7].eq_ignore_ascii_case(b"sqlite_"))
        || (t.tab_flags & TF_Eponymous) != 0
        || ((t.tab_flags & TF_Shadow) != 0 && crate::ddl::sqlite3_read_only_shadow_tables(p))
    {
        let n = p.tab(tab).z_name.clone();
        p.error_msg(format!("table {n} may not be altered"));
        return true;
    }
    false
}

/// `renameTestSchema(pParse, zDb, bTemp, zWhen, bNoDQS)`: generate code to verify that the
/// schemas of database zDb and, if bTemp is not true, database "temp", can still be parsed.
/// This is called at the end of the generation of code for an ALTER TABLE command, to ensure
/// that the schemas are valid.
fn rename_test_schema(p: &mut Parse, z_db: &str, b_temp: bool, z_when: &str, b_no_dqs: i32) {
    p.col_names_set = true;
    sqlite3_nested_parse(
        p,
        format!(
            "SELECT 1 FROM \"{}\".{LEGACY_SCHEMA_TABLE} WHERE name NOT LIKE 'sqliteX_%' ESCAPE 'X' AND sql NOT LIKE 'create virtual%' AND sqlite_rename_test({}, sql, type, name, {}, {}, {})=NULL ",
            pf_w(z_db),
            pf_big_q(Some(z_db)),
            b_temp as i32,
            pf_big_q(Some(z_when)),
            b_no_dqs
        ),
    );
    if !b_temp {
        sqlite3_nested_parse(
            p,
            format!(
                "SELECT 1 FROM temp.{LEGACY_SCHEMA_TABLE} WHERE name NOT LIKE 'sqliteX_%' ESCAPE 'X' AND sql NOT LIKE 'create virtual%' AND sqlite_rename_test({}, sql, type, name, 1, {}, {})=NULL ",
                pf_big_q(Some(z_db)),
                pf_big_q(Some(z_when)),
                b_no_dqs
            ),
        );
    }
}

/// `renameFixQuotes(pParse, zDb, bTemp)`: generate VM code to replace any double-quoted
/// strings (other than identifiers) in the schema of database zDb (and of "temp" unless
/// bTemp) with single-quoted strings.
fn rename_fix_quotes(p: &mut Parse, z_db: &str, b_temp: bool) {
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE \"{}\".{LEGACY_SCHEMA_TABLE} SET sql = sqlite_rename_quotefix({}, sql)WHERE name NOT LIKE 'sqliteX_%' ESCAPE 'X' AND sql NOT LIKE 'create virtual%'",
            pf_w(z_db),
            pf_big_q(Some(z_db))
        ),
    );
    if !b_temp {
        sqlite3_nested_parse(
            p,
            format!(
                "UPDATE temp.{LEGACY_SCHEMA_TABLE} SET sql = sqlite_rename_quotefix('temp', sql)WHERE name NOT LIKE 'sqliteX_%' ESCAPE 'X' AND sql NOT LIKE 'create virtual%'"
            ),
        );
    }
}

/// `renameReloadSchema(pParse, iDb, p5)`: generate code to reload the schema for database iDb.
/// And, if iDb!=1, for the temp database as well.
fn rename_reload_schema(p: &mut Parse, i_db: i32, p5: u16) {
    if p.has_vdbe {
        crate::ddl::sqlite3_change_cookie(p, i_db);
        crate::ddl::sqlite3_vdbe_add_parse_schema_op(p, i_db, None, p5);
        if i_db != 1 {
            crate::ddl::sqlite3_vdbe_add_parse_schema_op(p, 1, None, p5);
        }
    }
}

/// `sqlite3Utf8CharLen(z, -1)`: the number of characters of a UTF-8 string.
fn utf8_char_len(z: &str) -> usize {
    // (C counts lead bytes; for valid UTF-8 that is the number of chars)
    z.as_bytes().iter().filter(|&&b| (b & 0xc0) != 0x80).count()
}

/// `sqlite3AlterRenameTable(pParse, pSrc, pName)`: generate code to implement the "ALTER
/// TABLE xxx RENAME TO yyy" command.
pub fn sqlite3_alter_rename_table(p: &mut Parse, src: SrcListId, name: &Token) {
    let tab = crate::schema::sqlite3_locate_table_item(p, 0, src, 0);
    if tab.is_null() {
        return;
    }
    let i_db = p.tab(tab).i_db;
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();

    // Get a NULL terminated version of the new table name.
    let Some(z_name) = sqlite3_name_from_token(p, Some(name)) else {
        return;
    };

    // Check that a table or index named 'zName' does not already exist in database iDb. If
    // so, this is an error.
    if crate::schema::sqlite3_find_table(p, &z_name, Some(&z_db)).nn()
        || crate::build_tab::sqlite3_find_index(p, &z_name, Some(&z_db)).nn()
        || crate::vtab::sqlite3_is_shadow_table_of(p, tab, &z_name)
    {
        p.error_msg(format!(
            "there is already another table or index with this name: {z_name}"
        ));
        return;
    }

    // Make sure it is not a system table being altered, or a reserved name that the table is
    // being renamed to.
    if is_alterable_table(p, tab) {
        return;
    }
    if crate::build_tab::sqlite3_check_object_name(p, &z_name, "table", &z_name) {
        return;
    }
    if p.tab(tab).is_view() {
        let n = p.tab(tab).z_name.clone();
        p.error_msg(format!("view {n} may not be altered"));
        return;
    }
    // (sqlite3AuthCheck: the generator is not used with an authorizer)

    if crate::select::sqlite3_view_get_column_names(p, tab) != 0 {
        return;
    }
    let mut vtab: Option<(u64, String)> = None;
    if p.tab(tab).is_virtual() {
        match crate::vtab::sqlite3_get_vtable(p, tab) {
            Some(v) if v.has_rename == Some(true) => vtab = Some((v.id, p.tab(tab).z_name.clone())),
            Some(v) if v.has_rename == Some(false) => {}
            _ => {
                p.not_supported("ALTER TABLE RENAME of a virtual table without a connection");
                return;
            }
        }
    }

    // Begin a transaction for database iDb. Then modify the schema cookie (since the ALTER
    // TABLE modifies the schema).
    crate::select::sqlite3_get_vdbe(p);
    sqlite3_may_abort(p);

    // figure out how many UTF-8 characters are in zName
    let z_tab_name = p.tab(tab).z_name.clone();
    let n_tab_name = utf8_char_len(&z_tab_name);

    // Rewrite all CREATE TABLE, INDEX, TRIGGER or VIEW statements in the schema to use the
    // new table name.
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE \"{}\".{LEGACY_SCHEMA_TABLE} SET sql = sqlite_rename_table({}, type, name, sql, {}, {}, {}) WHERE (type!='index' OR tbl_name={} COLLATE nocase)AND   name NOT LIKE 'sqliteX_%' ESCAPE 'X'",
            pf_w(&z_db),
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_tab_name)),
            pf_big_q(Some(&z_name)),
            (i_db == 1) as i32,
            pf_big_q(Some(&z_tab_name)),
        ),
    );

    // Update the tbl_name and name columns of the sqlite_schema table as required.
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE {}.{LEGACY_SCHEMA_TABLE} SET tbl_name = {}, name = CASE WHEN type='table' THEN {} WHEN name LIKE 'sqliteX_autoindex%' ESCAPE 'X'      AND type='index' THEN 'sqlite_autoindex_' || {} || substr(name,{}+18) ELSE name END WHERE tbl_name={} COLLATE nocase AND (type='table' OR type='index' OR type='trigger');",
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_name)),
            pf_big_q(Some(&z_name)),
            pf_big_q(Some(&z_name)),
            n_tab_name,
            pf_big_q(Some(&z_tab_name)),
        ),
    );

    // If the sqlite_sequence table exists in this database, then update it with the new
    // table name.
    if crate::schema::sqlite3_find_table(p, "sqlite_sequence", Some(&z_db)).nn() {
        sqlite3_nested_parse(
            p,
            format!(
                "UPDATE \"{}\".sqlite_sequence set name = {} WHERE name = {}",
                pf_w(&z_db),
                pf_big_q(Some(&z_name)),
                pf_big_q(Some(&z_tab_name))
            ),
        );
    }

    // If the table being renamed is not itself part of the temp database, edit view and
    // trigger definitions within the temp database as required.
    if i_db != 1 {
        sqlite3_nested_parse(
            p,
            format!(
                "UPDATE sqlite_temp_schema SET sql = sqlite_rename_table({}, type, name, sql, {}, {}, 1), tbl_name = CASE WHEN tbl_name={} COLLATE nocase AND   sqlite_rename_test({}, sql, type, name, 1, 'after rename', 0) THEN {} ELSE tbl_name END WHERE type IN ('view', 'trigger')",
                pf_big_q(Some(&z_db)),
                pf_big_q(Some(&z_tab_name)),
                pf_big_q(Some(&z_name)),
                pf_big_q(Some(&z_tab_name)),
                pf_big_q(Some(&z_db)),
                pf_big_q(Some(&z_name)),
            ),
        );
    }

    // If this is a virtual table, invoke the xRename() function if one is defined. The
    // xRename() callback will modify the names of any resources (tables, etc.) used by the
    // virtual table implementation.
    if let Some((id, vname)) = vtab {
        p.n_mem += 1;
        let i = p.n_mem;
        p.v.load_string(i, &z_name);
        p.v.add_op4(
            OP_VRename,
            i,
            0,
            0,
            P4::Vtab(std::sync::Arc::new(sqliters_vdbe::vdbeaux::P4Vtab {
                id,
                name: vname,
            })),
        );
    }

    rename_reload_schema(p, i_db, INITFLAG_ALTER_RENAME);
    rename_test_schema(p, &z_db, i_db == 1, "after rename", 0);
}

/// `sqlite3ErrorIfNotEmpty(pParse, zDb, zTab, zErr)`: write code that will raise an error if
/// the table described by zDb and zTab is not empty.
fn sqlite3_error_if_not_empty(p: &mut Parse, z_db: &str, z_tab: &str, z_err: &str) {
    sqlite3_nested_parse(
        p,
        format!(
            "SELECT raise(ABORT,{}) FROM \"{}\".\"{}\"",
            pf_big_q(Some(z_err)),
            pf_w(z_db),
            pf_w(z_tab)
        ),
    );
}

/// `isRowidAlias(pParse, pTab, zCol, zOp)`: if the column zCol of pTab is one of the names of
/// the rowid (and the table has one), leave an error message and return true.
fn is_rowid_alias(p: &mut Parse, tab: TableId, z_col: &str, z_op: &str) -> bool {
    if p.tab(tab).has_rowid() && crate::expr::sqlite3_is_rowid(z_col) {
        p.error_msg(format!("cannot {z_op} rowid alias: {z_col}"));
        return true;
    }
    false
}

/// `sqlite3AlterFinishAddColumn(pParse, pColDef)`: this function is called after an "ALTER
/// TABLE ... ADD" statement has been parsed. Argument pColDef contains the text of the new
/// column definition.
///
/// The Table structure pParse->pNewTable was extended to include the new column during
/// parsing.
pub fn sqlite3_alter_finish_add_column(p: &mut Parse, col_def: &Token) {
    if p.n_err != 0 || p.unsupported.is_some() {
        return;
    }
    let new = p.p_new_table;
    if new.is_null() {
        return;
    }
    let i_db = p.tab(new).i_db;
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();
    let z_tab = p.tab(new).z_name[16..].to_owned(); // Skip the "sqlite_altertab_" prefix
    let n_col = p.tab(new).a_col.len();
    let col = p.tab(new).a_col[n_col - 1].clone();
    let mut dflt = crate::build_tab::sqlite3_column_expr(p, new, n_col - 1);
    let tab = crate::schema::sqlite3_find_table(p, &z_tab, Some(&z_db));
    // (sqlite3AuthCheck: the generator is not used with an authorizer)

    // Check that the new column is not specified as PRIMARY KEY or UNIQUE. If there is a
    // NOT NULL constraint, then the default value for the new column must not be NULL.
    if (col.col_flags & COLFLAG_PRIMKEY as u16) != 0 {
        p.error_msg("Cannot add a PRIMARY KEY column");
        return;
    }
    if !p.tab(new).p_index.is_empty() {
        p.error_msg("Cannot add a UNIQUE column");
        return;
    }
    if is_rowid_alias(p, tab, &col.z_cn_name, "add") {
        return;
    }
    if (col.col_flags & COLFLAG_GENERATED as u16) == 0 {
        // If the default value for the new column was specified with a literal NULL, then
        // set pDflt to 0. This simplifies checking for an SQL NULL default below.
        if dflt.nn() && p.e(p.e(dflt).p_left).op == TK_NULL {
            dflt = ExprId::NULL;
        }
        if (p.db.flags & SQLITE_ForeignKeys) != 0 && !p.tab(new).p_fkey.is_empty() && dflt.nn() {
            sqlite3_error_if_not_empty(
                p,
                &z_db,
                &z_tab,
                "Cannot add a REFERENCES column with non-NULL default value",
            );
        }
        if col.not_null != 0 && dflt.is_null() {
            sqlite3_error_if_not_empty(
                p,
                &z_db,
                &z_tab,
                "Cannot add a NOT NULL column with default value NULL",
            );
        }

        // Ensure the default expression is something that sqlite3ValueFromExpr() can handle
        // (i.e. not CURRENT_TIME etc.)
        if dflt.nn()
            && crate::build_tab::sqlite3_value_from_expr(p, dflt, SQLITE_AFF_BLOB).is_none()
        {
            sqlite3_error_if_not_empty(
                p,
                &z_db,
                &z_tab,
                "Cannot add a column with non-constant default",
            );
        }
    } else if (col.col_flags & COLFLAG_STORED as u16) != 0 {
        sqlite3_error_if_not_empty(p, &z_db, &z_tab, "cannot add a STORED column");
    }

    // Modify the CREATE TABLE statement.
    let b = p.z_sql.as_bytes();
    let (a, n) = (col_def.z as usize, col_def.n as usize);
    let mut z_col: &[u8] = &b[a..(a + n).min(b.len())];
    while z_col.len() > 1
        && (z_col[z_col.len() - 1] == b';' || crate::util::is_space(z_col[z_col.len() - 1]))
    {
        z_col = &z_col[..z_col.len() - 1];
    }
    let z_col = String::from_utf8_lossy(z_col).into_owned();
    // substr() operations on characters, but addColOffset is in bytes. So we have to use
    // printf() to do the substr() operations.
    let off = p.tab(new).add_col_offset;
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE \"{}\".{LEGACY_SCHEMA_TABLE} SET sql = printf('%.{off}s, ',sql) || {} || substr(sql,1+length(printf('%.{off}s',sql))) WHERE type = 'table' AND name = {}",
            pf_w(&z_db),
            pf_big_q(Some(&z_col)),
            pf_big_q(Some(&z_tab)),
        ),
    );

    // Make sure the schema version is at least 3. But do not upgrade from less than 3 to 4,
    // as that will corrupt any preexisting DESC index entries.
    if p.has_vdbe {
        let r1 = crate::expr::sqlite3_get_temp_reg(p);
        p.v.add_op3(OP_ReadCookie, i_db, r1, crate::ddl::BTREE_FILE_FORMAT);
        let sharable = p.db.a_db[i_db as usize].sharable;
        p.v.uses_btree(i_db, sharable);
        p.v.add_op2(OP_AddImm, r1, -2);
        let a = p.v.current_addr() + 2;
        p.v.add_op2(OP_IfPos, r1, a);
        p.v.add_op3(OP_SetCookie, i_db, crate::ddl::BTREE_FILE_FORMAT, 3);
        crate::expr::sqlite3_release_temp_reg(p, r1);

        // Reload the table definition
        rename_reload_schema(p, i_db, INITFLAG_ALTER_ADD);

        // Verify that constraints are still satisfied
        if p.tab(new).p_check.nn()
            || (col.not_null != 0 && (col.col_flags & COLFLAG_GENERATED as u16) != 0)
            || (p.tab(tab).tab_flags & TF_Strict) != 0
        {
            p.col_names_set = true;
            sqlite3_nested_parse(
                p,
                format!(
                    "SELECT CASE WHEN quick_check GLOB 'CHECK*' THEN raise(ABORT,'CHECK constraint failed') WHEN quick_check GLOB 'non-* value in*' THEN raise(ABORT,'type mismatch on DEFAULT') ELSE raise(ABORT,'NOT NULL constraint failed') END  FROM pragma_quick_check({},{}) WHERE quick_check GLOB 'CHECK*' OR quick_check GLOB 'NULL*' OR quick_check GLOB 'non-* value in*'",
                    pf_big_q(Some(&z_tab)),
                    pf_big_q(Some(&z_db)),
                ),
            );
        }
    }
}

/// `sqlite3AlterBeginAddColumn(pParse, pSrc)`: this function is called by the parser after
/// the table-name in an "ALTER TABLE <table-name> ADD" statement is parsed. Argument pSrc is
/// the full-name of the table being altered.
///
/// This routine makes a (partial) copy of the Table structure for the table being altered
/// and sets Parse.pNewTable to point to it. Routines called by the parser as the column
/// definition is parsed (i.e. sqlite3AddColumn()) add the new Column data to the copy. The
/// copy of the Table structure is deleted by tokenize.c after parsing is finished.
///
/// Routine sqlite3AlterFinishAddColumn() will be called to complete coding the "ALTER TABLE
/// ... ADD" statement.
pub fn sqlite3_alter_begin_add_column(p: &mut Parse, src: SrcListId) {
    let tab = crate::schema::sqlite3_locate_table_item(p, 0, src, 0);
    if tab.is_null() {
        return;
    }
    if p.tab(tab).is_virtual() {
        p.error_msg("virtual tables may not be altered");
        return;
    }

    // Make sure this is not an attempt to ALTER a view.
    if p.tab(tab).is_view() {
        p.error_msg("Cannot add a column to a view");
        return;
    }
    if is_alterable_table(p, tab) {
        return;
    }

    sqlite3_may_abort(p);
    let i_db = p.tab(tab).i_db;

    // Put a copy of the Table struct in Parse.pNewTable for the sqlite3AddColumn() function
    // and others that modify the new column. The copy carries the columns, the default list
    // and addColOffset (C copies nothing else).
    let t = p.tab(tab);
    let new = Table {
        z_name: format!("sqlite_altertab_{}", t.z_name),
        a_col: t.a_col.clone(),
        n_tab_ref: 1,
        i_db,
        add_col_offset: t.add_col_offset,
        ..Table::default()
    };
    let dflt = t.p_dflt_list;
    let new = p.new_tab(new);
    let dup = crate::expr::sqlite3_expr_list_dup(p, dflt, 0);
    p.tabm(new).p_dflt_list = dup;
    p.p_new_table = new;
}

/// `isRealTable(pParse, pTab, iOp)`: if the table is a view or virtual table, leave an error
/// message ("cannot rename columns of / drop column from / edit constraints of ...") and
/// return true.
fn is_real_table(p: &mut Parse, tab: TableId, i_op: usize) -> bool {
    let z_type = if p.tab(tab).is_view() {
        Some("view")
    } else if p.tab(tab).is_virtual() {
        Some("virtual table")
    } else {
        None
    };
    if let Some(z_type) = z_type {
        const AZ_MSG: [&str; 3] = [
            "rename columns of",
            "drop column from",
            "edit constraints of",
        ];
        let n = p.tab(tab).z_name.clone();
        p.error_msg(format!("cannot {} {z_type} \"{n}\"", AZ_MSG[i_op]));
        return true;
    }
    false
}

/// `sqlite3AlterRenameColumn(pParse, pSrc, pOld, pNew)`: handles the following parser
/// reduction:
///
///  cmd ::= ALTER TABLE pSrc RENAME COLUMN pOld TO pNew
pub fn sqlite3_alter_rename_column(p: &mut Parse, src: SrcListId, old: &Token, new: &Token) {
    // Locate the table to be altered
    let tab = crate::schema::sqlite3_locate_table_item(p, 0, src, 0);
    if tab.is_null() {
        return;
    }

    // Cannot alter a system table
    if is_alterable_table(p, tab) {
        return;
    }
    if is_real_table(p, tab, 0) {
        return;
    }

    // Which schema holds the table to be altered
    let i_schema = p.tab(tab).i_db;
    let z_db = p.db.a_db[i_schema as usize].z_db_s_name.clone();
    // (sqlite3AuthCheck: the generator is not used with an authorizer)

    // Make sure the old name really is a column name in the table to be altered. Set iCol to
    // be the index of the column being renamed
    let Some(z_old) = sqlite3_name_from_token(p, Some(old)) else {
        return;
    };
    let i_col = crate::resolve::sqlite3_column_index(p, tab, &z_old);
    if i_col < 0 {
        let msg = format!("no such column: \"{}\"", p.tok_str(old));
        p.error_msg_tok(msg, old);
        return;
    }

    // Ensure the schema contains no double-quoted strings
    rename_test_schema(p, &z_db, i_schema == 1, "", 0);
    rename_fix_quotes(p, &z_db, i_schema == 1);

    // Do the rename operation using a recursive UPDATE statement that uses the
    // sqlite_rename_column() SQL function to compute the new CREATE statement text for the
    // sqlite_schema table.
    sqlite3_may_abort(p);
    let Some(z_new) = sqlite3_name_from_token(p, Some(new)) else {
        return;
    };
    if is_rowid_alias(p, tab, &z_old, "rename from") {
        return;
    }
    if is_rowid_alias(p, tab, &z_new, "rename to") {
        return;
    }
    let first = p.z_sql.as_bytes().get(new.z as usize).copied().unwrap_or(0);
    // bQuote = sqlite3Isquote(pNew->z[0]): the 0x80 bit of sqlite3CtypeMap[], not 0/1
    let b_quote = if crate::util::is_quote(first) {
        0x80
    } else {
        0
    };
    let z_tab = p.tab(tab).z_name.clone();
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE \"{}\".{LEGACY_SCHEMA_TABLE} SET sql = sqlite_rename_column(sql, type, name, {}, {}, {i_col}, {}, {b_quote}, {}) WHERE name NOT LIKE 'sqliteX_%' ESCAPE 'X'  AND (type != 'index' OR tbl_name = {})",
            pf_w(&z_db),
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_tab)),
            pf_big_q(Some(&z_new)),
            (i_schema == 1) as i32,
            pf_big_q(Some(&z_tab)),
        ),
    );

    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE temp.{LEGACY_SCHEMA_TABLE} SET sql = sqlite_rename_column(sql, type, name, {}, {}, {i_col}, {}, {b_quote}, 1) WHERE type IN ('trigger', 'view')",
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_tab)),
            pf_big_q(Some(&z_new)),
        ),
    );

    // Drop and reload the database schema.
    rename_reload_schema(p, i_schema, INITFLAG_ALTER_RENAME);
    rename_test_schema(p, &z_db, i_schema == 1, "after rename", 1);
}

/// `sqlite3AlterDropColumn(pParse, pSrc, pName)`: this function is called by the parser
/// upon parsing an
///
///     ALTER TABLE pSrc DROP COLUMN pName
///
/// statement. Argument pSrc contains the possibly qualified name of the table being altered.
/// pName contains the name of the column to drop.
pub fn sqlite3_alter_drop_column(p: &mut Parse, src: SrcListId, name: &Token) {
    // Locate the table to be altered
    let tab = crate::schema::sqlite3_locate_table_item(p, 0, src, 0);
    if tab.is_null() {
        return;
    }

    // Make sure this is not an attempt to ALTER a view, virtual table or system table.
    if is_alterable_table(p, tab) {
        return;
    }
    if is_real_table(p, tab, 1) {
        return;
    }

    // Find the index of the column being dropped.
    let Some(z_col) = sqlite3_name_from_token(p, Some(name)) else {
        return;
    };
    let i_col = crate::resolve::sqlite3_column_index(p, tab, &z_col);
    if i_col < 0 {
        let msg = format!("no such column: \"{}\"", p.tok_str(name));
        p.error_msg_tok(msg, name);
        return;
    }
    if is_rowid_alias(p, tab, &z_col, "drop") {
        return;
    }

    // Do not allow the user to drop a PRIMARY KEY column or a column constrained by a UNIQUE
    // constraint.
    let cf = p.tab(tab).a_col[i_col as usize].col_flags;
    if (cf & (COLFLAG_PRIMKEY | COLFLAG_UNIQUE) as u16) != 0 {
        let what = if (cf & COLFLAG_PRIMKEY as u16) != 0 {
            "PRIMARY KEY"
        } else {
            "UNIQUE"
        };
        p.error_msg(format!("cannot drop {what} column: \"{z_col}\""));
        return;
    }

    // Do not allow the number of columns to go to zero
    if p.tab(tab).a_col.len() <= 1 {
        p.error_msg(format!(
            "cannot drop column \"{z_col}\": no other columns exist"
        ));
        return;
    }

    // Edit the sqlite_schema table
    let i_db = p.tab(tab).i_db;
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();
    // (sqlite3AuthCheck: the generator is not used with an authorizer)
    rename_test_schema(p, &z_db, i_db == 1, "", 0);
    rename_fix_quotes(p, &z_db, i_db == 1);
    let z_tab = p.tab(tab).z_name.clone();
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE \"{}\".{LEGACY_SCHEMA_TABLE} SET sql = sqlite_drop_column({i_db}, sql, {i_col}) WHERE (type=='table' AND tbl_name={} COLLATE nocase)",
            pf_w(&z_db),
            pf_big_q(Some(&z_tab)),
        ),
    );

    // Drop and reload the database schema.
    rename_reload_schema(p, i_db, INITFLAG_ALTER_DROP);
    rename_test_schema(p, &z_db, i_db == 1, "after drop column", 1);

    // Edit rows of table on disk
    if p.n_err == 0 && (cf & COLFLAG_VIRTUAL as u16) == 0 {
        crate::select::sqlite3_get_vdbe(p);
        let i_cur = p.n_tab;
        p.n_tab += 1;
        crate::schema::sqlite3_open_table(p, i_cur, i_db, tab, OP_OpenWrite);
        let addr = p.v.add_op1(OP_Rewind, i_cur);
        p.n_mem += 1;
        let reg = p.n_mem;
        let mut pk = IndexId::NULL;
        let mut n_field: i32 = 0;
        if p.tab(tab).has_rowid() {
            p.v.add_op2(OP_Rowid, i_cur, reg);
            p.n_mem += p.tab(tab).a_col.len() as i32;
        } else {
            pk = crate::schema::sqlite3_primary_key_index(p, tab);
            p.n_mem += p.idx(pk).n_column as i32;
            for i in 0..p.idx(pk).n_key_col as i32 {
                p.v.add_op3(OP_Column, i_cur, i, reg + i + 1);
            }
            n_field = p.idx(pk).n_key_col as i32;
        }
        p.n_mem += 1;
        let reg_rec = p.n_mem;
        for i in 0..p.tab(tab).a_col.len() as i32 {
            if i != i_col && (p.tab(tab).a_col[i as usize].col_flags & COLFLAG_VIRTUAL as u16) == 0
            {
                let reg_out;
                if pk.nn() {
                    let i_pos = crate::schema::sqlite3_table_column_to_index(p.idx(pk), i);
                    let i_col_pos = crate::schema::sqlite3_table_column_to_index(p.idx(pk), i_col);
                    if i_pos < p.idx(pk).n_key_col as i32 {
                        continue;
                    }
                    reg_out = reg + 1 + i_pos - (i_pos > i_col_pos) as i32;
                } else {
                    reg_out = reg + 1 + n_field;
                }
                if i == p.tab(tab).i_p_key as i32 {
                    p.v.add_op2(OP_Null, 0, reg_out);
                } else {
                    let aff = p.tab(tab).a_col[i as usize].affinity;
                    if aff == SQLITE_AFF_REAL {
                        p.tabm(tab).a_col[i as usize].affinity = SQLITE_AFF_NUMERIC;
                    }
                    crate::exprcode::sqlite3_expr_code_get_column_of_table(
                        p, tab, i_cur, i, reg_out,
                    );
                    p.tabm(tab).a_col[i as usize].affinity = aff;
                }
                n_field += 1;
            }
        }
        if n_field == 0 {
            // dropping the only non-primary-key column of a WITHOUT ROWID table
            p.n_mem += 1;
            p.v.add_op2(OP_Null, 0, reg + 1);
            n_field = 1;
        }
        p.v.add_op3(OP_MakeRecord, reg + 1, n_field, reg_rec);
        if pk.nn() {
            let nk = p.idx(pk).n_key_col as i32;
            p.v.add_op4_int(OP_IdxInsert, i_cur, reg_rec, reg + 1, nk);
        } else {
            p.v.add_op3(OP_Insert, i_cur, reg_rec, reg);
        }
        p.v.change_p5(OPFLAG_SAVEPOSITION as u16);

        p.v.add_op2(OP_Next, i_cur, addr + 1);
        p.v.jump_here(addr);
    }
}

/// `alterFindCol(pParse, pTab, pCol, piCol)`: find the column named by token pCol in table
/// pTab. Returns the column index, or None after leaving an error message.
fn alter_find_col(p: &mut Parse, tab: TableId, col: &Token) -> Option<i32> {
    let z_name = sqlite3_name_from_token(p, Some(col))?;
    let i_col = crate::resolve::sqlite3_column_index(p, tab, &z_name);
    if i_col < 0 {
        p.error_msg(format!("no such column: {z_name}"));
        return None;
    }
    // (sqlite3AuthCheck: the generator is not used with an authorizer)
    Some(i_col)
}

/// `alterFindTable(pParse, pSrc, piDb, pzDb, bAuth)`: find the table named by pSrc; it must be
/// an ordinary table that may be altered.
fn alter_find_table(p: &mut Parse, src: SrcListId) -> Option<(TableId, i32, String)> {
    let tab = crate::schema::sqlite3_locate_table_item(p, 0, src, 0);
    if tab.is_null() {
        return None;
    }
    let i_db = p.tab(tab).i_db;
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();
    if is_real_table(p, tab, 2) || is_alterable_table(p, tab) {
        return None;
    }
    Some((tab, i_db, z_db))
}

/// `sqlite3AlterDropConstraint(pParse, pSrc, pCons, pCol)`: ALTER TABLE pSrc DROP CONSTRAINT
/// pCons, or (pCons==0) ALTER TABLE pSrc ALTER COLUMN pCol DROP NOT NULL.
pub fn sqlite3_alter_drop_constraint(
    p: &mut Parse,
    src: SrcListId,
    cons: Option<&Token>,
    col: Option<&Token>,
) {
    let Some((tab, i_db, z_db)) = alter_find_table(p, src) else {
        return;
    };
    let z_arg = if let Some(c) = cons {
        let z = sqlite3_name_from_token(p, Some(c));
        pf_big_q(z.as_deref())
    } else {
        let Some(i_col) = alter_find_col(p, tab, col.expect("column")) else {
            return;
        };
        format!("{i_col}")
    };

    // Edit the SQL for the named table.
    let z_tab = p.tab(tab).z_name.clone();
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE \"{}\".{LEGACY_SCHEMA_TABLE} SET sql = sqlite_drop_constraint(sql, {z_arg}) WHERE type='table' AND tbl_name={} COLLATE nocase",
            pf_w(&z_db),
            pf_big_q(Some(&z_tab)),
        ),
    );

    // Finally, reload the database schema.
    rename_reload_schema(p, i_db, INITFLAG_ALTER_DROP_CONS);
}

/// `alterRtrimConstraint(db, pCons, nCons)`: the number of bytes of the constraint text
/// without trailing white space and trailing "--" comments.
fn alter_rtrim_constraint(z: &[u8]) -> usize {
    let mut i_off = 0;
    let mut i_end = 0;
    loop {
        let (n_token, t) = crate::tokenize::sqlite3_get_token(&z[i_off.min(z.len())..]);
        if t == TK_ILLEGAL || n_token == 0 {
            break;
        }
        if t != TK_SPACE && (t != TK_COMMENT || z[i_off] != b'-') {
            i_end = i_off + n_token;
        }
        i_off += n_token;
    }
    i_end
}

/// `sqlite3AlterSetNotNull(pParse, pSrc, pCol, pFirst)`: ALTER TABLE pSrc ALTER COLUMN pCol
/// SET NOT NULL ... (pFirst is the NOT token).
pub fn sqlite3_alter_set_not_null(p: &mut Parse, src: SrcListId, col: &Token, first: &Token) {
    let Some((tab, i_db, z_db)) = alter_find_table(p, src) else {
        return;
    };
    let Some(i_col) = alter_find_col(p, tab, col) else {
        return;
    };

    // Find the length in bytes of the constraint definition
    let b = p.z_sql.as_bytes();
    let cons_end = (p.s_last_token.z as usize).min(b.len());
    let cons = &b[first.z as usize..cons_end];
    let n_cons = alter_rtrim_constraint(cons);
    let z_cons = String::from_utf8_lossy(&cons[..n_cons]).into_owned();
    let z_colname = p.tok_str(col).to_owned();

    // Search for a NULL value in the column; if one exists, fail the statement
    let z_tab = p.tab(tab).z_name.clone();
    sqlite3_nested_parse(
        p,
        format!(
            "SELECT sqlite_fail('constraint failed', {}) FROM {}.{} AS x WHERE x.{z_colname} IS NULL",
            SQLITE_CONSTRAINT,
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_tab)),
        ),
    );

    // Edit the SQL for the named table.
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE \"{}\".{LEGACY_SCHEMA_TABLE} SET sql = sqlite_add_constraint(sqlite_drop_constraint(sql, {i_col}), {}, {i_col}) WHERE type='table' AND tbl_name={} COLLATE nocase",
            pf_w(&z_db),
            pf_big_q(Some(&z_cons)),
            pf_big_q(Some(&z_tab)),
        ),
    );

    // Finally, reload the database schema.
    rename_reload_schema(p, i_db, INITFLAG_ALTER_DROP_CONS);
}

/// `sqlite3AlterAddConstraint(pParse, pSrc, pFirst, pName, zExpr, nExpr, pExpr)`: ALTER TABLE
/// pSrc ADD [CONSTRAINT pName] CHECK(zExpr) ... (pFirst is the CONSTRAINT or CHECK token).
#[allow(clippy::too_many_arguments)]
pub fn sqlite3_alter_add_constraint(
    p: &mut Parse,
    src: SrcListId,
    first: &Token,
    name: Option<&Token>,
    z_expr: (usize, usize),
    e: ExprId,
) {
    let Some((tab, i_db, z_db)) = alter_find_table(p, src) else {
        return;
    };

    // If there is a name, check that it is not already in use as a constraint.
    if crate::resolve::sqlite3_resolve_self_reference(p, tab, NC_IsCheck, e, ExprListId::NULL) != 0
    {
        return;
    }
    let z_tab = p.tab(tab).z_name.clone();
    if let Some(n) = name {
        let z_name = sqlite3_name_from_token(p, Some(n)).unwrap_or_default();
        sqlite3_nested_parse(
            p,
            format!(
                "SELECT sqlite_fail('constraint {} already exists', {}) FROM \"{}\".{LEGACY_SCHEMA_TABLE} WHERE type='table' AND tbl_name={} COLLATE nocase AND sqlite_find_constraint(sql, {})",
                pf_q(&z_name),
                SQLITE_ERROR,
                pf_w(&z_db),
                pf_big_q(Some(&z_tab)),
                pf_big_q(Some(&z_name)),
            ),
        );
    }

    // Search for a row that does not satisfy the new constraint; if one exists, fail
    let b = p.z_sql.as_bytes();
    let expr_txt =
        String::from_utf8_lossy(&b[z_expr.0..(z_expr.0 + z_expr.1).min(b.len())]).into_owned();
    sqlite3_nested_parse(
        p,
        format!(
            "SELECT sqlite_fail('constraint failed', {}) FROM {}.{} WHERE ({expr_txt}) IS FALSE",
            SQLITE_CONSTRAINT,
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_tab)),
        ),
    );

    // Edit the SQL for the named table.
    let b = p.z_sql.as_bytes();
    let cons_end = (p.s_last_token.z as usize).min(b.len());
    let cons = &b[first.z as usize..cons_end];
    let n_cons = alter_rtrim_constraint(cons);
    let z_cons = String::from_utf8_lossy(&cons[..n_cons]).into_owned();
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE \"{}\".{LEGACY_SCHEMA_TABLE} SET sql = sqlite_add_constraint(sql, {}, -1) WHERE type='table' AND tbl_name={} COLLATE nocase",
            pf_w(&z_db),
            pf_big_q(Some(&z_cons)),
            pf_big_q(Some(&z_tab)),
        ),
    );

    // Finally, reload the database schema.
    rename_reload_schema(p, i_db, INITFLAG_ALTER_DROP_CONS);
}
