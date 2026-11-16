//! Code generation for schema statements (step V6): the DDL parts of `build.c` (CREATE /
//! DROP TABLE, VIEW and INDEX, REINDEX, `sqlite3NestedParse`, `sqlite3ChangeCookie`,
//! `sqlite3RefillIndex`, `destroyRootPage`), `vacuum.c` (`sqlite3Vacuum`) and `attach.c`
//! (`codeAttach`). The schema-object builders shared with the schema load (`sqlite3StartTable`,
//! `sqlite3EndTable`, `sqlite3CreateIndex`, …) are in [`crate::build_tab`]; they call the
//! code-generating parts below when `db->init.busy` is clear.

use crate::build::*;
use crate::consts::*;
use crate::sqliteint::*;
use crate::tk::*;
use crate::util::*;
use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::P4;

/// `LEGACY_SCHEMA_TABLE`
pub const LEGACY_SCHEMA_TABLE: &str = "sqlite_master";
/// `SCHEMA_ROOT`
pub const SCHEMA_ROOT: i32 = 1;
/// `BTREE_SCHEMA_VERSION`, `BTREE_FILE_FORMAT`, `BTREE_TEXT_ENCODING`
pub const BTREE_SCHEMA_VERSION: i32 = 1;
pub const BTREE_FILE_FORMAT: i32 = 2;
pub const BTREE_TEXT_ENCODING: i32 = 5;
/// `BTREE_INTKEY`, `BTREE_BLOBKEY`
pub const BTREE_INTKEY: i32 = 1;
pub const BTREE_BLOBKEY: i32 = 2;
/// `SQLITE_MAX_FILE_FORMAT`
const SQLITE_MAX_FILE_FORMAT: i32 = 4;
/// `PAGER_JOURNALMODE_QUERY`
const PAGER_JOURNALMODE_QUERY: i32 = -1;

// ---------------------------------------------------------------------------------------------
// printf.c conversions used by the nested SQL texts
// ---------------------------------------------------------------------------------------------

/// `%Q`: the string in single quotes with every `'` doubled, `NULL` for a NULL pointer.
pub fn pf_big_q(z: Option<&str>) -> String {
    match z {
        Some(s) => format!("'{}'", s.replace('\'', "''")),
        None => "NULL".to_owned(),
    }
}

/// `%q`: the string with every `'` doubled.
pub fn pf_q(z: &str) -> String {
    z.replace('\'', "''")
}

/// `%w`: the string with every `"` doubled.
pub fn pf_w(z: &str) -> String {
    z.replace('"', "\"\"")
}

// ---------------------------------------------------------------------------------------------
// build.c helpers
// ---------------------------------------------------------------------------------------------

/// `sqlite3NestedParse(pParse, zFormat, ...)`: run the parser and code generator recursively
/// in order to generate code for the SQL statement given (already formatted by the caller).
/// The code is added to the program of `p`. The part of the Parse object below
/// `sLastToken` (`PARSE_TAIL`) is saved, zeroed for the nested parse and restored.
pub fn sqlite3_nested_parse(p: &mut Parse, z_sql: String) {
    if p.n_err != 0 {
        return;
    }
    if p.e_parse_mode != 0 {
        return;
    }
    let saved_db_flags = p.db.m_db_flags;
    p.nested += 1;
    // memcpy(saveBuf, PARSE_TAIL(pParse), PARSE_TAIL_SZ); memset(PARSE_TAIL(pParse), 0, ...)
    let saved_sql = std::mem::replace(&mut p.z_sql, z_sql);
    let saved_last = std::mem::replace(&mut p.s_last_token, Token::EMPTY);
    let saved_n_var = std::mem::replace(&mut p.n_var, 0);
    let saved_vnbmc = std::mem::take(&mut p.a_vnbmc);
    let saved_pk_order = std::mem::replace(&mut p.i_pk_sort_order, 0);
    let saved_explain = std::mem::replace(&mut p.explain, 0);
    let saved_mode = std::mem::replace(&mut p.e_parse_mode, 0);
    let saved_vtab_lock = std::mem::take(&mut p.ap_vtab_lock);
    let saved_height = std::mem::replace(&mut p.n_height, 0);
    let saved_addr_explain = std::mem::replace(&mut p.addr_explain, 0);
    let saved_vlist = std::mem::take(&mut p.p_v_list);
    let saved_tail = std::mem::replace(&mut p.z_tail, 0);
    let saved_new_table = std::mem::replace(&mut p.p_new_table, TableId::NULL);
    let saved_new_trigger_flag = std::mem::replace(&mut p.p_new_trigger, false);
    let saved_new_trigger = p.new_trigger.take();
    let saved_s_arg = std::mem::replace(&mut p.s_arg, Token::EMPTY);
    let saved_with = std::mem::replace(&mut p.p_with, WithId::NULL);
    // (the token-valued fields above sLastToken refer to the text of the outer statement)
    let saved_name_token = p.s_name_token;
    let saved_cname = p.cr_constraint_name;
    p.db.m_db_flags |= DBFLAG_PreferBuiltin as u32;
    crate::tokenize::sqlite3_run_parser(p);
    p.db.m_db_flags = saved_db_flags;
    p.z_sql = saved_sql;
    p.s_last_token = saved_last;
    p.n_var = saved_n_var;
    p.a_vnbmc = saved_vnbmc;
    p.i_pk_sort_order = saved_pk_order;
    p.explain = saved_explain;
    p.e_parse_mode = saved_mode;
    p.ap_vtab_lock = saved_vtab_lock;
    p.n_height = saved_height;
    p.addr_explain = saved_addr_explain;
    p.p_v_list = saved_vlist;
    p.z_tail = saved_tail;
    p.p_new_table = saved_new_table;
    p.p_new_trigger = saved_new_trigger_flag;
    p.new_trigger = saved_new_trigger;
    p.s_arg = saved_s_arg;
    p.p_with = saved_with;
    p.s_name_token = saved_name_token;
    p.cr_constraint_name = saved_cname;
    p.nested -= 1;
}

/// `sqlite3ChangeCookie(pParse, iDb)`: generate code that will increment the schema cookie.
pub fn sqlite3_change_cookie(p: &mut Parse, i_db: i32) {
    let cookie = p.db.a_db[i_db as usize].schema_cookie;
    p.v.add_op3(
        OP_SetCookie,
        i_db,
        BTREE_SCHEMA_VERSION,
        (1u32.wrapping_add(cookie as u32)) as i32,
    );
}

