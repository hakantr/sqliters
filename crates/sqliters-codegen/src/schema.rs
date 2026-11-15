//! The schema adapter: C's `Table` / `Column` / `Index` objects built from the declarations
//! the engine hands over ([`TableDef`]), with the logic of `build.c` that shapes them
//! (`sqlite3AddColumn`, `sqlite3AddPrimaryKey`, `sqlite3CreateIndex`,
//! `convertToWithoutRowidTable`, `sqlite3EndTable`, `sqlite3DefaultRowEst`,
//! `estimateTableWidth` / `estimateIndexWidth`, `recomputeColumnsNotIndexed`) and the lookup
//! functions of `build.c` (`sqlite3FindTable`, `sqlite3LocateTable`, `sqlite3LocateTableItem`,
//! `sqlite3PrimaryKeyIndex`, `sqlite3TableColumnToStorage`, `sqlite3KeyInfoOfIndex`, …).
//!
//! C builds these objects once, when the schema is read (`sqlite3InitOne`), by running its
//! parser over the `CREATE` statements of `sqlite_schema` and then loading `sqlite_stat1`
//! (`sqlite3AnalysisLoad`). The engine has its own parsed schema; one adapter in the engine
//! (`sqliters-core/src/cgen.rs`) turns it into [`TableDef`]s, and this module derives
//! everything else exactly as `build.c` does, so that the query planner sees C's numbers.

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{KeyInfo, P4, P4Table};

use crate::callback::*;
use crate::consts::*;
use crate::sqliteint::*;

/// `sqlite3AffinityType(zIn, pCol)`: the column affinity of a declared type and the size
/// estimate `Column.szEst` (scaled so that the size of an integer is 1).
pub fn sqlite3_affinity_type_sz(z_in: &str) -> (u8, u8) {
    let b = z_in.as_bytes();
    let mut h: u32 = 0;
    let mut aff = SQLITE_AFF_NUMERIC;
    let mut z_char: Option<usize> = None;
    let mut i = 0;
    while i < b.len() {
        let x = b[i];
        h = (h << 8).wrapping_add(x.to_ascii_lowercase() as u32);
        i += 1;
        if h == u32::from_be_bytes(*b"char") {
            aff = SQLITE_AFF_TEXT;
            z_char = Some(i);
        } else if h == u32::from_be_bytes(*b"clob") {
            aff = SQLITE_AFF_TEXT;
        } else if h == u32::from_be_bytes(*b"text") {
            aff = SQLITE_AFF_TEXT;
        } else if h == u32::from_be_bytes(*b"blob") && (aff == SQLITE_AFF_NUMERIC || aff == SQLITE_AFF_REAL) {
            aff = SQLITE_AFF_BLOB;
            if i < b.len() && b[i] == b'(' {
                z_char = Some(i);
            }
        } else if h == u32::from_be_bytes(*b"real") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if h == u32::from_be_bytes(*b"floa") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if h == u32::from_be_bytes(*b"doub") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if (h & 0x00FF_FFFF) == ((b'i' as u32) << 16) + ((b'n' as u32) << 8) + b't' as u32 {
            aff = SQLITE_AFF_INTEGER;
            break;
        }
    }
    // If pCol is not NULL, store an estimate of the field size. The estimate is scaled so
    // that the size of an integer is 1.
    let mut v: i32 = 0; // default size is approx 4 bytes
    if aff < SQLITE_AFF_NUMERIC {
        if let Some(mut k) = z_char {
            while k < b.len() {
                if b[k].is_ascii_digit() {
                    // BLOB(k), VARCHAR(k), CHAR(k) -> r=(k/4+1)
                    let s = std::str::from_utf8(&b[k..]).unwrap_or("");
                    v = crate::util::sqlite3_get_int32(s).unwrap_or(0);
                    break;
                }
                k += 1;
            }
        } else {
            v = 16; // BLOB, TEXT, CLOB -> r=5  (approx 20 bytes)
        }
    }
    v = v / 4 + 1;
    if v > 255 {
        v = 255;
    }
    (aff, v as u8)
}

