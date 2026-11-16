//! Port of the code-generating part of `analyze.c` (step V6): `openStatTable`,
//! `analyzeOneTable`, `analyzeDatabase`, `analyzeTable`, `sqlite3Analyze`. The reference build
//! has no SQLITE_ENABLE_STAT4 (`IsStat4` is 0): only `sqlite_stat1` is written.

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{FuncDef, P4};

use crate::build::*;
use crate::consts::*;
use crate::ddl::{pf_big_q, sqlite3_nested_parse};
use crate::sqliteint::*;

/// `statInitFuncdef`, `statPushFuncdef`, `statGetFuncdef` (nArg 4, 2+IsStat4, 1+IsStat4)
fn stat_funcdef(name: &str, n_arg: i16) -> Arc<FuncDef> {
    Arc::new(FuncDef {
        n_arg,
        func_flags: SQLITE_UTF8 as u32,
        name: name.to_owned(),
        user_data: 0,
        x_finalize: false,
        x_value: false,
    })
}

/// `openStatTable(pParse, iDb, iStatCur, zWhere, zWhereType)`: this routine generates code
/// that opens the sqlite_statN tables. The sqlite_stat1 table is always relevant. sqlite_stat2
/// is now obsolete. sqlite_stat3 and sqlite_stat4 are only opened when appropriate
/// compile-time options are provided.
///
/// If the sqlite_statN tables do not previously exist, it is created.
///
/// Argument zWhere may be a pointer to a buffer containing a table name, or it may be a NULL
/// pointer. If it is not NULL, then all entries in the sqlite_statN tables associated with the
/// named table are deleted. If zWhere==0, then code is generated to delete all stat table
/// entries.
fn open_stat_table(
    p: &mut Parse,
    i_db: i32,
    i_stat_cur: i32,
    z_where: Option<&str>,
    z_where_type: &str,
) {
    // aTable[]: { "sqlite_stat1", "tbl,idx,stat" }, { "sqlite_stat4", 0 }, { "sqlite_stat3", 0 }
    const A_TABLE: [(&str, Option<&str>); 3] = [
        ("sqlite_stat1", Some("tbl,idx,stat")),
        ("sqlite_stat4", None),
        ("sqlite_stat3", None),
    ];
    const N_TO_OPEN: usize = 1;
    crate::select::sqlite3_get_vdbe(p);
    let mut a_root = [0i32; 3];
    let mut a_create_tbl = [0u16; 3];
    let z_db = p.db.a_db[i_db as usize].z_db_s_name.clone();

    // Create new statistic tables if they do not exist, or clear them if they do already
    // exist.
    for (i, &(z_tab, z_cols)) in A_TABLE.iter().enumerate() {
        a_create_tbl[i] = 0;
        let stat = crate::schema::sqlite3_find_table(p, z_tab, Some(&z_db));
        if stat.is_null() {
            if i < N_TO_OPEN {
                // The sqlite_statN table does not exist. Create it. Note that a side-effect
                // of the CREATE TABLE statement is to leave the rootpage of the new table in
                // register pParse->regRoot. This is important because the OpenWrite opcode
                // below will be needing it.
                sqlite3_nested_parse(
                    p,
                    format!(
                        "CREATE TABLE {}.{z_tab}({})",
                        pf_big_q(Some(&z_db)),
                        z_cols.unwrap_or("")
                    ),
                );
                a_root[i] = p.cr_reg_root;
                a_create_tbl[i] = OPFLAG_P2ISREG as u16;
            }
        } else {
            // The table already exists. If zWhere is not NULL, delete all entries associated
            // with the table zWhere. If zWhere is NULL, delete the entire contents of the
            // table.
            a_root[i] = p.tab(stat).tnum as i32;
            if let Some(w) = z_where {
                sqlite3_nested_parse(
                    p,
                    format!(
                        "DELETE FROM {}.{z_tab} WHERE {z_where_type}={}",
                        pf_big_q(Some(&z_db)),
                        pf_big_q(Some(w))
                    ),
                );
            } else {
                // The sqlite_stat[134] table already exists. Delete all rows.
                p.v.add_op2(OP_Clear, a_root[i], i_db);
            }
        }
    }

    // Open the sqlite_stat[134] tables for writing.
    for i in 0..N_TO_OPEN {
        p.v.add_op4_int(OP_OpenWrite, i_stat_cur + i as i32, a_root[i], i_db, 3);
        p.v.change_p5(a_create_tbl[i]);
    }
}