/// `sqlite3OpenSchemaTable(p, iDb)`: open the sqlite_schema table stored in database number
/// iDb for writing. The table is opened using cursor 0.
pub fn sqlite3_open_schema_table(p: &mut Parse, i_db: i32) {
    crate::select::sqlite3_get_vdbe(p);
    // (sqlite3TableLock: shared-cache table locks are not used)
    p.v.add_op4_int(OP_OpenWrite, 0, SCHEMA_ROOT, i_db, 5);
    if p.n_tab == 0 {
        p.n_tab = 1;
    }
}

/// `sqlite3VdbeAddParseSchemaOp(v, iDb, zWhere, p5)`: add an OP_ParseSchema opcode. This
/// routine is broken out from sqlite3VdbeAddOp4() since it needs to also needs to mark all
/// btrees as having been accessed.
pub fn sqlite3_vdbe_add_parse_schema_op(p: &mut Parse, i_db: i32, z_where: Option<&str>, p5: u16) {
    match z_where {
        Some(w) => p.v.add_op4(OP_ParseSchema, i_db, 0, 0, P4::dynamic(w)),
        None => p.v.add_op4(OP_ParseSchema, i_db, 0, 0, P4::NotUsed),
    };
    p.v.change_p5(p5);
    for j in 0..p.db.a_db.len() {
        let sharable = p.db.a_db[j].sharable;
        p.v.uses_btree(j as i32, sharable);
    }
    sqlite3_may_abort(p);
}

/// `sqlite3ForceNotReadOnly(pParse)`: when a CREATE ... IF NOT EXISTS or DROP ... IF EXISTS
/// statement finds nothing to do, an OP_JournalMode query keeps the statement from being
/// read-only (sqlite3_stmt_readonly()).
pub fn sqlite3_force_not_read_only(p: &mut Parse) {
    p.n_mem += 1;
    let i_reg = p.n_mem;
    crate::select::sqlite3_get_vdbe(p);
    p.v.add_op3(OP_JournalMode, 0, i_reg, PAGER_JOURNALMODE_QUERY);
    let sharable = p.db.a_db[0].sharable;
    p.v.uses_btree(0, sharable);
}

/// `sqlite3CodeVerifyNamedSchema(pParse, zDb)`: if `z_db` is None, verify the schema of every
/// opened database; otherwise of the opened database of that name.
pub fn sqlite3_code_verify_named_schema(p: &mut Parse, z_db: Option<&str>) {
    for i in 0..p.db.a_db.len() {
        let d = &p.db.a_db[i];
        if d.has_bt && z_db.is_none_or(|z| z.eq_ignore_ascii_case(&d.z_db_s_name)) {
            sqlite3_code_verify_schema(p, i as i32);
        }
    }
}

/// `sqlite3ReadOnlyShadowTables(db)`: true if shadow tables are read-only (SQLITE_Defensive
/// outside of a virtual table constructor and of a running statement).
pub fn sqlite3_read_only_shadow_tables(p: &Parse) -> bool {
    (p.db.flags & SQLITE_Defensive) != 0 && p.vtab_ctx.is_empty()
}

/// `identLength(z)`: the number of bytes of `z` as an identifier in double quotes.
fn ident_length(z: &str) -> i64 {
    z.bytes()
        .map(|c| if c == b'"' { 2 } else { 1 })
        .sum::<i64>()
        + 2
}

/// `identPut(z, pIdx, zSignedIdent)`: append the identifier, quoted if it needs to be.
fn ident_put(out: &mut Vec<u8>, z_ident: &str) {
    let b = z_ident.as_bytes();
    let mut j = 0;
    while j < b.len() {
        if !b[j].is_ascii_alphanumeric() && b[j] != b'_' {
            break;
        }
        j += 1;
    }
    let need_quote = b.first().is_some_and(|c| c.is_ascii_digit())
        || crate::tokenize::keyword_code(&b[..j]) != TK_ID
        || j < b.len()
        || j == 0;
    if need_quote {
        out.push(b'"');
    }
    for &c in b {
        out.push(c);
        if c == b'"' {
            out.push(b'"');
        }
    }
    if need_quote {
        out.push(b'"');
    }
}