/// `sqlite3StdType[]` / `sqlite3StdTypeAffinity[]`
pub const STD_TYPE: [(&str, u8); 6] = [
    ("ANY", SQLITE_AFF_NUMERIC),
    ("BLOB", SQLITE_AFF_BLOB),
    ("INT", SQLITE_AFF_INTEGER),
    ("INTEGER", SQLITE_AFF_INTEGER),
    ("REAL", SQLITE_AFF_REAL),
    ("TEXT", SQLITE_AFF_TEXT),
];

/// `sqlite3ColumnColl(pCol)`: the collating sequence name for a column, `None` for the
/// default.
pub fn sqlite3_column_coll(col: &Column) -> Option<&str> {
    if (col.col_flags & COLFLAG_HASCOLL as u16) == 0 {
        return None;
    }
    col.z_coll.as_deref()
}

/// `sqlite3ColumnType(pCol, zDflt)`: the declared type of a column.
pub fn sqlite3_column_type<'a>(col: &'a Column, z_dflt: Option<&'a str>) -> Option<&'a str> {
    if (col.col_flags & COLFLAG_HASTYPE as u16) != 0 {
        col.z_type.as_deref()
    } else if col.e_c_type != 0 {
        Some(STD_TYPE[col.e_c_type as usize - 1].0)
    } else {
        z_dflt
    }
}

/// `sqlite3DefaultRowEst(pIdx)`: fill the Index.aiRowLogEst[] array with default information
/// - information to be used when we have not run the ANALYZE command.
///
/// aiRowLogEst[0] is supposed to contain the number of elements in the index. Since we do not
/// know, guess 1 million. aiRowLogEst[1] is an estimate of the number of rows returned by a
/// lookup on the first column of the index, and so on.
pub fn sqlite3_default_row_est(idx: &mut Index, tab_n_row_log_est: &mut LogEst) {
    const A_VAL: [LogEst; 5] = [33, 32, 30, 28, 26];
    let n_key_col = idx.n_key_col as usize;
    let n_copy = A_VAL.len().min(n_key_col);
    // (the array keeps its allocated size: nColumn+1 entries, or N after resizeIndexObject())
    if idx.ai_row_log_est.len() < n_key_col + 1 {
        idx.ai_row_log_est.resize(n_key_col + 1, 0);
    }
    // Set the first entry (number of rows in the index) to the estimated number of rows in
    // the table, or half the number of rows in the table for a partial index.
    //
    // 2020-05-27: If some of the stat data is coming from the sqlite_stat1 table but other
    // parts we are having to guess at, then do not let the estimated number of rows in the
    // table be less than 1000 (LogEst 99).
    let mut x = *tab_n_row_log_est;
    if x < 99 {
        x = 99;
        *tab_n_row_log_est = x;
    }
    if idx.p_part_idx_where.nn() {
        x -= 10;
    }
    idx.ai_row_log_est[0] = x;
    // Estimate that a[1] is 10, a[2] is 9, a[3] is 8, a[4] is 7, a[5] is 6 and each
    // subsequent value (if any) is 5.
    idx.ai_row_log_est[1..=n_copy].copy_from_slice(&A_VAL[..n_copy]);
    for i in n_copy + 1..=n_key_col {
        idx.ai_row_log_est[i] = 23;
    }
    if idx.is_unique() {
        idx.ai_row_log_est[n_key_col] = 0;
    }
}

/// `sqlite3PrimaryKeyIndex(pTab)`: return the PRIMARY KEY index of a table.
pub fn sqlite3_primary_key_index(p: &Parse, tab: TableId) -> IndexId {
    for &i in &p.tab(tab).p_index {
        if p.idx(i).is_primary_key() {
            return i;
        }
    }
    IndexId::NULL
}

/// `sqlite3TableColumnToIndex(pIdx, iCol)`: convert a table column number into an index
/// column number. That is, for the column iCol in the table (as defined by the CREATE TABLE
/// statement) find the (first) offset of that column in index pIdx. Or return -1 if column
/// iCol is not used in index pIdx.
pub fn sqlite3_table_column_to_index(idx: &Index, i_col: i32) -> i32 {
    let i_col16 = i_col as i16;
    for i in 0..idx.n_column as usize {
        if i_col16 == idx.ai_column[i] {
            return i as i32;
        }
    }
    -1
}