/// `callStatGet(pParse, regStat, iParam, regOut)`: generate code to invoke the stat_get()
/// function (without STAT4 only STAT_GET_STAT1).
fn call_stat_get(p: &mut Parse, reg_stat: i32, reg_out: i32) {
    p.v.add_function_call(0, reg_stat, reg_out, 1, stat_funcdef("stat_get", 1), 0, 0);
    sqlite3_may_abort(p);
}

/// `sqlite3_strlike("sqlite\\_%", zName, '\\')==0`: the name starts with "sqlite_" (ASCII case
/// folded).
fn is_sqlite_name(z: &str) -> bool {
    z.len() >= 7 && z.as_bytes()[..7].eq_ignore_ascii_case(b"sqlite_")
}

/// `analyzeOneTable(pParse, pTab, pOnlyIdx, iStatCur, iMem, iTab)`: generate code to do an
/// analysis of all indices associated with a single table.
fn analyze_one_table(
    p: &mut Parse,
    tab: TableId,
    only_idx: IndexId,
    i_stat_cur: i32,
    mut i_mem: i32,
    mut i_tab: i32,
) {
    let mut need_table_cnt = true;
    let reg_new_rowid = i_mem;
    i_mem += 1;
    let reg_stat = i_mem;
    i_mem += 1;
    let reg_chng = i_mem;
    i_mem += 1;
    let reg_rowid = i_mem;
    i_mem += 1;
    let reg_temp = i_mem;
    i_mem += 1;
    let reg_temp2 = i_mem;
    i_mem += 1;
    let reg_tabname = i_mem;
    i_mem += 1;
    let reg_idxname = i_mem;
    i_mem += 1;
    let reg_stat1 = i_mem;
    i_mem += 1;
    let reg_prev = i_mem;
    let _ = reg_chng;

    crate::expr::sqlite3_touch_register(p, i_mem);
    crate::select::sqlite3_get_vdbe(p);
    if !p.tab(tab).is_ordinary() {
        // Do not gather statistics on views or virtual tables
        return;
    }
    if is_sqlite_name(&p.tab(tab).z_name) {
        // Do not gather statistics on system tables
        return;
    }
    let i_db = p.tab(tab).i_db;
    // (sqlite3AuthCheck(SQLITE_ANALYZE): the generator is not used with an authorizer)

    // Establish a read-lock on the table at the shared-cache level. Open a read-only cursor
    // on the table. Also allocate a cursor number to use for scanning indexes (iIdxCur). No
    // index cursor is opened at this point.
    let i_tab_cur = i_tab;
    i_tab += 1;
    let i_idx_cur = i_tab;
    i_tab += 1;
    p.n_tab = p.n_tab.max(i_tab);
    crate::schema::sqlite3_open_table(p, i_tab_cur, i_db, tab, OP_OpenRead);
    let tname = p.tab(tab).z_name.clone();
    p.v.load_string(reg_tabname, &tname);

    for ix in p.tab(tab).p_index.clone() {
        if only_idx.nn() && only_idx != ix {
            continue;
        }
        if p.idx(ix).p_part_idx_where.is_null() {
            need_table_cnt = false;
        }
        let (n_col, z_idx_name, n_col_test);
        if !p.tab(tab).has_rowid() && p.idx(ix).is_primary_key() {
            n_col = p.idx(ix).n_key_col as i32;
            z_idx_name = tname.clone();
            n_col_test = n_col - 1;
        } else {
            n_col = p.idx(ix).n_column as i32;
            z_idx_name = p.idx(ix).z_name.clone();
            n_col_test = if p.idx(ix).uniq_not_null {
                p.idx(ix).n_key_col as i32 - 1
            } else {
                n_col - 1
            };
        }

        // Populate the register containing the index name.
        p.v.load_string(reg_idxname, &z_idx_name);

        // Pseudo-code for loop that calls stat_push():
        //
        //   regChng = 0
        //   Rewind csr
        //   if eof(csr){
        //      stat_init() with count = 0;
        //      goto end_of_scan;
        //   }
        //   count()
        //   stat_init()
        //   goto chng_addr_0;
        //
        //  next_row:
        //   regChng = 0
        //   if( idx(0) != regPrev(0) ) goto chng_addr_0
        //   regChng = 1
        //   if( idx(1) != regPrev(1) ) goto chng_addr_1
        //   ...
        //   regChng = N
        //   goto endDistinctTest
        //
        //  chng_addr_0:
        //   regPrev(0) = idx(0)
        //  chng_addr_1:
        //   regPrev(1) = idx(1)
        //  ...
        //
        //  endDistinctTest:
        //   regRowid = idx(rowid)
        //   stat_push(P, regChng, regRowid)
        //   Next csr
        //   if !eof(csr) goto next_row;
        //
        //  end_of_scan:

        // Make sure there are enough memory cells allocated to accommodate the regPrev array
        // and a trailing rowid (the rowid slot is required when building a record to insert
        // into the sample column of the sqlite_stat4 table.
        crate::expr::sqlite3_touch_register(p, reg_prev + n_col_test);

        // Open a read-only cursor on the index being analyzed.
        let itnum = p.idx(ix).tnum as i32;
        p.v.add_op3(OP_OpenRead, i_idx_cur, itnum, i_db);
        crate::schema::sqlite3_vdbe_set_p4_key_info(p, ix);

        // Invoke the stat_init() function.
        let limit = p.db.n_analysis_limit;
        p.v.add_op2(OP_Integer, limit, reg_temp2);
        p.v.add_op2(OP_Integer, n_col, reg_stat + 1);
        let n_key_col = p.idx(ix).n_key_col as i32;
        p.v.add_op2(OP_Integer, n_key_col, reg_rowid);
        let stat4_off = p.db.optimization_disabled(SQLITE_Stat4) as i32;
        p.v.add_op3(OP_Count, i_idx_cur, reg_temp, stat4_off);
        p.v.add_function_call(
            0,
            reg_stat + 1,
            reg_stat,
            4,
            stat_funcdef("stat_init", 4),
            0,
            0,
        );
        sqlite3_may_abort(p);
        let mut addr_goto_end = p.v.add_op1(OP_Rewind, i_idx_cur);

        p.v.add_op2(OP_Integer, 0, reg_chng);
        let mut addr_next_row = p.v.current_addr();

        if n_col_test > 0 {
            let end_distinct_test = p.v.make_label();
            let mut a_goto_chng = Vec::with_capacity(n_col_test as usize);

            // next_row:
            //  regChng = 0
            //  if( idx(0) != regPrev(0) ) goto chng_addr_0
            //  regChng = 1
            //  if( idx(1) != regPrev(1) ) goto chng_addr_1
            //  ...
            //  regChng = N
            //  goto endDistinctTest
            p.v.add_op0(OP_Goto);
            addr_next_row = p.v.current_addr();
            if n_col_test == 1 && n_key_col == 1 && p.idx(ix).is_unique() {
                // For a single-column UNIQUE index, once we have found a non-NULL row, we
                // know that all the rest will be distinct, so skip subsequent distinctness
                // tests.
                p.v.add_op2(OP_NotNull, reg_prev, end_distinct_test);
            }
            for i in 0..n_col_test {
                let zc = p.idx(ix).az_coll[i as usize].clone();
                let coll = crate::callback::sqlite3_locate_coll_seq(p, &zc);
                p.v.add_op2(OP_Integer, i, reg_chng);
                p.v.add_op3(OP_Column, i_idx_cur, i, reg_temp);
                let a = p.v.add_op4(
                    OP_Ne,
                    reg_temp,
                    0,
                    reg_prev + i,
                    coll.map_or(P4::NotUsed, P4::CollSeq),
                );
                a_goto_chng.push(a);
                p.v.change_p5(SQLITE_NULLEQ as u16);
            }
            p.v.add_op2(OP_Integer, n_col_test, reg_chng);
            p.v.goto(end_distinct_test);

            // chng_addr_0:
            //  regPrev(0) = idx(0)
            // chng_addr_1:
            //  regPrev(1) = idx(1)
            // ...
            p.v.jump_here(addr_next_row - 1);
            for i in 0..n_col_test {
                p.v.jump_here(a_goto_chng[i as usize]);
                p.v.add_op3(OP_Column, i_idx_cur, i, reg_prev + i);
            }
            p.v.resolve_label(end_distinct_test);
        }

        // chng_addr_N:
        //  regRowid = idx(rowid)            // STAT4 only
        //  stat_push(P, regChng, regRowid)  // 3rd parameter STAT4 only
        //  Next csr
        //  if !eof(csr) goto next_row;
        p.v.add_function_call(1, reg_stat, reg_temp, 2, stat_funcdef("stat_push", 2), 0, 0);
        sqlite3_may_abort(p);
        if p.db.n_analysis_limit != 0 {
            let j1 = p.v.add_op1(OP_IsNull, reg_temp);
            let j2 = p.v.add_op1(OP_If, reg_temp);
            let j3 = p.v.add_op4_int(OP_SeekGT, i_idx_cur, 0, reg_prev, 1);
            p.v.jump_here(j1);
            p.v.add_op2(OP_Next, i_idx_cur, addr_next_row);
            p.v.jump_here(j2);
            p.v.jump_here(j3);
        } else {
            p.v.add_op2(OP_Next, i_idx_cur, addr_next_row);
        }

        // Add the entry to the stat1 table.
        if p.idx(ix).p_part_idx_where.nn() {
            // Partial indexes might get a zero-entry in sqlite_stat1. But an empty table
            // or index gets no entry.
            p.v.jump_here(addr_goto_end);
            addr_goto_end = 0;
        }
        call_stat_get(p, reg_stat, reg_stat1);
        p.v.add_op4(OP_MakeRecord, reg_tabname, 3, reg_temp, P4::Static("BBB"));
        p.v.add_op2(OP_NewRowid, i_stat_cur, reg_new_rowid);
        p.v.add_op3(OP_Insert, i_stat_cur, reg_temp, reg_new_rowid);
        p.v.change_p5(OPFLAG_APPEND as u16);

        // End of analysis
        if addr_goto_end != 0 {
            p.v.jump_here(addr_goto_end);
        }
    }

    // Create a single sqlite_stat1 entry containing NULL as the index name and the row count
    // as the content.
    if only_idx.is_null() && need_table_cnt {
        p.v.add_op2(OP_Count, i_tab_cur, reg_stat1);
        let j_zero_rows = p.v.add_op1(OP_IfNot, reg_stat1);
        p.v.add_op2(OP_Null, 0, reg_idxname);
        p.v.add_op4(OP_MakeRecord, reg_tabname, 3, reg_temp, P4::Static("BBB"));
        p.v.add_op2(OP_NewRowid, i_stat_cur, reg_new_rowid);
        p.v.add_op3(OP_Insert, i_stat_cur, reg_temp, reg_new_rowid);
        p.v.change_p5(OPFLAG_APPEND as u16);
        p.v.jump_here(j_zero_rows);
    }
}