/// `createTableStmt(db, p)`: generate a CREATE TABLE statement appropriate for the given table
/// (CREATE TABLE ... AS SELECT).
pub fn create_table_stmt(p: &Parse, tab: TableId) -> String {
    let t = p.tab(tab);
    let mut n: i64 = 0;
    for c in &t.a_col {
        n += ident_length(&c.z_cn_name) + 5;
    }
    n += ident_length(&t.z_name);
    let (mut z_sep, z_sep2, z_end) = if n < 50 {
        ("", ",", ")")
    } else {
        ("\n  ", ",\n  ", "\n)")
    };
    let mut out: Vec<u8> = b"CREATE TABLE ".to_vec();
    ident_put(&mut out, &t.z_name);
    out.push(b'(');
    const AZ_TYPE: [&str; 6] = ["", " TEXT", " NUM", " INT", " REAL", " NUM"];
    for c in &t.a_col {
        out.extend_from_slice(z_sep.as_bytes());
        z_sep = z_sep2;
        ident_put(&mut out, &c.z_cn_name);
        let k = c.affinity.wrapping_sub(SQLITE_AFF_BLOB) as usize;
        out.extend_from_slice(AZ_TYPE.get(k).copied().unwrap_or("").as_bytes());
    }
    out.extend_from_slice(z_end.as_bytes());
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------------------------
// CREATE TABLE / VIEW / VIRTUAL TABLE: the code-generating parts
// ---------------------------------------------------------------------------------------------

/// The part of `sqlite3StartTable()` under `if( !db->init.busy && (v = sqlite3GetVdbe(pParse))!=0 )`:
/// begin generating the code that will insert the table record into the schema table. Note
/// in particular that we must go ahead and allocate the record number for the table entry
/// now. Before any PRIMARY KEY or UNIQUE keywords are parsed. Those keywords will cause
/// indices to be created and the table record must come before the indices. Hence, the
/// record number for the table must be allocated now.
pub fn start_table_code(p: &mut Parse, i_db: i32, is_view: bool, is_virtual: bool) {
    crate::select::sqlite3_get_vdbe(p);
    sqlite3_begin_write_operation(p, true, i_db);
    if is_virtual {
        p.v.add_op0(OP_VBegin);
    }

    // If the file format and encoding in the database have not been set, set them now.
    p.n_mem += 1;
    let reg1 = p.n_mem;
    p.cr_reg_rowid = reg1;
    p.n_mem += 1;
    let reg2 = p.n_mem;
    p.cr_reg_root = reg2;
    p.n_mem += 1;
    let reg3 = p.n_mem;
    p.v.add_op3(OP_ReadCookie, i_db, reg3, BTREE_FILE_FORMAT);
    let sharable = p.db.a_db[i_db as usize].sharable;
    p.v.uses_btree(i_db, sharable);
    let addr1 = p.v.add_op1(OP_If, reg3);
    let file_format = if (p.db.flags & SQLITE_LegacyFileFmt) != 0 {
        1
    } else {
        SQLITE_MAX_FILE_FORMAT
    };
    p.v.add_op3(OP_SetCookie, i_db, BTREE_FILE_FORMAT, file_format);
    let enc = p.db.enc as i32;
    p.v.add_op3(OP_SetCookie, i_db, BTREE_TEXT_ENCODING, enc);
    p.v.jump_here(addr1);

    // This just creates a place-holder record in the sqlite_schema table. The record created
    // does not contain anything yet. It will be replaced by the real record by
    // sqlite3EndTable().
    //
    // The rowid for the new entry is left in register pParse->u1.cr.regRowid. The root page
    // of the new table is left in reg pParse->u1.cr.regRoot. The rowid and root page number
    // values are needed by sqlite3EndTable().
    if is_view || is_virtual {
        p.v.add_op2(OP_Integer, 0, reg2);
    } else {
        p.cr_addr_cr_tab = p.v.add_op3(OP_CreateBtree, i_db, reg2, BTREE_INTKEY);
    }
    sqlite3_open_schema_table(p, i_db);
    p.v.add_op2(OP_NewRowid, 0, reg1);
    // static const char nullRow[] = { 6, 0, 0, 0, 0, 0 };
    p.v.add_op4(OP_Blob, 6, reg3, 0, P4::Dynamic(vec![6, 0, 0, 0, 0, 0]));
    p.v.add_op3(OP_Insert, 0, reg3, reg1);
    p.v.change_p5(OPFLAG_APPEND as u16);
    p.v.add_op0(OP_Close);
}

/// The part of `sqlite3EndTable()` under `if( !db->init.busy )`: the code that writes the
/// schema record of a new table or view (`cons` is pCons, `end` is pEnd).
pub fn end_table_code(
    p: &mut Parse,
    tab: TableId,
    select: SelectId,
    tab_opts: u32,
    end: Option<&Token>,
) {
    let i_db = p.tab(tab).i_db;
    crate::select::sqlite3_get_vdbe(p);
    p.v.add_op1(OP_Close, 0);

    // Initialize zType for the new view or table.
    let (z_type, z_type2) = if p.tab(tab).is_ordinary() {
        ("table", "TABLE")
    } else {
        ("view", "VIEW")
    };

    // If this is a CREATE TABLE xx AS SELECT ..., execute the SELECT statement to populate the
    // new table. If this is a WITHOUT ROWID table, the PRIMARY KEY is the btree key so the
    // statement transaction (OP_OpenWrite with OPFLAG_P2ISREG) is needed.
    if select.nn() {
        if p.e_parse_mode != 0 {
            p.rc = SQLITE_ERROR;
            p.n_err += 1;
            return;
        }
        let i_csr = p.n_tab;
        p.n_tab += 1;
        p.n_mem += 1;
        let reg_yield = p.n_mem;
        p.n_mem += 1;
        let reg_rec = p.n_mem;
        p.n_mem += 1;
        let reg_rowid = p.n_mem;
        sqlite3_may_abort(p);
        let reg_root = p.cr_reg_root;
        p.v.add_op3(OP_OpenWrite, i_csr, reg_root, i_db);
        p.v.change_p5(OPFLAG_P2ISREG as u16);
        let addr_top = p.v.current_addr() + 1;
        p.v.add_op3(OP_InitCoroutine, reg_yield, 0, addr_top);
        if p.n_err != 0 {
            return;
        }
        let sel_tab = crate::select::sqlite3_result_set_of_select(p, select, SQLITE_AFF_BLOB);
        if sel_tab.is_null() {
            return;
        }
        let cols = std::mem::take(&mut p.tabm(sel_tab).a_col);
        {
            let t = p.tabm(tab);
            t.n_nv_col = cols.len() as i16;
            t.a_col = cols;
        }
        let mut dest = crate::select::sqlite3_select_dest_init(SRT_Coroutine as i32, reg_yield);
        crate::select::sqlite3_select(p, select, &mut dest);
        if p.n_err != 0 {
            return;
        }
        p.v.end_coroutine(reg_yield);
        p.n_temp_reg = 0;
        p.n_range_reg = 0;
        p.v.jump_here(addr_top - 1);
        let addr_ins_loop = p.v.add_op1(OP_Yield, dest.i_sd_parm);
        p.v.add_op3(OP_MakeRecord, dest.i_sdst, dest.n_sdst, reg_rec);
        crate::insert::sqlite3_table_affinity(p, tab, 0);
        p.v.add_op2(OP_NewRowid, i_csr, reg_rowid);
        p.v.add_op3(OP_Insert, i_csr, reg_rec, reg_rowid);
        p.v.goto(addr_ins_loop);
        p.v.jump_here(addr_ins_loop);
        p.v.add_op1(OP_Close, i_csr);
    }

    // Compute the complete text of the CREATE statement
    let z_stmt = if select.nn() {
        create_table_stmt(p, tab)
    } else {
        let end2 = if tab_opts != 0 {
            p.s_last_token
        } else {
            *end.expect("pEnd")
        };
        let name_z = p.s_name_token.z as usize;
        let mut n = end2.z as usize - name_z;
        let first = p
            .z_sql
            .as_bytes()
            .get(end2.z as usize)
            .copied()
            .unwrap_or(0);
        if first != b';' {
            n += end2.n as usize;
        }
        let b = p.z_sql.as_bytes();
        let txt = String::from_utf8_lossy(&b[name_z..(name_z + n).min(b.len())]).into_owned();
        format!("CREATE {z_type2} {txt}")
    };

    // A slot for the record has already been allocated in the schema table. We just need to
    // update that slot with all the information we've collected.
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();
    let z_name = p.tab(tab).z_name.clone();
    let (reg_root, reg_rowid) = (p.cr_reg_root, p.cr_reg_rowid);
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE {}.{LEGACY_SCHEMA_TABLE} SET type='{z_type}', name={}, tbl_name={}, rootpage=#{reg_root}, sql={} WHERE rowid=#{reg_rowid}",
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_name)),
            pf_big_q(Some(&z_name)),
            pf_big_q(Some(&z_stmt)),
        ),
    );
    sqlite3_change_cookie(p, i_db);

    // Check to see if we need to create an sqlite_sequence table for keeping track of
    // autoincrement keys.
    if (p.tab(tab).tab_flags & TF_Autoincrement) != 0 && p.e_parse_mode == 0 {
        if p.a_schema[i_db as usize].p_seq_tab.is_null() {
            sqlite3_nested_parse(
                p,
                format!(
                    "CREATE TABLE {}.sqlite_sequence(name,seq)",
                    pf_big_q(Some(&z_db))
                ),
            );
        }
    }

    // Reparse everything to update our internal data structures
    let w = format!("tbl_name='{}' AND type!='trigger'", pf_q(&z_name));
    sqlite3_vdbe_add_parse_schema_op(p, i_db, Some(&w), 0);

    // Test for cycles in generated columns and illegal expressions in CHECK constraints and
    // in DEFAULT clauses.
    if (p.tab(tab).tab_flags & TF_HasGenerated) != 0 {
        let s = format!("SELECT*FROM\"{}\".\"{}\"", pf_w(&z_db), pf_w(&z_name));
        p.v.add_op4(OP_SqlExec, 0x0001, 0, 0, P4::dynamic(&s));
    }
}