/// `sqlite3StorageColumnToTable(pTab, iCol)`: convert a storage column number into a table
/// column number.
pub fn sqlite3_storage_column_to_table(tab: &Table, mut i_col: i32) -> i32 {
    if tab.tab_flags & TF_HasVirtual != 0 {
        let mut i = 0;
        while i <= i_col {
            if tab.a_col[i as usize].col_flags & COLFLAG_VIRTUAL as u16 != 0 {
                i_col += 1;
            }
            i += 1;
        }
    }
    i_col
}

/// `sqlite3TableColumnToStorage(pTab, iCol)`: convert a table column number into a storage
/// column number.
pub fn sqlite3_table_column_to_storage(tab: &Table, i_col: i32) -> i32 {
    if (tab.tab_flags & TF_HasVirtual) == 0 || i_col < 0 {
        return i_col;
    }
    let mut n = 0;
    let mut i = 0;
    while i < i_col {
        if (tab.a_col[i as usize].col_flags & COLFLAG_VIRTUAL as u16) == 0 {
            n += 1;
        }
        i += 1;
    }
    if tab.a_col[i as usize].col_flags & COLFLAG_VIRTUAL as u16 != 0 {
        // iCol is a virtual column itself
        tab.n_nv_col as i32 + i - n
    } else {
        // iCol is a normal or stored column
        n
    }
}

/// `sqlite3FindTable(db, zName, zDatabase)`: locate the in-memory structure that describes a
/// particular database table given the name of that table and (optionally) the name of the
/// database containing the table. Return NULL if not found.
///
/// If zDatabase is 0, all databases are searched for the table and the first matching table
/// is returned. (No checking for duplicate table names is done.) The search order is TEMP
/// first, then MAIN, then any auxiliary databases added using the ATTACH command.
pub fn sqlite3_find_table(p: &mut Parse, z_name: &str, z_database: Option<&str>) -> TableId {
    const LEGACY_SCHEMA_TABLE: &str = "sqlite_master";
    const LEGACY_TEMP_SCHEMA_TABLE: &str = "sqlite_temp_master";
    const PREFERRED_SCHEMA_TABLE: &str = "sqlite_schema";
    const PREFERRED_TEMP_SCHEMA_TABLE: &str = "sqlite_temp_schema";
    let n_db = p.db.a_db.len();
    let is_sqlite_ = z_name.len() >= 7 && z_name.as_bytes()[..7].eq_ignore_ascii_case(b"sqlite_");
    let tail = if is_sqlite_ { &z_name[7..] } else { "" };
    if let Some(z_db) = z_database {
        let mut i = 0;
        while i < n_db {
            if z_db.eq_ignore_ascii_case(&p.db.a_db[i].z_db_s_name) {
                break;
            }
            i += 1;
        }
        if i >= n_db {
            // No match against the official names. But always match "main" to schema 0 as a
            // legacy fallback.
            if z_db.eq_ignore_ascii_case("main") {
                i = 0;
            } else {
                return TableId::NULL;
            }
        }
        let mut t = hash_find(p, i, z_name);
        if t.is_null() && is_sqlite_ {
            if i == 1 {
                if tail.eq_ignore_ascii_case(&PREFERRED_TEMP_SCHEMA_TABLE[7..])
                    || tail.eq_ignore_ascii_case(&PREFERRED_SCHEMA_TABLE[7..])
                    || tail.eq_ignore_ascii_case(&LEGACY_SCHEMA_TABLE[7..])
                {
                    t = hash_find(p, 1, LEGACY_TEMP_SCHEMA_TABLE);
                }
            } else if tail.eq_ignore_ascii_case(&PREFERRED_SCHEMA_TABLE[7..]) {
                t = hash_find(p, i, LEGACY_SCHEMA_TABLE);
            }
        }
        t
    } else {
        // Match against TEMP first
        let mut t = if n_db > 1 { hash_find(p, 1, z_name) } else { TableId::NULL };
        if t.nn() {
            return t;
        }
        // The main database is second
        t = hash_find(p, 0, z_name);
        if t.nn() {
            return t;
        }
        // Attached databases are in order of attachment
        for i in 2..n_db {
            t = hash_find(p, i, z_name);
            if t.nn() {
                break;
            }
        }
        if t.is_null() && is_sqlite_ {
            if tail.eq_ignore_ascii_case(&PREFERRED_SCHEMA_TABLE[7..]) {
                t = hash_find(p, 0, LEGACY_SCHEMA_TABLE);
            } else if tail.eq_ignore_ascii_case(&PREFERRED_TEMP_SCHEMA_TABLE[7..]) && n_db > 1 {
                t = hash_find(p, 1, LEGACY_TEMP_SCHEMA_TABLE);
            }
        }
        t
    }
}