/// `loadAnalysis(pParse, iDb)`: generate code that will cause the most recent index analysis
/// to be loaded into internal hash tables where is can be used.
fn load_analysis(p: &mut Parse, i_db: i32) {
    crate::select::sqlite3_get_vdbe(p);
    p.v.add_op1(OP_LoadAnalysis, i_db);
}

/// `analyzeDatabase(pParse, iDb)`: generate code that will do an analysis of an entire
/// database
fn analyze_database(p: &mut Parse, i_db: i32) {
    sqlite3_begin_write_operation(p, false, i_db);
    let i_stat_cur = p.n_tab;
    p.n_tab += 3;
    open_stat_table(p, i_db, i_stat_cur, None, "");
    let i_mem = p.n_mem + 1;
    let i_tab = p.n_tab;
    let tabs = p
        .a_schema
        .get(i_db as usize)
        .map(|s| s.tbl_order.ids())
        .unwrap_or_default();
    for tab in tabs {
        analyze_one_table(p, tab, IndexId::NULL, i_stat_cur, i_mem, i_tab);
    }
    load_analysis(p, i_db);
}

/// `analyzeTable(pParse, pTab, pOnlyIdx)`: generate code that will do an analysis of a single
/// table in a database. If pOnlyIdx is not NULL then it is a single index in pTab that should
/// be analyzed.
fn analyze_table(p: &mut Parse, tab: TableId, only_idx: IndexId) {
    let i_db = p.tab(tab).i_db;
    sqlite3_begin_write_operation(p, false, i_db);
    let i_stat_cur = p.n_tab;
    p.n_tab += 3;
    if only_idx.nn() {
        let n = p.idx(only_idx).z_name.clone();
        open_stat_table(p, i_db, i_stat_cur, Some(&n), "idx");
    } else {
        let n = p.tab(tab).z_name.clone();
        open_stat_table(p, i_db, i_stat_cur, Some(&n), "tbl");
    }
    let (i_mem, i_tab) = (p.n_mem + 1, p.n_tab);
    analyze_one_table(p, tab, only_idx, i_stat_cur, i_mem, i_tab);
    load_analysis(p, i_db);
}