// ---------------------------------------------------------------------------------------------
// CREATE INDEX: the code-generating parts
// ---------------------------------------------------------------------------------------------

/// `sqlite3RefillIndex(pParse, pIndex, memRootPage)`: generate code that will erase and refill
/// index *pIdx. This is used to initialize a newly created index or to recompute the content
/// of an index in response to a REINDEX command.
///
/// if memRootPage is not negative, it means that the index is newly created. The register
/// specified by memRootPage contains the root page number of the index. If memRootPage is
/// negative, then the index already exists and must be cleared before being refilled and the
/// root page number of the index is taken from pIndex->tnum.
pub fn sqlite3_refill_index(p: &mut Parse, ix: IndexId, mem_root_page: i32) {
    let tab = p.idx(ix).p_table;
    let i_tab = p.n_tab;
    p.n_tab += 1;
    let i_idx = p.n_tab;
    p.n_tab += 1;
    let i_db = p.idx(ix).i_db;
    // (sqlite3AuthCheck(SQLITE_REINDEX): the generator is not used with an authorizer;
    // sqlite3TableLock: shared-cache table locks are not used)
    crate::select::sqlite3_get_vdbe(p);
    let tnum: i32 = if mem_root_page >= 0 {
        mem_root_page
    } else {
        p.idx(ix).tnum as i32
    };
    let key = crate::schema::sqlite3_key_info_of_index(p, ix);

    // Open the sorter cursor if we are to use one.
    let i_sorter = p.n_tab;
    p.n_tab += 1;
    let n_key_col = p.idx(ix).n_key_col as i32;
    let key_p4 = || match &key {
        Some(k) => P4::KeyInfo(std::sync::Arc::new(k.clone())),
        None => P4::NotUsed,
    };
    p.v.add_op4(OP_SorterOpen, i_sorter, 0, n_key_col, key_p4());

    // Open the table. Loop through all rows of the table, inserting index records into the
    // sorter.
    crate::schema::sqlite3_open_table(p, i_tab, i_db, tab, OP_OpenRead);
    let addr1 = p.v.add_op2(OP_Rewind, i_tab, 0);
    let reg_record = crate::expr::sqlite3_get_temp_reg(p);
    sqlite3_multi_write(p);

    let mut i_part_idx_label = 0;
    crate::delete::sqlite3_generate_index_key_full(
        p,
        ix,
        i_tab,
        reg_record,
        false,
        Some(&mut i_part_idx_label),
        IndexId::NULL,
        0,
    );
    p.v.add_op2(OP_SorterInsert, i_sorter, reg_record);
    crate::delete::sqlite3_resolve_part_idx_label(p, i_part_idx_label);
    p.v.add_op2(OP_Next, i_tab, addr1 + 1);
    p.v.jump_here(addr1);
    if mem_root_page < 0 {
        p.v.add_op2(OP_Clear, tnum, i_db);
    }
    p.v.add_op4(OP_OpenWrite, i_idx, tnum, i_db, key_p4());
    p.v.change_p5(
        (OPFLAG_BULKCSR
            | if mem_root_page >= 0 {
                OPFLAG_P2ISREG
            } else {
                0
            }) as u16,
    );

    let addr1 = p.v.add_op2(OP_SorterSort, i_sorter, 0);
    let addr2;
    if p.idx(ix).is_unique() {
        let j2 = p.v.goto(1);
        addr2 = p.v.current_addr();
        p.v.add_op4_int(OP_SorterCompare, i_sorter, j2, reg_record, n_key_col);
        sqlite3_unique_constraint(p, OE_Abort, ix);
        p.v.jump_here(j2);
    } else {
        // Most CREATE INDEX and REINDEX statements that are not UNIQUE can not abort. The
        // exception is if one of the indexed expressions contains a user function that
        // throws an exception when it is evaluated. But the overhead of adding a statement
        // journal to a CREATE INDEX statement is very small (since most of the pages written
        // do not originally exist in the database), so we just set the mayAbort flag.
        sqlite3_may_abort(p);
        addr2 = p.v.current_addr();
    }
    p.v.add_op3(OP_SorterData, i_sorter, reg_record, i_idx);
    if !p.idx(ix).b_asc_key_bug {
        // This OP_SeekEnd opcode makes index insert for a REINDEX go much faster by causing
        // the btree to be positioned at the end of the index so that the next insert will
        // land on the right-most leaf.
        p.v.add_op1(OP_SeekEnd, i_idx);
    }
    p.v.add_op2(OP_IdxInsert, i_idx, reg_record);
    p.v.change_p5(OPFLAG_USESEEKRESULT as u16);
    crate::expr::sqlite3_release_temp_reg(p, reg_record);
    p.v.add_op2(OP_SorterNext, i_sorter, addr2);
    p.v.jump_here(addr1);

    p.v.add_op1(OP_Close, i_tab);
    p.v.add_op1(OP_Close, i_idx);
    p.v.add_op1(OP_Close, i_sorter);
}