/// `sqlite3HashFind(&db->aDb[i].pSchema->tblHash, zName)`
fn hash_find(p: &mut Parse, i_db: usize, z_name: &str) -> TableId {
    match p.a_schema.get(i_db) {
        Some(sch) => sch.tbl_hash.get(&z_name.to_ascii_lowercase()).copied().unwrap_or(TableId::NULL),
        None => TableId::NULL,
    }
}

/// `sqlite3ReadSchema(pParse)`: this routine is a no-op if the database schema is already
/// initialized. Otherwise, the schema is loaded (`sqlite3Init()`). C's `sqlite3Init()` runs
/// nested statements that read `sqlite_schema`, which reset `db->errByteOffset`: that happens
/// when C's connection had a schema to load (`Db.schema_not_loaded`, the engine's record of
/// C's DB_SchemaLoaded).
pub fn sqlite3_read_schema(p: &mut Parse) {
    if !p.db.init_busy {
        if p.a_schema.len() < p.db.a_db.len() || p.a_schema.iter().any(|s| !s.loaded) {
            crate::build_tab::sqlite3_init(p);
        }
        if p.db.schema_not_loaded {
            p.db.schema_not_loaded = false;
            p.err_byte_offset = -1;
            p.db.host.note_schema_loaded();
        }
    }
}

/// `sqlite3LocateTable(pParse, flags, zName, zDbase)`: locate the in-memory structure that
/// describes a particular database table given the name of that table and (optionally) the
/// name of the database containing the table. Return NULL if not found. Also leave an error
/// message in pParse->zErrMsg.
///
/// The difference between this routine and sqlite3FindTable() is that this routine leaves an
/// error message in pParse->zErrMsg where sqlite3FindTable() does not.
pub fn sqlite3_locate_table(p: &mut Parse, flags: u32, z_name: &str, z_dbase: Option<&str>) -> TableId {
    // Read the database schema. If an error occurs, leave an error message and code in
    // pParse and return NULL.
    sqlite3_read_schema(p);
    let mut t = sqlite3_find_table(p, z_name, z_dbase);
    if t.is_null() {
        // If zName is the not the name of a table in the schema created using CREATE, then
        // check to see if it is the name of an virtual table that can be an eponymous
        // virtual table.
        if (p.prep_flags as i32 & SQLITE_PREPARE_NO_VTAB) == 0 && !p.db.init_busy {
            // sqlite3HashFind(&db->aModule, zName), sqlite3PragmaVtabRegister(db, zName) for
            // "pragma_*", sqlite3JsonVtabRegister(db, zName) for "json*"
            let host = p.db.host;
            if let Some(m) = host.vtab_module(z_name, true)
                && let Some(epo) = crate::vtab::sqlite3_vtab_eponymous_table_init(p, &m)
            {
                return epo;
            }
            if p.unsupported.is_some() {
                return TableId::NULL;
            }
        }
        if !p.db.init_busy {
            let key = z_name.to_ascii_lowercase();
            if let Some(why) = p.a_schema.iter().find_map(|s| s.bad.get(&key).copied()) {
                p.not_supported(why);
                return TableId::NULL;
            }
        }
        if flags & LOCATE_NOERR as u32 != 0 {
            return TableId::NULL;
        }
        p.check_schema = true;
    } else if p.tab(t).is_virtual() && (p.prep_flags as i32 & SQLITE_PREPARE_NO_VTAB) != 0 {
        t = TableId::NULL;
    }
    if t.nn() && !p.db.init_busy {
        // An object the generator could not load, or state of C's connection that the engine
        // knows to differ: the statement is reported as unsupported.
        let (i_db, key) = (p.tab(t).i_db as usize, p.tab(t).z_name.to_ascii_lowercase());
        let why = p.a_schema.get(i_db).and_then(|s| s.bad.get(&key).copied()).or_else(|| {
            let host = p.db.host;
            host.table_unsupported(i_db, &key)
        });
        if let Some(why) = why {
            p.not_supported(why);
            return TableId::NULL;
        }
    }
    if t.is_null() {
        let z_msg = if flags & LOCATE_VIEW as u32 != 0 { "no such view" } else { "no such table" };
        match z_dbase {
            Some(d) => p.error_msg(format!("{z_msg}: {d}.{z_name}")),
            None => p.error_msg(format!("{z_msg}: {z_name}")),
        }
    }
    t
}