/// `sqlite3FindDb(db, pName)`: the index of the database named by the token, -1 if none
/// ("main" always names database 0).
pub fn sqlite3_find_db(p: &Parse, name: &Token) -> i32 {
    let z = crate::util::sqlite3_dequote(p.tok_str(name));
    for i in (0..p.db.a_db.len()).rev() {
        if p.db.a_db[i].z_db_s_name.eq_ignore_ascii_case(&z) {
            return i as i32;
        }
        if i == 0 && z.eq_ignore_ascii_case("main") {
            return 0;
        }
    }
    -1
}

/// `sqlite3Analyze(pParse, pName1, pName2)`: generate code for the ANALYZE command. The
/// parser calls this routine when it recognizes an ANALYZE command.
///
///        ANALYZE                            -- 1
///        ANALYZE  <database>                -- 2
///        ANALYZE  ?<database>.?<tablename>  -- 3
///
/// Form 1 causes all indices in all attached databases to be analyzed. Form 2 analyzes all
/// indices the single database named. Form 3 analyzes all indices associated with the named
/// table.
pub fn sqlite3_analyze(p: &mut Parse, name1: Option<&Token>, name2: Option<&Token>) {
    // Read the database schema. If an error occurs, leave an error message and code in
    // pParse and return NULL.
    crate::schema::sqlite3_read_schema(p);
    match (name1, name2) {
        (None, _) => {
            // Form 1: Analyze everything
            for i in 0..p.db.a_db.len() {
                if i == 1 {
                    continue; // Do not analyze the TEMP database
                }
                analyze_database(p, i as i32);
            }
        }
        (Some(n1), Some(n2)) => {
            let i_db = if n2.n == 0 {
                sqlite3_find_db(p, n1)
            } else {
                -1
            };
            if n2.n == 0 && i_db >= 0 {
                // Analyze the schema named as the argument
                analyze_database(p, i_db);
            } else {
                // Form 3: Analyze the table or index named as an argument
                let mut table_name = Token::EMPTY;
                let i_db = crate::build_tab::sqlite3_two_part_name(p, n1, n2, &mut table_name);
                if i_db >= 0 {
                    let z_db = if n2.n != 0 {
                        Some(p.db.a_db[i_db as usize].z_db_s_name.clone())
                    } else {
                        None
                    };
                    if let Some(z) = sqlite3_name_from_token(p, Some(&table_name)) {
                        let ix = crate::build_tab::sqlite3_find_index(p, &z, z_db.as_deref());
                        if ix.nn() {
                            let t = p.idx(ix).p_table;
                            analyze_table(p, t, ix);
                        } else {
                            let t = crate::schema::sqlite3_locate_table(p, 0, &z, z_db.as_deref());
                            if t.nn() {
                                analyze_table(p, t, IndexId::NULL);
                            }
                        }
                    }
                }
            }
        }
        (Some(_), None) => {}
    }
    if p.unsupported.is_none() {
        crate::select::sqlite3_get_vdbe(p);
        p.v.add_op0(OP_Expire);
    }
}