/// The part of `sqlite3CreateIndex()` that is "else if( HasRowid(pTab) || pTblName!=0 )":
/// create an rootpage for the index on disk, write the schema record, and fill the index.
/// `start` is pStart (the CREATE token; None for an index of a CREATE TABLE constraint).
pub fn create_index_code(
    p: &mut Parse,
    ix: IndexId,
    i_db: i32,
    has_tbl_name: bool,
    name: &Token,
    start: Option<&Token>,
    on_error: i32,
) {
    p.n_mem += 1;
    let i_mem = p.n_mem;
    crate::select::sqlite3_get_vdbe(p);
    sqlite3_begin_write_operation(p, true, i_db);

    // Create the rootpage for the index using CreateIndex. But before doing so, code a Noop
    // instruction and store its address in pIndex->tnum. This is used by the code generated
    // by sqlite3RefillIndex() to determine where it should start writing. It is also
    // converted into a Goto in convertToWithoutRowidTable() if the table this index is on
    // becomes a WITHOUT ROWID table.
    let noop = p.v.add_op0(OP_Noop);
    p.idxm(ix).tnum = noop as u32;
    p.v.add_op3(OP_CreateBtree, i_db, i_mem, BTREE_BLOBKEY);

    // Gather the complete text of the CREATE INDEX statement into the zStmt variable
    let z_stmt: Option<String> = if start.is_some() {
        let last = p.s_last_token;
        let b = p.z_sql.as_bytes();
        let mut n = (last.z as usize + last.n as usize) - name.z as usize;
        if n > 0 && b.get(name.z as usize + n - 1) == Some(&b';') {
            n -= 1;
        }
        let txt = String::from_utf8_lossy(&b[name.z as usize..(name.z as usize + n).min(b.len())])
            .into_owned();
        Some(format!(
            "CREATE{} INDEX {txt}",
            if on_error == OE_None { "" } else { " UNIQUE" }
        ))
    } else {
        // An automatic index created by a PRIMARY KEY or UNIQUE constraint
        None
    };

    // Add an entry in sqlite_schema for this index
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();
    let (z_idx, z_tab) = (
        p.idx(ix).z_name.clone(),
        p.tab(p.idx(ix).p_table).z_name.clone(),
    );
    sqlite3_nested_parse(
        p,
        format!(
            "INSERT INTO {}.{LEGACY_SCHEMA_TABLE} VALUES('index',{},{},#{i_mem},{});",
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_idx)),
            pf_big_q(Some(&z_tab)),
            pf_big_q(z_stmt.as_deref()),
        ),
    );

    // Fill the index with data and reparse the schema. Code an OP_Expire to invalidate all
    // pre-compiled statements.
    if has_tbl_name {
        sqlite3_refill_index(p, ix, i_mem);
        sqlite3_change_cookie(p, i_db);
        let w = format!("name='{}' AND type='index'", pf_q(&z_idx));
        sqlite3_vdbe_add_parse_schema_op(p, i_db, Some(&w), 0);
        p.v.add_op2(OP_Expire, 0, 1);
    }

    p.v.jump_here(noop);
}

// ---------------------------------------------------------------------------------------------
// DROP TABLE / VIEW / INDEX
// ---------------------------------------------------------------------------------------------

/// `destroyRootPage(pParse, iTable, iDb)`: write code to erase the table with root-page iTable
/// from database iDb. Also write code to modify the sqlite_schema table and internal schema
/// if a root-page of another table is moved by the btree-layer whilst erasing iTable (this
/// can happen with an auto-vacuum database).
fn destroy_root_page(p: &mut Parse, i_table: i32, i_db: i32) {
    crate::select::sqlite3_get_vdbe(p);
    let r1 = crate::expr::sqlite3_get_temp_reg(p);
    if i_table < 2 {
        p.error_msg("corrupt schema");
    }
    p.v.add_op3(OP_Destroy, i_table, r1, i_db);
    sqlite3_may_abort(p);
    // OP_Destroy stores an in integer r1. If this integer is non-zero, then it is the root
    // page number of a table moved to location iTable. The following code modifies the
    // sqlite_schema table to reflect this.
    //
    // The "#NNN" in the SQL is a special constant that means whatever value is in register
    // NNN. See grammar rules associated with the TK_REGISTER token.
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();
    sqlite3_nested_parse(
        p,
        format!(
            "UPDATE {}.{LEGACY_SCHEMA_TABLE} SET rootpage={i_table} WHERE #{r1} AND rootpage=#{r1}",
            pf_big_q(Some(&z_db))
        ),
    );
    crate::expr::sqlite3_release_temp_reg(p, r1);
}

/// `destroyTable(pParse, pTab)`: write VDBE code to erase table pTab and all associated
/// indices from the database. The root pages are destroyed in descending order: with an
/// auto-vacuum database a root page moves only into a page that was freed before it.
fn destroy_table(p: &mut Parse, tab: TableId) {
    let i_tab = p.tab(tab).tnum;
    let mut i_destroyed: u32 = 0;
    loop {
        let mut i_largest: u32 = 0;
        if i_destroyed == 0 || i_tab < i_destroyed {
            i_largest = i_tab;
        }
        for &ix in &p.tab(tab).p_index {
            let i_idx = p.idx(ix).tnum;
            if (i_destroyed == 0 || i_idx < i_destroyed) && i_idx > i_largest {
                i_largest = i_idx;
            }
        }
        if i_largest == 0 {
            return;
        }
        let i_db = p.tab(tab).i_db;
        destroy_root_page(p, i_largest as i32, i_db);
        i_destroyed = i_largest;
    }
}

/// `sqlite3ClearStatTables(pParse, iDb, zType, zName)`: remove entries from the sqlite_statN
/// tables for the index or table named zName.
fn sqlite3_clear_stat_tables(p: &mut Parse, i_db: i32, z_type: &str, z_name: &str) {
    let z_db_name = p.db.a_db[i_db as usize].z_db_s_name.clone();
    for i in 1..=4 {
        let z_tab = format!("sqlite_stat{i}");
        if crate::schema::sqlite3_find_table(p, &z_tab, Some(&z_db_name)).nn() {
            sqlite3_nested_parse(
                p,
                format!(
                    "DELETE FROM {}.{z_tab} WHERE {z_type}={}",
                    pf_big_q(Some(&z_db_name)),
                    pf_big_q(Some(z_name))
                ),
            );
        }
    }
}