/// `sqlite3LocateTableItem(pParse, flags, p)`: locate the table identified by a SrcItem.
pub fn sqlite3_locate_table_item(p: &mut Parse, flags: u32, src: SrcListId, i: usize) -> TableId {
    let (z_name, z_db) = {
        let it = &p.src(src).a[i];
        let z_db = if it.fixed_schema {
            Some(p.db.a_db[it.i_fixed_db as usize].z_db_s_name.clone())
        } else {
            it.z_database.clone()
        };
        (it.z_name.clone().unwrap_or_default(), z_db)
    };
    sqlite3_locate_table(p, flags, &z_name, z_db.as_deref())
}

/// `sqlite3KeyInfoOfIndex(pParse, pIdx)`: return a KeyInfo structure that is appropriate for
/// the given Index.
pub fn sqlite3_key_info_of_index(p: &mut Parse, idx: IndexId) -> Option<KeyInfo> {
    let (n_col, n_key, uniq) = {
        let i = p.idx(idx);
        (i.n_column as usize, i.n_key_col as usize, i.uniq_not_null)
    };
    if p.n_err != 0 {
        return None;
    }
    let mut key = if uniq {
        KeyInfo::alloc(p.enc(), n_key, n_col - n_key)
    } else {
        KeyInfo::alloc(p.enc(), n_col, 0)
    };
    for i in 0..n_col {
        let (z_coll, is_static) = (p.idx(idx).az_coll[i].clone(), p.idx(idx).a_coll_static[i]);
        // zColl==sqlite3StrBINARY ? 0 : sqlite3LocateCollSeq(pParse, zColl)
        key.a_coll[i] = if is_static { None } else { sqlite3_locate_coll_seq(p, &z_coll) };
        key.a_sort_flags[i] = p.idx(idx).a_sort_order[i];
    }
    if p.n_err != 0 {
        // (pIdx->bNoQuery: the collating sequence of an index is not registered. The index
        // cannot be used; C marks it and sets pParse->rc = SQLITE_ERROR_RETRY)
        return None;
    }
    Some(key)
}

/// `sqlite3VdbeSetP4KeyInfo(pParse, pIdx)`: set the P4 on the most recently added opcode to
/// the KeyInfo for the index pIdx.
pub fn sqlite3_vdbe_set_p4_key_info(p: &mut Parse, idx: IndexId) {
    if let Some(k) = sqlite3_key_info_of_index(p, idx) {
        p.v.append_p4(P4::KeyInfo(Arc::new(k)));
    }
}

/// `sqlite3TableLock(pParse, iDb, iTab, isWriteLock, zName)`: record the fact that we want to
/// lock a table at run-time. Shared-cache mode only: no btree of the engine is sharable, so
/// nothing is recorded (`if( !sqlite3BtreeSharable(pBtree) ) return`).
pub fn sqlite3_table_lock(_p: &mut Parse, _i_db: i32, _i_tab: u32, _is_write_lock: u8, _z_name: &str) {}