/// `sqlite3CodeDropTable(pParse, pTab, iDb, isView)`: generate code to drop a table.
pub fn sqlite3_code_drop_table(p: &mut Parse, tab: TableId, i_db: i32, _is_view: bool) {
    crate::select::sqlite3_get_vdbe(p);
    sqlite3_begin_write_operation(p, true, i_db);
    if p.tab(tab).is_virtual() {
        p.v.add_op0(OP_VBegin);
    }

    // Drop all triggers associated with the table being dropped. Code is generated to remove
    // entries from sqlite_schema and/or sqlite_temp_schema if required.
    for trig in crate::trigger::sqlite3_trigger_list(p, tab) {
        sqlite3_drop_trigger_ptr(p, trig);
    }

    // Remove any entries of the sqlite_sequence table associated with the table being dropped.
    // This is done before the table is dropped at the btree level, in case the
    // sqlite_sequence table needs to be moved as a result of the drop (can happen in
    // auto-vacuum mode).
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();
    let z_name = p.tab(tab).z_name.clone();
    if (p.tab(tab).tab_flags & TF_Autoincrement) != 0 {
        sqlite3_nested_parse(
            p,
            format!(
                "DELETE FROM {}.sqlite_sequence WHERE name={}",
                pf_big_q(Some(&z_db)),
                pf_big_q(Some(&z_name))
            ),
        );
    }

    // Drop all entries in the schema table that refer to the table. The program name loops
    // through the schema table and deletes every row that refers to a table of the same name
    // as the table being dropped. This is required to handle the case where the table being
    // dropped is the sqlite_sequence table.
    sqlite3_nested_parse(
        p,
        format!(
            "DELETE FROM {}.{LEGACY_SCHEMA_TABLE} WHERE tbl_name={} and type!='trigger'",
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_name))
        ),
    );
    if !_is_view && !p.tab(tab).is_virtual() {
        destroy_table(p, tab);
    }

    // Remove the table entry from SQLite's internal schema and modify the schema cookie.
    if p.tab(tab).is_virtual() {
        p.v.add_op4(OP_VDestroy, i_db, 0, 0, P4::dynamic(&z_name));
        sqlite3_may_abort(p);
    }
    p.v.add_op4(OP_DropTable, i_db, 0, 0, P4::dynamic(&z_name));
    sqlite3_change_cookie(p, i_db);
    // (sqliteViewResetAll: no code)
}

/// `tableMayNotBeDropped(db, pTab)`: return true if it is not allowed to drop the given table
fn table_may_not_be_dropped(p: &Parse, tab: TableId) -> bool {
    let t = p.tab(tab);
    let z = t.z_name.as_bytes();
    if z.len() >= 7 && z[..7].eq_ignore_ascii_case(b"sqlite_") {
        let rest = &z[7..];
        if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case(b"stat") {
            return false;
        }
        if rest.len() >= 10 && rest[..10].eq_ignore_ascii_case(b"parameters") {
            return false;
        }
        return true;
    }
    if (t.tab_flags & TF_Shadow) != 0 && sqlite3_read_only_shadow_tables(p) {
        return true;
    }
    if (t.tab_flags & TF_Eponymous) != 0 {
        return true;
    }
    false
}

/// `sqlite3DropTable(pParse, pName, isView, noErr)`: this routine is called to do the work of
/// a DROP TABLE statement. pName is the name of the table to be dropped.
pub fn sqlite3_drop_table(p: &mut Parse, name: SrcListId, is_view: bool, no_err: bool) {
    if name.is_null() {
        return;
    }
    crate::schema::sqlite3_read_schema(p);
    let saved_suppress = p.suppress_err;
    if no_err {
        p.suppress_err = true;
    }
    let tab = crate::schema::sqlite3_locate_table_item(
        p,
        if is_view { LOCATE_VIEW as u32 } else { 0 },
        name,
        0,
    );
    p.suppress_err = saved_suppress;
    if p.unsupported.is_some() {
        return;
    }
    if tab.is_null() {
        if no_err {
            let zdb = p.src(name).a[0].z_database.clone();
            sqlite3_code_verify_named_schema(p, zdb.as_deref());
            sqlite3_force_not_read_only(p);
        }
        return;
    }
    let i_db = p.tab(tab).i_db;

    // If pTab is a virtual table, call ViewGetColumnNames() to ensure it is initialized.
    if p.tab(tab).is_virtual() && crate::select::sqlite3_view_get_column_names(p, tab) != 0 {
        return;
    }
    // (sqlite3AuthCheck: the generator is not used with an authorizer)
    if table_may_not_be_dropped(p, tab) {
        let n = p.tab(tab).z_name.clone();
        p.error_msg(format!("table {n} may not be dropped"));
        return;
    }

    // Ensure DROP TABLE is not used on a view, and DROP VIEW is not used on a table.
    if is_view && !p.tab(tab).is_view() {
        let n = p.tab(tab).z_name.clone();
        p.error_msg(format!("use DROP TABLE to delete table {n}"));
        return;
    }
    if !is_view && p.tab(tab).is_view() {
        let n = p.tab(tab).z_name.clone();
        p.error_msg(format!("use DROP VIEW to delete view {n}"));
        return;
    }

    // Generate code to remove the table from the schema table on disk.
    crate::select::sqlite3_get_vdbe(p);
    sqlite3_begin_write_operation(p, true, i_db);
    if !is_view {
        let z_name = p.tab(tab).z_name.clone();
        sqlite3_clear_stat_tables(p, i_db, "tbl", &z_name);
        crate::fkey::sqlite3_fk_drop_table(p, name, tab);
    }
    sqlite3_code_drop_table(p, tab, i_db, is_view);
}

/// `sqlite3DropIndex(pParse, pName, ifExists)`: this routine will drop an existing named
/// index. This routine implements the DROP INDEX statement.
pub fn sqlite3_drop_index(p: &mut Parse, name: SrcListId, if_exists: bool) {
    if name.is_null() {
        return;
    }
    crate::schema::sqlite3_read_schema(p);
    let (z_name, z_db) = {
        let it = &p.src(name).a[0];
        (it.z_name.clone().unwrap_or_default(), it.z_database.clone())
    };
    let ix = crate::build_tab::sqlite3_find_index(p, &z_name, z_db.as_deref());
    if ix.is_null() {
        if !if_exists {
            let n = crate::wherecode::src_item_name(p, name, 0, false);
            p.error_msg(format!("no such index: {n}"));
        } else {
            sqlite3_code_verify_named_schema(p, z_db.as_deref());
            sqlite3_force_not_read_only(p);
        }
        p.check_schema = true;
        return;
    }
    if p.idx(ix).idx_type != SQLITE_IDXTYPE_APPDEF as u8 {
        p.error_msg("index associated with UNIQUE or PRIMARY KEY constraint cannot be dropped");
        return;
    }
    let i_db = p.idx(ix).i_db;
    // (sqlite3AuthCheck: the generator is not used with an authorizer)

    // Generate code to remove the index and from the schema table
    crate::select::sqlite3_get_vdbe(p);
    sqlite3_begin_write_operation(p, true, i_db);
    let z_db_name = p.db.a_db[i_db as usize].z_db_s_name.clone();
    let z_idx = p.idx(ix).z_name.clone();
    sqlite3_nested_parse(
        p,
        format!(
            "DELETE FROM {}.{LEGACY_SCHEMA_TABLE} WHERE name={} AND type='index'",
            pf_big_q(Some(&z_db_name)),
            pf_big_q(Some(&z_idx))
        ),
    );
    sqlite3_clear_stat_tables(p, i_db, "idx", &z_idx);
    sqlite3_change_cookie(p, i_db);
    let tnum = p.idx(ix).tnum as i32;
    destroy_root_page(p, tnum, i_db);
    p.v.add_op4(OP_DropIndex, i_db, 0, 0, P4::dynamic(&z_idx));
}

// ---------------------------------------------------------------------------------------------
// DROP TRIGGER (trigger.c)
// ---------------------------------------------------------------------------------------------

/// `sqlite3DropTrigger(pParse, pName, noErr)`: this function is called to drop a trigger from
/// the database schema.
pub fn sqlite3_drop_trigger(p: &mut Parse, name: SrcListId, no_err: bool) {
    if name.is_null() {
        return;
    }
    crate::schema::sqlite3_read_schema(p);
    let (z_name, z_db) = {
        let it = &p.src(name).a[0];
        (it.z_name.clone().unwrap_or_default(), it.z_database.clone())
    };
    let mut trig: Option<TriggerId> = None;
    for i in 0..p.db.a_db.len() {
        let j = if i < 2 { i ^ 1 } else { i };
        if let Some(zd) = &z_db
            && !sqlite3_db_is_named(p, j, zd)
        {
            continue;
        }
        if let Some(t) = p.a_schema.get(j).and_then(|s| s.trig_hash.find(&z_name)) {
            trig = Some(t);
            break;
        }
    }
    let Some(trig) = trig else {
        if !no_err {
            let n = crate::wherecode::src_item_name(p, name, 0, false);
            p.error_msg(format!("no such trigger: {n}"));
        } else {
            sqlite3_code_verify_named_schema(p, z_db.as_deref());
        }
        p.check_schema = true;
        return;
    };
    sqlite3_drop_trigger_ptr(p, trig);
}

/// `sqlite3DbIsNamed(db, iDb, zName)`: true if database `i_db` is called `z_name` ("main" is
/// always a name of database 0).
pub fn sqlite3_db_is_named(p: &Parse, i_db: usize, z_name: &str) -> bool {
    p.db.a_db[i_db].z_db_s_name.eq_ignore_ascii_case(z_name)
        || (i_db == 0 && z_name.eq_ignore_ascii_case("main"))
}

/// `sqlite3DropTriggerPtr(pParse, pTrigger)`: drop a trigger given a pointer to that trigger.
pub fn sqlite3_drop_trigger_ptr(p: &mut Parse, trig: TriggerId) {
    let (i_db, z_name) = {
        let t = p.trig(trig);
        (t.i_schema, t.z_name.clone().unwrap_or_default())
    };
    // (tableOfTrigger + sqlite3AuthCheck: the generator is not used with an authorizer)

    // Generate code to destroy the database record of the trigger.
    crate::select::sqlite3_get_vdbe(p);
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();
    sqlite3_nested_parse(
        p,
        format!(
            "DELETE FROM {}.{LEGACY_SCHEMA_TABLE} WHERE name={} AND type='trigger'",
            pf_big_q(Some(&z_db)),
            pf_big_q(Some(&z_name))
        ),
    );
    sqlite3_change_cookie(p, i_db);
    p.v.add_op4(OP_DropTrigger, i_db, 0, 0, P4::dynamic(&z_name));
}

// ---------------------------------------------------------------------------------------------
// REINDEX
// ---------------------------------------------------------------------------------------------

/// `collationMatch(zColl, pIndex)`: check to see if pIndex uses the collating sequence pColl.
fn collation_match(z_coll: &str, idx: &Index) -> bool {
    idx.az_coll[..idx.n_column as usize]
        .iter()
        .any(|z| z.eq_ignore_ascii_case(z_coll))
}

/// `sqlite3FindCollSeq(db, ENC(db), z, 0)!=0`: a collating sequence of that name exists.
fn coll_seq_exists(p: &Parse, z: &str) -> bool {
    ["BINARY", "NOCASE", "RTRIM"]
        .iter()
        .any(|b| b.eq_ignore_ascii_case(z))
        || p.db.host.find_collation(z).is_some()
}

/// `sqlite3Reindex(pParse, pName1, pName2)`: generate code for the REINDEX command.
///
///        REINDEX                             -- 1
///        REINDEX  <collation>                -- 2
///        REINDEX  ?<database>.?<tablename>   -- 3
///        REINDEX  ?<database>.?<indexname>   -- 4
///        REINDEX  EXPRESSIONS                -- 5
///
/// Form 1 causes all indices in all attached databases to be rebuilt. Form 2 rebuilds all
/// indices in all databases that use the named collating function. Forms 3 and 4 rebuild the
/// named index or all indices associated with the named table, or if no database prefix is
/// present, all tables and indices of the given name in all attached databases. Form 5
/// rebuilds all expression indexes in all attached databases.
pub fn sqlite3_reindex(p: &mut Parse, name1: Option<&Token>, name2: Option<&Token>) {
    crate::schema::sqlite3_read_schema(p);
    let mut z: Option<String> = None;
    let mut z_db: Option<String> = None;
    let mut i_re_db: i32 = -1;
    let mut b_match = false;
    let mut b_all = false;
    let mut is_expr_idx = false;
    let mut z_coll: Option<String> = None;
    let mut re_tab = TableId::NULL;
    let mut re_index = IndexId::NULL;
    match (name1, name2) {
        (None, _) => {
            // Form 1: Reindex all indices in all databases
            b_match = true;
            b_all = true;
        }
        (Some(n1), n2) if n2.is_none_or(|t| t.z == Token::NO_Z) => {
            // Form 2: Reindex all indices that use the named collating sequence
            z = sqlite3_name_from_token(p, Some(n1));
            if z.is_none() {
                return;
            }
        }
        (Some(n1), n2) => {
            let n2 = n2.copied().unwrap_or(Token::EMPTY);
            let n2 = &n2;
            // Form 3 or 4: reindex a table or index
            let mut obj = Token::EMPTY;
            i_re_db = crate::build_tab::sqlite3_two_part_name(p, n1, n2, &mut obj);
            if i_re_db < 0 {
                return;
            }
            z = sqlite3_name_from_token(p, Some(&obj));
            if z.is_none() {
                return;
            }
            z_db = Some(p.db.a_db[i_re_db as usize].z_db_s_name.clone());
        }
    }
    if !b_all {
        let zz = z.clone().unwrap_or_default();
        if z_db.is_none() && zz.eq_ignore_ascii_case("expressions") {
            is_expr_idx = true;
            b_match = true;
        }
        if z_db.is_none() && coll_seq_exists(p, &zz) {
            z_coll = Some(zz.clone());
            b_match = true;
        }
        if z_coll.is_none() {
            re_tab = crate::schema::sqlite3_find_table(p, &zz, z_db.as_deref());
            if re_tab.nn() {
                b_match = true;
            }
        }
        if z_coll.is_none() {
            re_index = crate::build_tab::sqlite3_find_index(p, &zz, z_db.as_deref());
            if re_index.nn() {
                b_match = true;
            }
        }
    }
    if b_match {
        for i_db in 0..p.db.a_db.len() {
            if i_re_db >= 0 && i_re_db != i_db as i32 {
                continue;
            }
            let tabs = p
                .a_schema
                .get(i_db)
                .map(|s| s.tbl_order.ids())
                .unwrap_or_default();
            for tab in tabs {
                if p.tab(tab).is_virtual() {
                    continue;
                }
                for ix in p.tab(tab).p_index.clone() {
                    if b_all
                        || tab == re_tab
                        || ix == re_index
                        || (is_expr_idx && p.idx(ix).b_has_expr)
                        || z_coll
                            .as_deref()
                            .is_some_and(|c| collation_match(c, p.idx(ix)))
                    {
                        sqlite3_begin_write_operation(p, false, i_db as i32);
                        sqlite3_refill_index(p, ix, -1);
                    }
                }
            }
        }
    } else {
        p.error_msg("unable to identify the object to be reindexed");
    }
}