/// `sqlite3OpenTable(pParse, iCur, iDb, pTab, opcode)`: generate code that will open cursor
/// number `i_cur` for a table `tab` in database `i_db` for reading (`OP_OpenRead`) or writing
/// (`OP_OpenWrite`).
pub fn sqlite3_open_table(p: &mut Parse, i_cur: i32, i_db: i32, tab: TableId, opcode: u8) {
    let (tnum, z_name, has_rowid, n_nv_col) = {
        let t = p.tab(tab);
        (t.tnum, t.z_name.clone(), t.has_rowid(), t.n_nv_col as i32)
    };
    if p.within_rj_subrtn == 0 {
        sqlite3_table_lock(p, i_db, tnum, (opcode == OP_OpenWrite) as u8, &z_name);
    }
    if has_rowid {
        p.v.add_op4_int(opcode, i_cur, tnum as i32, i_db, n_nv_col);
    } else {
        let pk = sqlite3_primary_key_index(p, tab);
        let pk_tnum = p.idx(pk).tnum;
        p.v.add_op3(opcode, i_cur, pk_tnum as i32, i_db);
        sqlite3_vdbe_set_p4_key_info(p, pk);
    }
}

/// `sqlite3IndexAffinityStr(db, pIdx)`: return a pointer to the column affinity string
/// associated with index pIdx. A column affinity string has one character for each column in
/// the table, according to the affinity of the column:
///
/// ```text
///  Character      Column affinity
///  ------------------------------
///  'A'            BLOB
///  'B'            TEXT
///  'C'            NUMERIC
///  'D'            INTEGER
///  'F'            REAL
/// ```
///
/// An extra 'D' is appended to the end of the string to cover the rowid that appears as the
/// last column in every index.
pub fn sqlite3_index_affinity_str(p: &mut Parse, idx: IndexId) -> Vec<u8> {
    if let Some(z) = &p.idx(idx).z_col_aff {
        return z.clone();
    }
    // computeIndexAffStr(db, pIdx)
    let tab = p.idx(idx).p_table;
    let n = p.idx(idx).n_column as usize;
    let mut z = Vec::with_capacity(n);
    for i in 0..n {
        let x = p.idx(idx).ai_column[i] as i32;
        let mut aff = if x >= 0 {
            p.tab(tab).a_col[x as usize].affinity
        } else if x == XN_ROWID {
            SQLITE_AFF_INTEGER
        } else {
            let el = p.idx(idx).a_col_expr;
            let e = p.l(el).a[i].p_expr;
            crate::expr::sqlite3_expr_affinity(p, e)
        };
        if aff < SQLITE_AFF_BLOB {
            aff = SQLITE_AFF_BLOB;
        }
        if aff > SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_NUMERIC;
        }
        z.push(aff);
    }
    p.idxm(idx).z_col_aff = Some(z.clone());
    z
}

/// `sqlite3IndexColumnAffinity(db, pIdx, iCol)`: return the affinity for a single column of
/// an index.
pub fn sqlite3_index_column_affinity(p: &mut Parse, idx: IndexId, i_col: i32) -> u8 {
    sqlite3_index_affinity_str(p, idx)[i_col as usize]
}

/// The P4 of an `OP_OpenRead` on a table of a schema with shared-cache locks would be a
/// `P4_TABLE`; kept for completeness of the operand kinds.
#[allow(dead_code)]
fn p4_table(name: &str) -> P4 {
    P4::Table(Arc::new(P4Table { name: name.to_owned() }))
}


/// The P4_TABLE operand for a table (`OP_TypeCheck`).
pub fn p4_table_of(p: &Parse, tab: TableId) -> P4 {
    p4_table(&p.tab(tab).z_name)
}

/// The P4_INDEX operand for an index (`OP_IdxDelete`).
pub fn p4_index_of(p: &Parse, idx: IndexId) -> P4 {
    P4::Index(Arc::new(sqliters_vdbe::vdbeaux::P4Index { name: p.idx(idx).z_name.clone() }))
}