// ---------------------------------------------------------------------------------------------
// vacuum.c, attach.c
// ---------------------------------------------------------------------------------------------

/// `sqlite3Vacuum(pParse, pNm, pInto)`: the VACUUM command is used to clean up the database,
/// collapse free space, etc. It is modelled after the VACUUM command in PostgreSQL. The
/// VACUUM command works as follows:
///
///   (1)  Create a new transient database file
///   (2)  Copy all content from the database being vacuumed into the new transient database
///        file
///   (3)  Copy content from the transient database back into the original database.
///
/// The only code generated here is OP_Vacuum.
pub fn sqlite3_vacuum(p: &mut Parse, nm: Option<&Token>, into: ExprId) {
    crate::select::sqlite3_get_vdbe(p);
    if p.n_err != 0 {
        return;
    }
    let mut i_db = 0;
    if let Some(nm) = nm {
        // Default behavior: Report an error if the argument to VACUUM is not recognized
        let mut n = Token::EMPTY;
        i_db = crate::build_tab::sqlite3_two_part_name(p, nm, nm, &mut n);
        if i_db < 0 {
            return;
        }
    }
    if i_db != 1 {
        let mut i_into_reg = 0;
        if into.nn()
            && crate::resolve::sqlite3_resolve_self_reference(
                p,
                TableId::NULL,
                0,
                into,
                ExprListId::NULL,
            ) == 0
        {
            p.n_mem += 1;
            i_into_reg = p.n_mem;
            crate::exprcode::sqlite3_expr_code(p, into, i_into_reg);
        }
        p.v.add_op2(OP_Vacuum, i_db, i_into_reg);
        let sharable = p.db.a_db[i_db as usize].sharable;
        p.v.uses_btree(i_db, sharable);
    }
}

/// `resolveAttachExpr(pName, pExpr)`: an SQL user-function registered to do the work of an
/// ATTACH or DETACH statement takes its arguments as expressions: an identifier is taken as a
/// string (the name of the database), anything else is resolved without a FROM clause.
fn resolve_attach_expr(p: &mut Parse, e: ExprId) -> i32 {
    if e.nn() {
        if p.e(e).op != TK_ID {
            let mark = p.a_nc.len();
            let nc = p.new_nc(NameContext::default());
            let rc = crate::resolve::sqlite3_resolve_expr_names(p, nc, e);
            p.a_nc.truncate(mark);
            return rc;
        } else {
            p.em(e).op = TK_STRING;
        }
    }
    SQLITE_OK
}

/// `codeAttach(pParse, type, pFunc, pAuthArg, pFilename, pDbname, pKey)`: this procedure
/// generates VDBE code for a single invocation of either the sqlite_detach() or
/// sqlite_attach() SQL user functions.
fn code_attach(
    p: &mut Parse,
    is_attach: bool,
    z_func: &str,
    n_arg: i16,
    filename: ExprId,
    dbname: ExprId,
    key: ExprId,
) {
    crate::schema::sqlite3_read_schema(p);
    if p.n_err != 0 {
        return;
    }
    if resolve_attach_expr(p, filename) != SQLITE_OK
        || resolve_attach_expr(p, dbname) != SQLITE_OK
        || resolve_attach_expr(p, key) != SQLITE_OK
    {
        return;
    }
    // (sqlite3AuthCheck(SQLITE_ATTACH / SQLITE_DETACH): the generator is not used with an
    // authorizer)
    crate::select::sqlite3_get_vdbe(p);
    let reg_args = crate::expr::sqlite3_get_temp_range(p, 4);
    crate::exprcode::sqlite3_expr_code(p, filename, reg_args);
    crate::exprcode::sqlite3_expr_code(p, dbname, reg_args + 1);
    crate::exprcode::sqlite3_expr_code(p, key, reg_args + 2);
    let func = std::sync::Arc::new(sqliters_vdbe::vdbeaux::FuncDef {
        n_arg,
        func_flags: SQLITE_UTF8 as u32,
        name: z_func.to_owned(),
        user_data: 0,
        x_finalize: false,
        x_value: false,
    });
    p.v.add_function_call(
        0,
        reg_args + 3 - n_arg as i32,
        reg_args + 3,
        n_arg as i32,
        func,
        0,
        0,
    );
    sqlite3_may_abort(p);
    // Code an OP_Expire. For an ATTACH statement, set P1 to true (expire this statement only).
    // For DETACH, set it to false (expire all existing statements).
    p.v.add_op1(OP_Expire, is_attach as i32);
}

/// `sqlite3Detach(pParse, pDbname)`: called by the parser to compile a DETACH statement.
///
///     DETACH pDbname
pub fn sqlite3_detach(p: &mut Parse, dbname: ExprId) {
    code_attach(
        p,
        false,
        "sqlite_detach",
        1,
        ExprId::NULL,
        ExprId::NULL,
        dbname,
    );
}

/// `sqlite3Attach(pParse, p, pDbname, pKey)`: called by the parser to compile an ATTACH
/// statement.
///
///     ATTACH p AS pDbname KEY pKey
pub fn sqlite3_attach(p: &mut Parse, filename: ExprId, dbname: ExprId, key: ExprId) {
    code_attach(p, true, "sqlite_attach", 3, filename, dbname, key);
}

#[allow(dead_code)]
fn _unused(p: &mut Parse) {
    let _ = sqlite3_dequote("");
    let _ = p;
}
