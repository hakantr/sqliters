//! Phase 2 — Step 2-A (`Adım 2-A`) Integration & C SQLite 3.54.1 Differential Suite:
//! - **Group 46**: `ExecutionServices` & `BtreeAccess` trait boundary, standalone `VdbeVm` execution
//!   without `Connection`, disjoint borrow composition (`&mut Btree` + `ExecutionContext`), custom
//!   scalar functions, custom collations, progress callbacks, and interrupt propagation across both
//!   compiled `VdbeVm` and relational interpreter paths.
//! - **Group 64**: Structural `QualifiedName` & `ColumnRef` AST nodes, `ATTACH`/`DETACH` parsing and
//!   controlled runtime diagnostics, quoted identifiers containing `.`, 1/2/3-part column & wildcard
//!   resolution, `IN [schema.]table`, `sqlite_schema.sql` normalization parity, and DML alias vs
//!   `RETURNING` scope/wildcard rules differentially verified against C SQLite 3.54.1.
//! - **Group 65**: `RuntimeValue { value: Value, subtype: u8 }` ephemeral subtype contract, `SUBTYPE(X)`,
//!   low-8-bit `0xff` truncation, `FunctionFlags::SUBTYPE` argument stripping,
//!   `FunctionFlags::RESULT_SUBTYPE` strict enforcement (`SQLITE_STRICT_SUBTYPE`), disk record codec
//!   non-persistence, and subquery/CTE/compound boundaries differentially verified against C SQLite 3.54.1.
#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};

use sqliters::btree::Btree;
use sqliters::syntax::{
    ColumnRef, Expr, QualifiedName, ResultColumn, SelectCore, Stmt, TableOrSubquery, parse_one,
};
use sqliters::types::{
    DatabaseId, ErrorCode, ExecutionServices, FunctionFlags, MAX_DB_SLOTS, Result, RuntimeValue,
    SQLITE_MAX_ATTACHED, SqliteError, Value, decode_record, encode_record,
    enforce_result_subtype_contract, strip_unsubscribed_subtypes,
};
use sqliters::vdbe::{
    BtreeAccess, Instruction, Opcode, P4, VdbeProgram, VdbeVm, VmRuntimeContext, VmStep,
};
use sqliters::{Connection, StepResult};

// ============================================================================
// Out-of-Tree C SQLite 3.54.1 Reference Helper (`target/sqlite-ref/c_p2a_helper`)
// ============================================================================

const C_P2A_HELPER_SOURCE: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sqlite3.h"

static void c_test_set_subtype(
    sqlite3_context *ctx,
    int argc,
    sqlite3_value **argv
) {
    (void)argc;
    sqlite3_result_value(ctx, argv[0]);
    unsigned int sub = (unsigned int)sqlite3_value_int64(argv[1]);
    sqlite3_result_subtype(ctx, sub);
}

static void c_test_plain_arg_subtype(
    sqlite3_context *ctx,
    int argc,
    sqlite3_value **argv
) {
    (void)argc;
    sqlite3_result_int(ctx, (int)sqlite3_value_subtype(argv[0]));
}

static int c_rev_collation(void *unused, int n1, const void *p1, int n2, const void *p2) {
    (void)unused;
    int min_n = n1 < n2 ? n1 : n2;
    int cmp = memcmp(p1, p2, (size_t)min_n);
    if (cmp == 0) {
        cmp = (n1 < n2) ? -1 : (n1 > n2 ? 1 : 0);
    }
    return -cmp;
}

static int print_row_cb(void *unused, int argc, char **argv, char **col_names) {
    (void)unused;
    (void)col_names;
    for (int i = 0; i < argc; i++) {
        if (i > 0) fputc('|', stdout);
        fputs(argv[i] ? argv[i] : "NULL", stdout);
    }
    fputc('\n', stdout);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: c_p2a_helper <db_path> <sql>\n");
        return 2;
    }
    sqlite3 *db = NULL;
    int rc = sqlite3_open_v2(
        argv[1],
        &db,
        SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_URI,
        NULL
    );
    if (rc != SQLITE_OK) {
        fprintf(stderr, "OPEN_ERR rc=%d msg=%s\n", rc, db ? sqlite3_errmsg(db) : "oom");
        if (db) sqlite3_close(db);
        return 2;
    }

    sqlite3_create_function_v2(
        db,
        "test_set_subtype",
        2,
        SQLITE_UTF8 | SQLITE_DETERMINISTIC | SQLITE_SUBTYPE | SQLITE_RESULT_SUBTYPE,
        NULL,
        c_test_set_subtype,
        NULL,
        NULL,
        NULL
    );
    sqlite3_create_function_v2(
        db,
        "test_bad_subtype",
        2,
        SQLITE_UTF8 | SQLITE_DETERMINISTIC | SQLITE_SUBTYPE,
        NULL,
        c_test_set_subtype,
        NULL,
        NULL,
        NULL
    );
    sqlite3_create_function_v2(
        db,
        "test_plain_arg_subtype",
        1,
        SQLITE_UTF8 | SQLITE_DETERMINISTIC | SQLITE_INNOCUOUS,
        NULL,
        c_test_plain_arg_subtype,
        NULL,
        NULL,
        NULL
    );
    sqlite3_create_collation(db, "rev", SQLITE_UTF8, NULL, c_rev_collation);

    char *errmsg = NULL;
    rc = sqlite3_exec(db, argv[2], print_row_cb, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(
            stderr,
            "EXEC_ERR rc=%d msg=%s\n",
            rc,
            errmsg ? errmsg : sqlite3_errmsg(db)
        );
        sqlite3_free(errmsg);
        sqlite3_close(db);
        return 3;
    }
    sqlite3_close(db);
    return 0;
}
"#;

fn c_p2a_helper() -> &'static Path {
    static HELPER_BIN: OnceLock<PathBuf> = OnceLock::new();
    HELPER_BIN.get_or_init(|| {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sqlite_src_dir = workspace_root.join("sqlite");
        let ref_root = workspace_root.join("target").join("sqlite-ref");
        let amalg_dir = ref_root.join("amalg");
        fs::create_dir_all(&amalg_dir).expect("create target/sqlite-ref/amalg");

        let sqlite3_c = amalg_dir.join("sqlite3.c");
        let sqlite3_h = amalg_dir.join("sqlite3.h");
        if !sqlite3_c.exists() || !sqlite3_h.exists() {
            let make_out = Command::new("make")
                .arg("-C")
                .arg(&amalg_dir)
                .arg("-f")
                .arg(sqlite_src_dir.join("Makefile.linux-generic"))
                .arg(format!("TOP={}", sqlite_src_dir.display()))
                .arg("sqlite3.c")
                .output()
                .expect("invoke make for out-of-tree sqlite3.c");
            assert!(
                make_out.status.success(),
                "failed building sqlite3.c:\n{}",
                String::from_utf8_lossy(&make_out.stderr)
            );
        }

        let strict_o = ref_root.join("sqlite3_strict_subtype.o");
        if !strict_o.exists() {
            let tmp_o = ref_root.join(format!("sqlite3_strict.{}.o.tmp", std::process::id()));
            let cc_o = Command::new("cc")
                .args(["-O2", "-pthread", "-DSQLITE_STRICT_SUBTYPE=1", "-c"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&sqlite3_c)
                .arg("-o")
                .arg(&tmp_o)
                .output()
                .expect("compile sqlite3_strict_subtype.o");
            assert!(
                cc_o.status.success(),
                "failed compiling sqlite3_strict_subtype.o:\n{}",
                String::from_utf8_lossy(&cc_o.stderr)
            );
            let _ = fs::rename(&tmp_o, &strict_o);
        }

        let helper_c = ref_root.join("c_p2a_helper.c");
        let helper_bin = ref_root.join("c_p2a_helper");
        let need_rebuild = !helper_bin.exists()
            || fs::read_to_string(&helper_c)
                .map(|cur| cur != C_P2A_HELPER_SOURCE)
                .unwrap_or(true);
        if need_rebuild {
            fs::write(&helper_c, C_P2A_HELPER_SOURCE).expect("write c_p2a_helper.c");
            let tmp_bin = ref_root.join(format!("c_p2a_helper.{}.tmp", std::process::id()));
            let cc_bin = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&helper_c)
                .arg(&strict_o)
                .args(["-lm", "-ldl", "-o"])
                .arg(&tmp_bin)
                .output()
                .expect("link c_p2a_helper");
            assert!(
                cc_bin.status.success(),
                "failed linking c_p2a_helper:\n{}",
                String::from_utf8_lossy(&cc_bin.stderr)
            );
            let _ = fs::rename(&tmp_bin, &helper_bin);
        }
        helper_bin
    })
}

fn run_c_sql(sql: &str) -> std::result::Result<Vec<String>, String> {
    let out = Command::new(c_p2a_helper())
        .arg(":memory:")
        .arg(sql)
        .output()
        .expect("spawn c_p2a_helper");
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines = stdout.lines().map(str::to_owned).collect::<Vec<String>>();
        Ok(lines)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(stderr)
    }
}

fn format_rust_val_like_c(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format!("{r}"),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn run_rust_script(conn: &Connection, sql: &str) -> std::result::Result<Vec<String>, SqliteError> {
    let mut cur = sql.to_owned();
    let mut out_lines = Vec::new();
    while !cur.trim().is_empty() {
        let mut stmt = conn.prepare(&cur)?;
        let next_tail = stmt.tail().to_owned();
        while let StepResult::Row(row) = stmt.step()? {
            let line = row
                .values()
                .iter()
                .map(format_rust_val_like_c)
                .collect::<Vec<_>>()
                .join("|");
            out_lines.push(line);
        }
        cur = next_tail;
    }
    Ok(out_lines)
}

// ============================================================================
// Group 46: `ExecutionServices` & `BtreeAccess` Boundary Tests
// ============================================================================

struct StandaloneMockServices {
    interrupted: bool,
    interrupt_after_ticks: Option<usize>,
    ticks: usize,
    scalar_calls: Vec<String>,
    collation_calls: usize,
}

impl StandaloneMockServices {
    fn new() -> Self {
        Self {
            interrupted: false,
            interrupt_after_ticks: None,
            ticks: 0,
            scalar_calls: Vec::new(),
            collation_calls: 0,
        }
    }
}

impl ExecutionServices for StandaloneMockServices {
    fn check_interrupted(&mut self) -> Result<()> {
        if self.interrupted {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        Ok(())
    }

    fn tick_progress(&mut self) -> Result<()> {
        self.ticks += 1;
        if let Some(limit) = self.interrupt_after_ticks
            && self.ticks >= limit
        {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        Ok(())
    }

    fn call_scalar(&mut self, name: &str, args: &[RuntimeValue]) -> Result<RuntimeValue> {
        self.scalar_calls.push(name.to_owned());
        match name {
            "mock_tag" => {
                let flags = FunctionFlags::DETERMINISTIC | FunctionFlags::RESULT_SUBTYPE;
                let mut scratch = Vec::new();
                let filtered = strip_unsubscribed_subtypes(flags, args, &mut scratch);
                let out = RuntimeValue::with_subtype(filtered[0].value.clone(), 0x14A);
                enforce_result_subtype_contract(name, flags, true, out)
            }
            "mock_inspect_sub" => {
                let flags = FunctionFlags::DETERMINISTIC | FunctionFlags::SUBTYPE;
                let mut scratch = Vec::new();
                let filtered = strip_unsubscribed_subtypes(flags, args, &mut scratch);
                let out = RuntimeValue::plain(Value::Integer(i64::from(filtered[0].subtype)));
                enforce_result_subtype_contract(name, flags, true, out)
            }
            "mock_strip_sub" => {
                let flags = FunctionFlags::DETERMINISTIC;
                let mut scratch = Vec::new();
                let filtered = strip_unsubscribed_subtypes(flags, args, &mut scratch);
                let out = RuntimeValue::plain(Value::Integer(i64::from(filtered[0].subtype)));
                enforce_result_subtype_contract(name, flags, true, out)
            }
            "mock_illegal_sub" => {
                let flags = FunctionFlags::DETERMINISTIC;
                let out = RuntimeValue::with_subtype(Value::Text("illegal".into()), 77);
                enforce_result_subtype_contract(name, flags, true, out)
            }
            _ => Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such function: {name}"),
            )),
        }
    }

    fn compare_collated(
        &mut self,
        collation_name: &str,
        left: &str,
        right: &str,
    ) -> Result<Ordering> {
        self.collation_calls += 1;
        if collation_name.eq_ignore_ascii_case("REV_ASCII") {
            Ok(right.cmp(left))
        } else {
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {collation_name}"),
            ))
        }
    }
}

#[test]
fn group46_standalone_execution_services_and_btree_access_without_connection() {
    assert_eq!(DatabaseId::MAIN, DatabaseId(0));
    assert_eq!(DatabaseId::TEMP, DatabaseId(1));
    assert_eq!(SQLITE_MAX_ATTACHED, 10);
    assert_eq!(MAX_DB_SLOTS, 12);

    // 1. Standalone `BtreeAccess` routing on single `Btree`
    let mut bt = Btree::open_in_memory().unwrap();
    assert!(bt.btree_ref(DatabaseId::MAIN).is_ok());
    assert!(bt.btree_mut(DatabaseId::MAIN).is_ok());
    for slot in 1..MAX_DB_SLOTS {
        assert!(bt.btree_ref(DatabaseId(slot)).is_err());
        assert!(bt.btree_mut(DatabaseId(slot)).is_err());
    }

    // Seed a real table directly in `Btree` without `Connection`
    bt.begin_write().unwrap();
    let root = bt.create_table().unwrap();
    bt.insert_table(root, 1, &encode_record(&[Value::Text("alpha".into())]))
        .unwrap();
    bt.insert_table(root, 2, &encode_record(&[Value::Text("zeta".into())]))
        .unwrap();
    bt.commit().unwrap();

    // 2. Build a `VdbeProgram` using `DatabaseId::MAIN`, custom scalar calls, and custom collation
    bt.begin_read().unwrap();
    let mut prog = VdbeProgram::new();
    prog.num_registers = 10;
    prog.num_cursors = 1;
    prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    prog.emit(Instruction::new(Opcode::OpenRead, 0, root as i32, 1).with_db(DatabaseId::MAIN));
    let rew = prog.emit(Instruction::new(Opcode::Rewind, 0, 0, 0));
    prog.emit(Instruction::new(Opcode::Column, 0, 0, 1));
    // r2 = mock_tag(r1) -> subtype 0x4A (74)
    prog.emit(
        Instruction::new(Opcode::Function, 1, 0, 2).with_p4(P4::Func {
            name: "mock_tag".into(),
            argc: 1,
        }),
    );
    // r3 = mock_inspect_sub(r2) -> 74, r4 = mock_strip_sub(r2) -> 0
    prog.emit(
        Instruction::new(Opcode::Function, 2, 0, 3).with_p4(P4::Func {
            name: "mock_inspect_sub".into(),
            argc: 1,
        }),
    );
    prog.emit(
        Instruction::new(Opcode::Function, 2, 0, 4).with_p4(P4::Func {
            name: "mock_strip_sub".into(),
            argc: 1,
        }),
    );
    // r5 = 'beta', r6 = (r1 < r5 COLLATE REV_ASCII)
    prog.emit(Instruction::new(Opcode::String8, 0, 5, 0).with_p4(P4::Str("beta".into())));
    prog.emit(Instruction::new(Opcode::Lt, 1, 5, 6).with_p4(P4::Str("REV_ASCII".into())));
    prog.emit(Instruction::new(Opcode::ResultRow, 3, 4, 0));
    prog.emit(Instruction::new(Opcode::Next, 0, (rew + 1) as i32, 0));
    let done_pc = prog.current_addr();
    prog.patch_p2(rew, done_pc);
    prog.emit(Instruction::new(Opcode::Close, 0, 0, 0));
    prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    let mut svc = StandaloneMockServices::new();
    let mut vm = VdbeVm::new(prog.clone(), Vec::new(), VmRuntimeContext::default());
    // Row 1 ("alpha"): REV_ASCII("alpha" < "beta") is false (0) because "beta" < "alpha" in reverse!
    assert_eq!(
        vm.step(&mut bt, &mut svc).unwrap(),
        VmStep::Row(vec![
            Value::Integer(74),
            Value::Integer(0),
            Value::Text("beta".into()),
            Value::Integer(0),
        ])
    );
    // Row 2 ("zeta"): REV_ASCII("zeta" < "beta") is true (1)!
    assert_eq!(
        vm.step(&mut bt, &mut svc).unwrap(),
        VmStep::Row(vec![
            Value::Integer(74),
            Value::Integer(0),
            Value::Text("beta".into()),
            Value::Integer(1),
        ])
    );
    assert_eq!(vm.step(&mut bt, &mut svc).unwrap(), VmStep::Done);
    assert!(svc.ticks >= 12);
    assert_eq!(svc.collation_calls, 2);

    // 3. Progress-tick interrupt and strict `RESULT_SUBTYPE` rejection via `ExecutionServices`
    let mut irq_svc = StandaloneMockServices::new();
    irq_svc.interrupt_after_ticks = Some(3);
    let mut irq_vm = VdbeVm::new(prog, Vec::new(), VmRuntimeContext::default());
    let irq_err = irq_vm.step(&mut bt, &mut irq_svc).unwrap_err();
    assert_eq!(irq_err.code, ErrorCode::Interrupt);

    let mut bad_prog = VdbeProgram::new();
    bad_prog.num_registers = 3;
    bad_prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    bad_prog.emit(
        Instruction::new(Opcode::Function, 0, 0, 1).with_p4(P4::Func {
            name: "mock_illegal_sub".into(),
            argc: 0,
        }),
    );
    bad_prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    let mut bad_vm = VdbeVm::new(bad_prog, Vec::new(), VmRuntimeContext::default());
    let bad_err = bad_vm
        .step(&mut bt, &mut StandaloneMockServices::new())
        .unwrap_err();
    assert_eq!(bad_err.code, ErrorCode::Error);
    assert_eq!(
        bad_err.message,
        "misuse of sqlite3_result_subtype() by mock_illegal_sub()"
    );
}

#[test]
fn group46_connection_disjoint_borrows_custom_functions_collations_progress_and_interrupt() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE items(id INTEGER PRIMARY KEY, word TEXT);
         INSERT INTO items VALUES (1, 'aa'), (2, 'b'), (3, 'ccc'), (4, 'dd');",
    )
    .unwrap();

    // 1. Custom fixed-arity & variadic scalar functions on BOTH compiled VDBE and interpreter
    let call_counter = Arc::new(AtomicUsize::new(0));
    let cc_clone = Arc::clone(&call_counter);
    conn.register_scalar_function(
        "double_plus",
        2,
        FunctionFlags::DETERMINISTIC,
        move |args| {
            cc_clone.fetch_add(1, AtomicOrdering::SeqCst);
            let a = args[0].value.to_integer_lossy();
            let b = args[1].value.to_integer_lossy();
            Ok(RuntimeValue::plain(Value::Integer(a * 2 + b)))
        },
    )
    .unwrap();

    conn.register_scalar_function("sum_variadic", -1, FunctionFlags::DETERMINISTIC, |args| {
        let total: i64 = args.iter().map(|rv| rv.value.to_integer_lossy()).sum();
        Ok(RuntimeValue::plain(Value::Integer(total)))
    })
    .unwrap();

    assert_eq!(
        conn.register_scalar_function("bad_arity", -2, FunctionFlags::NONE, |_| {
            Ok(RuntimeValue::null())
        })
        .unwrap_err()
        .code,
        ErrorCode::Misuse
    );

    // Compiled VDBE single-table SELECT (`EXPLAIN` contains `OpenRead`, no `OpenEphemeral`)
    let explain_vdbe = conn
        .query("EXPLAIN SELECT double_plus(id, 10) FROM items;", &[])
        .unwrap();
    assert!(
        explain_vdbe
            .rows
            .iter()
            .any(|r| r.get_str(1) == Some("OpenRead"))
    );
    let vdbe_rows = conn
        .query(
            "SELECT double_plus(id, 10), sum_variadic(id, 1, 2, 3) FROM items ORDER BY id;",
            &[],
        )
        .unwrap();
    let vdbe_ints: Vec<(i64, i64)> = vdbe_rows
        .rows
        .iter()
        .map(|r| (r.get_i64(0).unwrap(), r.get_i64(1).unwrap()))
        .collect();
    assert_eq!(vdbe_ints, vec![(12, 7), (14, 8), (16, 9), (18, 10)]);

    // Relational interpreter SELECT (CTE + JOIN forces interpreter path)
    let interp_rows = conn
        .query(
            "WITH base(k) AS (SELECT id FROM items)
             SELECT double_plus(base.k, 10), sum_variadic(base.k, 1, 2, 3)
             FROM base JOIN items ON items.id = base.k
             ORDER BY base.k;",
            &[],
        )
        .unwrap();
    let interp_ints: Vec<(i64, i64)> = interp_rows
        .rows
        .iter()
        .map(|r| (r.get_i64(0).unwrap(), r.get_i64(1).unwrap()))
        .collect();
    assert_eq!(interp_ints, vdbe_ints);
    assert_eq!(call_counter.load(AtomicOrdering::SeqCst), 8);

    // Unregistering removes the function cleanly
    assert!(conn.unregister_scalar_function("sum_variadic", -1).unwrap());
    assert!(!conn.unregister_scalar_function("sum_variadic", -1).unwrap());
    assert!(
        conn.query("SELECT sum_variadic(1, 2);", &[])
            .unwrap_err()
            .message
            .contains("no such function: sum_variadic")
    );

    // 2. Custom collation (`BY_LEN_DESC`: longer strings compare smaller!) across VDBE & interpreter
    conn.register_collation("BY_LEN_DESC", |a, b| {
        b.len().cmp(&a.len()).then_with(|| a.cmp(b))
    })
    .unwrap();

    // Compiled VDBE `WHERE` comparison with custom collation
    let vdbe_coll = conn
        .query(
            "SELECT id FROM items WHERE word < 'xx' COLLATE BY_LEN_DESC ORDER BY id;",
            &[],
        )
        .unwrap();
    let vdbe_coll_ids: Vec<i64> = vdbe_coll
        .rows
        .iter()
        .map(|r| r.get_i64(0).unwrap())
        .collect();
    // Under BY_LEN_DESC: 'ccc' (len 3) < 'xx' (len 2), and same-len 'aa' < 'xx', 'dd' < 'xx'!
    assert_eq!(vdbe_coll_ids, vec![1, 3, 4]);

    let vdbe_strict_len = conn
        .query(
            "SELECT id FROM items WHERE word < 'aa' COLLATE BY_LEN_DESC ORDER BY id;",
            &[],
        )
        .unwrap();
    assert_eq!(vdbe_strict_len.rows.len(), 1);
    assert_eq!(vdbe_strict_len.rows[0].get_i64(0), Some(3));

    // Interpreter `WHERE` + `ORDER BY ... COLLATE BY_LEN_DESC`
    let interp_coll = conn
        .query(
            "WITH w(id, word) AS (SELECT id, word FROM items)
             SELECT word FROM w WHERE word <= 'xx' COLLATE BY_LEN_DESC
             ORDER BY word COLLATE BY_LEN_DESC;",
            &[],
        )
        .unwrap();
    let sorted_words: Vec<&str> = interp_coll
        .rows
        .iter()
        .map(|r| r.get_str(0).unwrap())
        .collect();
    assert_eq!(sorted_words, vec!["ccc", "aa", "dd"]);

    // 3. Progress handler & op_count across BOTH compiled VDBE and interpreter
    conn.reset_progress_op_count().unwrap();
    assert_eq!(conn.progress_op_count().unwrap(), 0);
    let cb_hits = Arc::new(AtomicUsize::new(0));
    let cb_hits_clone = Arc::clone(&cb_hits);
    conn.progress_handler(
        2,
        Some(Box::new(move || {
            cb_hits_clone.fetch_add(1, AtomicOrdering::SeqCst);
            false
        })),
    )
    .unwrap();
    let _ = conn.query("SELECT id, word FROM items;", &[]).unwrap();
    let vdbe_ops = conn.progress_op_count().unwrap();
    assert!(vdbe_ops >= 8);
    assert!(cb_hits.load(AtomicOrdering::SeqCst) >= 4);

    // Abort via progress handler returning `true` on BOTH compiled VDBE and interpreter (`WITH RECURSIVE`)
    conn.progress_handler(3, Some(Box::new(|| true))).unwrap();
    assert_eq!(
        conn.query("SELECT id, word FROM items;", &[])
            .unwrap_err()
            .code,
        ErrorCode::Interrupt
    );
    assert_eq!(
        conn.query(
            "WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM cnt WHERE x < 100)
             SELECT x FROM cnt;",
            &[],
        )
        .unwrap_err()
        .code,
        ErrorCode::Interrupt
    );
    conn.progress_handler(0, None).unwrap();

    // 4. `InterruptHandle` & custom `set_interrupt_flag` on compiled VDBE and interpreter
    let irq = conn.interrupt_handle();
    irq.interrupt();
    assert_eq!(
        conn.query("SELECT id FROM items;", &[]).unwrap_err().code,
        ErrorCode::Interrupt
    );
    conn.interrupt_flag().store(false, AtomicOrdering::SeqCst);
    let custom_flag = Arc::new(AtomicBool::new(true));
    conn.set_interrupt_flag(Some(Arc::clone(&custom_flag)))
        .unwrap();
    assert_eq!(
        conn.query("SELECT id FROM items;", &[]).unwrap_err().code,
        ErrorCode::Interrupt
    );
    custom_flag.store(false, AtomicOrdering::SeqCst);
    assert_eq!(
        conn.query("SELECT count(*) FROM items;", &[]).unwrap().rows[0].get_i64(0),
        Some(4)
    );
}

// ============================================================================
// Group 64: Structural Qualified AST, Catalog Normalization & `RETURNING` Rules
// ============================================================================

#[test]
fn group64_ast_structural_qualified_names_and_attach_detach() {
    // 1. ATTACH / DETACH AST
    let (s_att, _) = parse_one("ATTACH DATABASE 'aux.db' AS aux KEY 'k1';").unwrap();
    assert!(matches!(
        s_att,
        Some(Stmt::AttachDatabase {
            expr: Expr::String(ref p),
            schema_name: Expr::Column(ref c),
            key: Some(Expr::String(ref k)),
        }) if p == "aux.db" && c == &ColumnRef::unqualified("aux") && k == "k1"
    ));

    let (s_det, _) = parse_one("DETACH \"my.aux\";").unwrap();
    assert!(matches!(
        s_det,
        Some(Stmt::DetachDatabase {
            schema_name: Expr::Column(ref c),
        }) if c == &ColumnRef::unqualified("my.aux")
    ));

    // Step 2-C runtime behavior for ATTACH / DETACH
    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(
        conn.execute("DETACH DATABASE main;").unwrap_err().message,
        "cannot detach database main"
    );
    conn.execute("ATTACH ':memory:' AS aux;").unwrap();
    conn.execute("DETACH DATABASE aux;").unwrap();

    // 2. Structural DDL AST + normalized `catalog_sql`
    let (s_ct, _) = parse_one(
        "CREATE TEMP TABLE IF NOT EXISTS main.\"a.b\" (id INT PRIMARY KEY, \"c.d\" TEXT);",
    )
    .unwrap();
    let Some(Stmt::CreateTable(ct)) = s_ct else {
        panic!("expected CreateTable");
    };
    assert!(ct.temp);
    assert!(ct.if_not_exists);
    assert_eq!(ct.name, QualifiedName::qualified("main", "a.b"));
    assert_eq!(
        ct.catalog_sql,
        "CREATE TABLE \"a.b\" (id INT PRIMARY KEY, \"c.d\" TEXT)"
    );

    let (s_ctas, _) =
        parse_one("CREATE TEMPORARY TABLE IF NOT EXISTS \"x.y\" AS SELECT 1 AS col;").unwrap();
    assert!(matches!(
        s_ctas,
        Some(Stmt::CreateTableAs {
            temp: true,
            if_not_exists: true,
            ref name,
            ..
        }) if name == &QualifiedName::unqualified("x.y")
    ));

    let (s_ci, _) =
        parse_one("CREATE UNIQUE INDEX IF NOT EXISTS main.\"idx.a\" ON \"a.b\"(\"c.d\");").unwrap();
    let Some(Stmt::CreateIndex(ci)) = s_ci else {
        panic!("expected CreateIndex");
    };
    assert!(ci.unique);
    assert!(ci.if_not_exists);
    assert_eq!(ci.name, QualifiedName::qualified("main", "idx.a"));
    assert_eq!(ci.table_name, "a.b");
    assert_eq!(
        ci.catalog_sql,
        "CREATE UNIQUE INDEX \"idx.a\" ON \"a.b\"(\"c.d\")"
    );

    // 3. DROP / ALTER / ANALYZE / REINDEX / INSERT / UPDATE / DELETE / SELECT 3-part & IN table
    let (s_dt, _) = parse_one("DROP TABLE IF EXISTS main.\"a.b\";").unwrap();
    assert!(matches!(
        s_dt,
        Some(Stmt::DropTable { if_exists: true, ref name })
            if name == &QualifiedName::qualified("main", "a.b")
    ));

    let (s_di, _) = parse_one("DROP INDEX IF EXISTS temp.\"idx.a\";").unwrap();
    assert!(matches!(
        s_di,
        Some(Stmt::DropIndex { if_exists: true, ref name })
            if name == &QualifiedName::qualified("temp", "idx.a")
    ));

    let (s_alt, _) = parse_one("ALTER TABLE main.\"a.b\" RENAME TO \"e.f\";").unwrap();
    assert!(matches!(
        s_alt,
        Some(Stmt::AlterTable { ref table, .. })
            if table == &QualifiedName::qualified("main", "a.b")
    ));

    let (s_ana, _) = parse_one("ANALYZE main.\"a.b\";").unwrap();
    assert!(matches!(
        s_ana,
        Some(Stmt::Analyze(Some(ref qn))) if qn == &QualifiedName::qualified("main", "a.b")
    ));

    let (s_rei, _) = parse_one("REINDEX main.\"a.b\";").unwrap();
    assert!(matches!(
        s_rei,
        Some(Stmt::Reindex(Some(ref qn))) if qn == &QualifiedName::qualified("main", "a.b")
    ));

    let (s_ins, _) = parse_one("INSERT INTO main.\"a.b\" AS al VALUES (1);").unwrap();
    assert!(matches!(
        s_ins,
        Some(Stmt::Insert(ref ins))
            if ins.table == QualifiedName::qualified("main", "a.b")
                && ins.alias.as_deref() == Some("al")
    ));

    let (s_sel, _) = parse_one(
        "SELECT main.\"a.b\".\"c.d\", \"a.b\".*, main.\"a.b\".*
         FROM main.\"a.b\"
         WHERE \"c.d\" IN main.\"t.2\";",
    )
    .unwrap();
    let Some(Stmt::Select(sel)) = s_sel else {
        panic!("expected Select");
    };
    let SelectCore::Select {
        projection,
        from,
        selection,
        ..
    } = &sel.body.head
    else {
        panic!("expected SelectCore::Select");
    };
    assert_eq!(
        projection[0],
        ResultColumn::Expr {
            expr: Expr::Column(ColumnRef {
                schema: Some("main".into()),
                table: Some("a.b".into()),
                column: "c.d".into(),
            }),
            alias: None,
        }
    );
    assert_eq!(
        projection[1],
        ResultColumn::TableStar {
            schema: None,
            table: "a.b".into(),
        }
    );
    assert_eq!(
        projection[2],
        ResultColumn::TableStar {
            schema: Some("main".into()),
            table: "a.b".into(),
        }
    );
    assert!(matches!(
        from.as_ref().unwrap().head,
        TableOrSubquery::Table { ref name, .. }
            if name == &QualifiedName::qualified("main", "a.b")
    ));
    assert!(matches!(
        selection,
        Some(Expr::InTable { table, negated: false, .. })
            if table == &QualifiedName::qualified("main", "t.2")
    ));
}

#[test]
fn group64_quoted_dots_three_part_refs_in_table_and_catalog_sql_c_parity() {
    let script = "
        CREATE TABLE IF NOT EXISTS main.norm_t(id INTEGER PRIMARY KEY, \"q.col\" TEXT);
        CREATE UNIQUE INDEX IF NOT EXISTS main.\"norm.idx\" ON norm_t(\"q.col\");
        CREATE TABLE IF NOT EXISTS main.\"a.b\"(\"c.d\" INT, plain INT);
        CREATE TABLE main.ctas_t AS SELECT 10 AS k, 'hello' AS v;
        CREATE TABLE main.lookup1(x INT);
        CREATE TABLE main.lookup2(x INT, y INT);

        INSERT INTO main.norm_t VALUES (1, 'one'), (2, 'two');
        INSERT INTO main.\"a.b\" VALUES (7, 70), (8, 80);
        INSERT INTO main.lookup1 VALUES (7), (99);
        INSERT INTO main.lookup2 VALUES (7, 70), (9, 90);

        SELECT type, name, tbl_name, sql FROM main.sqlite_schema ORDER BY name;
        SELECT \"a.b\".\"c.d\", main.\"a.b\".\"c.d\", \"a.b\".* FROM main.\"a.b\" ORDER BY plain;
        SELECT main.al.\"c.d\", al.* FROM main.\"a.b\" AS al WHERE al.\"c.d\" IN main.lookup1;
        SELECT \"c.d\", plain FROM \"a.b\" WHERE (\"c.d\", plain) IN main.lookup2;
        SELECT \"c.d\" FROM \"a.b\" WHERE (\"c.d\", plain) NOT IN \"a.b\";
        SELECT count(*) FROM temp.sqlite_schema;
        SELECT count(*) FROM sqlite_temp_schema;
    ";

    let c_rows = run_c_sql(script).expect("C SQLite script should succeed");
    let conn = Connection::open_in_memory().unwrap();
    let r_rows = run_rust_script(&conn, script).expect("Rust script should succeed");
    assert_eq!(r_rows, c_rows);

    // Verify 3-part schema-qualified `TableStar` (`SELECT main."a.b".*` / `SELECT main.al.*`)
    let three_part_star = run_rust_script(
        &conn,
        "SELECT main.\"a.b\".* FROM main.\"a.b\" ORDER BY plain;
         SELECT main.al.* FROM main.\"a.b\" AS al ORDER BY plain;",
    )
    .unwrap();
    assert_eq!(three_part_star, vec!["7|70", "8|80", "7|70", "8|80"]);

    // Negative cases differentially verified against C SQLite 3.54.1:
    let error_cases = [
        // Alias hides original table name even in 3-part reference!
        (
            "CREATE TABLE t(c INT); SELECT main.t.c FROM main.t AS al;",
            "no such column: main.t.c",
        ),
        // `IN table` column width mismatch!
        (
            "CREATE TABLE t2(a INT, b INT); SELECT 1 IN main.t2;",
            "sub-select returns 2 columns - expected 1",
        ),
        (
            "CREATE TABLE t1(a INT); SELECT (1, 2) IN main.t1;",
            "sub-select returns 1 columns - expected 2",
        ),
        // `CREATE TEMP TABLE` with explicit schema!
        (
            "CREATE TEMP TABLE main.bad(x INT);",
            "temporary table name must be unqualified",
        ),
        (
            "CREATE TEMP TABLE nosuch.bad(x INT);",
            "unknown database nosuch",
        ),
        // `main.sqlite_temp_schema` does not exist in `main`!
        (
            "SELECT * FROM main.sqlite_temp_schema;",
            "no such table: main.sqlite_temp_schema",
        ),
    ];

    for (bad_sql, expected_msg) in error_cases {
        let c_err = run_c_sql(bad_sql).expect_err("C SQLite must reject query");
        assert!(
            c_err.contains(expected_msg),
            "C error {c_err:?} did not contain {expected_msg:?} for {bad_sql}"
        );
        let r_conn = Connection::open_in_memory().unwrap();
        let r_err = run_rust_script(&r_conn, bad_sql).expect_err("Rust must reject query");
        assert_eq!(r_err.message, expected_msg, "SQL: {bad_sql}");
    }
}

#[test]
fn group64_dml_alias_and_returning_wildcard_and_qualifier_c_parity() {
    let dml_script = "
        CREATE TABLE main.rt(id INTEGER PRIMARY KEY, v INT);
        INSERT INTO main.rt AS al(id, v) VALUES (1, 10), (2, 20)
            RETURNING rt.id, v, *;
        UPDATE main.rt AS al SET v = al.v + 100 WHERE al.id = 2
            RETURNING rt.id, rt.v, *;
        DELETE FROM main.rt AS al WHERE al.id = 1
            RETURNING rt.id, v, *;
        SELECT id, v FROM main.rt ORDER BY id;
    ";

    let c_rows = run_c_sql(dml_script).expect("C DML + RETURNING should succeed");
    let conn = Connection::open_in_memory().unwrap();
    let r_rows = run_rust_script(&conn, dml_script).expect("Rust DML + RETURNING should succeed");
    assert_eq!(r_rows, c_rows);

    let returning_errors = [
        (
            "CREATE TABLE rt(id INT, v INT); INSERT INTO rt VALUES (1, 2) RETURNING rt.*;",
            "RETURNING may not use \"TABLE.*\" wildcards",
        ),
        (
            "CREATE TABLE rt(id INT, v INT); UPDATE rt AS al SET v = 3 RETURNING al.*;",
            "RETURNING may not use \"TABLE.*\" wildcards",
        ),
        (
            "CREATE TABLE rt(id INT, v INT); DELETE FROM main.rt RETURNING rt.*;",
            "RETURNING may not use \"TABLE.*\" wildcards",
        ),
        (
            "CREATE TABLE rt(id INT, v INT); INSERT INTO main.rt AS al VALUES (1, 10) RETURNING al.v;",
            "no such column: al.v",
        ),
        (
            "CREATE TABLE rt(id INT, v INT); INSERT INTO rt VALUES (1, 10); UPDATE rt AS al SET v = al.v + 1 RETURNING al.v;",
            "no such column: al.v",
        ),
        (
            "CREATE TABLE rt(id INT, v INT); INSERT INTO rt VALUES (1, 10); UPDATE main.rt SET v = v + 1 RETURNING main.rt.v;",
            "no such column: main.rt.v",
        ),
    ];

    for (bad_sql, expected_msg) in returning_errors {
        let c_err = run_c_sql(bad_sql).expect_err("C SQLite must reject RETURNING form");
        assert!(
            c_err.contains(expected_msg),
            "C error {c_err:?} did not contain {expected_msg:?}"
        );
        let r_conn = Connection::open_in_memory().unwrap();
        let r_err = run_rust_script(&r_conn, bad_sql).expect_err("Rust must reject RETURNING form");
        assert_eq!(r_err.message, expected_msg, "SQL: {bad_sql}");
    }

    // Also verify 3-part `RETURNING main.rt.*` is rejected with the exact wildcard error in Rust
    let r_conn = Connection::open_in_memory().unwrap();
    let r_err = run_rust_script(
        &r_conn,
        "CREATE TABLE rt(id INT, v INT); DELETE FROM main.rt RETURNING main.rt.*;",
    )
    .expect_err("Rust must reject RETURNING main.rt.*");
    assert_eq!(r_err.message, "RETURNING may not use \"TABLE.*\" wildcards");
}

// ============================================================================
// Group 65: `RuntimeValue` `u8` Subtype Contract & `SQLITE_STRICT_SUBTYPE` Parity
// ============================================================================

#[test]
fn group65_subtype_propagation_stripping_disk_and_subquery_barriers_c_parity() {
    // 0. Direct Format 4 record codec proof: only Value is encoded; decoded values are plain Value
    let encoded = encode_record(&[Value::Integer(42), Value::Text("tagged".into())]);
    let decoded = decode_record(&encoded).unwrap();
    assert_eq!(RuntimeValue::from(decoded[0].clone()).subtype, 0);
    assert_eq!(RuntimeValue::from(decoded[1].clone()).subtype, 0);

    // 1. Differential SQL script executed identically on C SQLite 3.54.1 and Rust (`enable_test_functions`)
    let subtype_script = "
        CREATE TABLE sub_tbl(id INTEGER PRIMARY KEY, txt TEXT, num INT);
        INSERT INTO sub_tbl VALUES (1, test_set_subtype('[1,2]', 74), test_set_subtype(99, 88));
        INSERT INTO sub_tbl VALUES (2, 'plain', 100);

        -- Plain literals and stored disk columns always have subtype 0
        SELECT subtype(NULL), subtype(42), subtype(3.5), subtype('abc'), subtype(X'ABCD');
        SELECT id, txt, subtype(txt), num, subtype(num) FROM sub_tbl ORDER BY id;

        -- Direct setting, low-8-bit 0xff mask (0x14A == 330 -> 74), and NULL with subtype
        SELECT
            subtype(test_set_subtype('json', 74)),
            subtype(test_set_subtype('json', 330)),
            subtype(test_set_subtype(NULL, 74));

        -- Subtype-preserving expressions (compiled VDBE tableless & single-table)
        SELECT
            subtype(+test_set_subtype('[1]', 74)),
            subtype(test_set_subtype('[1]', 74) COLLATE NOCASE),
            subtype(COALESCE(NULL, test_set_subtype('[1]', 74))),
            subtype(IFNULL(NULL, test_set_subtype('[1]', 74))),
            subtype(NULLIF(test_set_subtype('[1]', 74), 'other')),
            subtype(IIF(1, test_set_subtype('[1]', 74), 'fallback')),
            subtype(CASE WHEN 1 THEN test_set_subtype('[1]', 74) ELSE 'x' END),
            subtype(MAX(test_set_subtype('[1]', 74), 'A')),
            subtype(MIN(test_set_subtype('[1]', 74), 'z')),
            subtype(CAST(test_set_subtype('[1]', 74) AS TEXT));

        -- Subtype-clearing expressions
        SELECT
            subtype(test_set_subtype('[1]', 74) || ''),
            subtype(test_set_subtype('12', 74) + 0),
            subtype(-test_set_subtype('12', 74)),
            subtype(CAST(test_set_subtype('[1]', 74) AS BLOB)),
            subtype(CAST(test_set_subtype('123', 74) AS INT)),
            subtype(CAST(test_set_subtype('123', 74) AS REAL)),
            subtype(CAST(test_set_subtype('123', 74) AS NUMERIC)),
            subtype(LOWER(test_set_subtype('[A]', 74)));

        -- Same preserving & clearing expressions forced through the relational interpreter (JOIN)
        SELECT
            subtype(+test_set_subtype(a.txt, 74)),
            subtype(COALESCE(NULL, test_set_subtype(a.txt, 74))),
            subtype(CASE WHEN a.id = 1 THEN test_set_subtype(a.txt, 74) ELSE 'x' END),
            subtype(CAST(test_set_subtype(a.txt, 74) AS TEXT)),
            subtype(CAST(test_set_subtype(a.txt, 74) AS BLOB))
        FROM sub_tbl AS a JOIN sub_tbl AS b ON a.id = b.id
        WHERE a.id = 1;

        -- ORDER BY expression evaluation vs subquery / CTE / compound barriers
        SELECT subtype(test_set_subtype(txt, 74)) FROM sub_tbl ORDER BY id DESC;
        SELECT subtype(x) FROM (SELECT test_set_subtype('[1]', 74) AS x);
        SELECT subtype(x) FROM (SELECT test_set_subtype(txt, 74) AS x FROM sub_tbl ORDER BY id);
        WITH cte(x) AS (SELECT test_set_subtype('[1]', 74)) SELECT subtype(x) FROM cte;
        SELECT subtype(x) FROM (SELECT test_set_subtype('[1]', 74) AS x UNION ALL SELECT 'plain');
    ";

    let c_rows = run_c_sql(subtype_script).expect("C SQLite subtype script should succeed");
    let conn = Connection::open_in_memory().unwrap();
    conn.set_enable_test_functions(true).unwrap();
    let r_rows =
        run_rust_script(&conn, subtype_script).expect("Rust subtype script should succeed");
    assert_eq!(r_rows, c_rows);

    // Deterministic `FunctionFlags::SUBTYPE` stripping on unsubscribed functions across VDBE & interpreter
    let strip_rows = run_rust_script(
        &conn,
        "SELECT test_plain_arg_subtype(test_set_subtype('json', 74));
         SELECT test_plain_arg_subtype(test_set_subtype(a.txt, 74))
         FROM sub_tbl AS a JOIN sub_tbl AS b ON a.id = b.id WHERE a.id = 1;",
    )
    .unwrap();
    assert_eq!(strip_rows, vec!["0", "0"]);

    // Bound parameter subtype is always 0 on both compiled VDBE and interpreter
    let mut st = conn.prepare("SELECT subtype(?1);").unwrap();
    st.bind(1, Value::Text("[1]".into())).unwrap();
    assert_eq!(
        st.step().unwrap(),
        StepResult::Row(sqliters::Row::new(vec![Value::Integer(0)]))
    );
}

#[test]
fn group65_strict_result_subtype_and_custom_udf_subtype_flags_c_parity() {
    // 1. Differential C (`-DSQLITE_STRICT_SUBTYPE=1`) vs Rust check on `test_bad_subtype`
    let bad_vdbe_sql = "SELECT test_bad_subtype('x', 74);";
    let c_err = run_c_sql(bad_vdbe_sql).expect_err("C SQLite strict subtype must fail");
    assert!(
        c_err.contains("misuse of sqlite3_result_subtype() by test_bad_subtype()"),
        "unexpected C error: {c_err}"
    );

    let conn = Connection::open_in_memory().unwrap();
    conn.set_enable_test_functions(true).unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); INSERT INTO t VALUES (1, 'x');")
        .unwrap();

    // Compiled VDBE path
    let r_vdbe_err = conn.query(bad_vdbe_sql, &[]).unwrap_err();
    assert_eq!(r_vdbe_err.code, ErrorCode::Error);
    assert_eq!(
        r_vdbe_err.message,
        "misuse of sqlite3_result_subtype() by test_bad_subtype()"
    );

    // Relational interpreter path (JOIN)
    let r_interp_err = conn
        .query(
            "SELECT test_bad_subtype(a.v, 74) FROM t AS a JOIN t AS b ON a.id = b.id;",
            &[],
        )
        .unwrap_err();
    assert_eq!(r_interp_err.code, ErrorCode::Error);
    assert_eq!(
        r_interp_err.message,
        "misuse of sqlite3_result_subtype() by test_bad_subtype()"
    );

    // 2. Custom UDFs registered via `Connection::register_scalar_function`:
    //    - `udf_emit_sub` WITH `RESULT_SUBTYPE`
    //    - `udf_bad_emit` WITHOUT `RESULT_SUBTYPE`
    //    - `udf_read_sub` WITH `SUBTYPE`
    //    - `udf_plain_sub` WITHOUT `SUBTYPE`
    conn.register_scalar_function(
        "udf_emit_sub",
        1,
        FunctionFlags::DETERMINISTIC | FunctionFlags::RESULT_SUBTYPE,
        |args| Ok(RuntimeValue::with_subtype(args[0].value.clone(), 0x255)),
    )
    .unwrap();

    conn.register_scalar_function("udf_bad_emit", 2, FunctionFlags::DETERMINISTIC, |args| {
        let sub = args[1].value.to_integer_lossy() as u32;
        Ok(RuntimeValue::with_subtype(args[0].value.clone(), sub))
    })
    .unwrap();

    conn.register_scalar_function(
        "udf_read_sub",
        1,
        FunctionFlags::DETERMINISTIC | FunctionFlags::SUBTYPE,
        |args| {
            Ok(RuntimeValue::plain(Value::Integer(i64::from(
                args[0].subtype,
            ))))
        },
    )
    .unwrap();

    conn.register_scalar_function("udf_plain_sub", 1, FunctionFlags::DETERMINISTIC, |args| {
        Ok(RuntimeValue::plain(Value::Integer(i64::from(
            args[0].subtype,
        ))))
    })
    .unwrap();

    // Verify `0x255 & 0xff == 0x55 (85)` and SUBTYPE stripping on BOTH compiled VDBE and interpreter!
    let vdbe_udf = conn
        .query(
            "SELECT
                subtype(udf_emit_sub(v)),
                udf_read_sub(udf_emit_sub(v)),
                udf_plain_sub(udf_emit_sub(v)),
                udf_bad_emit(v, 0)
             FROM t;",
            &[],
        )
        .unwrap();
    assert_eq!(
        vdbe_udf.rows[0].values(),
        &[
            Value::Integer(0x55),
            Value::Integer(0x55),
            Value::Integer(0),
            Value::Text("x".into()),
        ]
    );

    let interp_udf = conn
        .query(
            "SELECT
                subtype(udf_emit_sub(a.v)),
                udf_read_sub(udf_emit_sub(a.v)),
                udf_plain_sub(udf_emit_sub(a.v)),
                udf_bad_emit(a.v, 0)
             FROM t AS a JOIN t AS b ON a.id = b.id;",
            &[],
        )
        .unwrap();
    assert_eq!(interp_udf.rows[0].values(), vdbe_udf.rows[0].values());

    // Non-zero subtype from `udf_bad_emit` fails identically on BOTH compiled VDBE and interpreter!
    for sql in [
        "SELECT udf_bad_emit(v, 1) FROM t;",
        "SELECT udf_bad_emit(a.v, 1) FROM t AS a JOIN t AS b ON a.id = b.id;",
    ] {
        let err = conn.query(sql, &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::Error);
        assert_eq!(
            err.message,
            "misuse of sqlite3_result_subtype() by udf_bad_emit()"
        );
    }
}

#[test]
fn group46_65_inline_shortcircuit_star_arg_and_custom_collation_regressions() {
    let conn = Connection::open_in_memory().unwrap();
    conn.set_enable_test_functions(true).unwrap();

    // Also register a custom collation `rev` (reverse ASCII ordering) matching `c_rev_collation`
    // AND a secondary collation `fold_eq` that treats 'alpha' and 'ALPHA_ALT' as equal!
    conn.register_collation("rev", |a, b| b.cmp(a)).unwrap();
    conn.register_collation("fold_eq", |a, b| {
        let norm = |s: &str| {
            if s.eq_ignore_ascii_case("alpha") || s.eq_ignore_ascii_case("alpha_alt") {
                "alpha".to_owned()
            } else {
                s.to_ascii_lowercase()
            }
        };
        norm(a).cmp(&norm(b))
    })
    .unwrap();

    // 1. Differential C SQLite 3.54.1 vs Rust checks for `COALESCE`, `IFNULL`, `IIF`, and `IF`
    //    short-circuiting over `test_bad_subtype('boom', 1)` and preserving `subtype == 74`
    //    across BOTH compiled VDBE (`SELECT ...`) and CTE-forced interpreter (`WITH c AS ...`)!
    let inline_exprs = [
        // COALESCE / IFNULL short-circuiting + subtype preservation
        "COALESCE(test_set_subtype('ok', 74), test_bad_subtype('boom', 1))",
        "subtype(COALESCE(test_set_subtype('ok', 74), test_bad_subtype('boom', 1)))",
        "COALESCE(NULL, test_set_subtype('second', 74), test_bad_subtype('boom', 1))",
        "subtype(COALESCE(NULL, test_set_subtype('second', 74), test_bad_subtype('boom', 1)))",
        "IFNULL(test_set_subtype('ifnull_ok', 74), test_bad_subtype('boom', 1))",
        "subtype(IFNULL(test_set_subtype('ifnull_ok', 74), test_bad_subtype('boom', 1)))",
        "IFNULL(NULL, test_set_subtype('ifnull_rhs', 74))",
        "subtype(IFNULL(NULL, test_set_subtype('ifnull_rhs', 74)))",
        // 2-arg IIF / IF
        "IIF(1, test_set_subtype('two_t', 74))",
        "subtype(IIF(1, test_set_subtype('two_t', 74)))",
        "IIF(0, test_bad_subtype('boom', 1))",
        "subtype(IIF(0, test_bad_subtype('boom', 1)))",
        "IF(1, test_set_subtype('if_two_t', 74))",
        "subtype(IF(1, test_set_subtype('if_two_t', 74)))",
        "IF(0, test_bad_subtype('boom', 1))",
        "subtype(IF(0, test_bad_subtype('boom', 1)))",
        // 3-arg IIF / IF
        "IIF(1, test_set_subtype('three_t', 74), test_bad_subtype('boom', 1))",
        "subtype(IIF(1, test_set_subtype('three_t', 74), test_bad_subtype('boom', 1)))",
        "IF(0, test_bad_subtype('boom', 1), test_set_subtype('three_f', 74))",
        "subtype(IF(0, test_bad_subtype('boom', 1), test_set_subtype('three_f', 74)))",
        // 4-arg and 5-arg multi-branch IIF / IF (with unreached conditions & branches!)
        "IF(0, test_bad_subtype('b1', 1), 1, test_set_subtype('yes4', 74))",
        "subtype(IF(0, test_bad_subtype('b1', 1), 1, test_set_subtype('yes4', 74)))",
        "IIF(1, test_set_subtype('first4', 74), test_bad_subtype('cond2', 1), test_bad_subtype('then2', 1))",
        "subtype(IIF(1, test_set_subtype('first4', 74), test_bad_subtype('cond2', 1), test_bad_subtype('then2', 1)))",
        "IIF(0, test_bad_subtype('b1', 1), 0, test_bad_subtype('b2', 1))",
        "IF(0, test_bad_subtype('b1', 1), 0, test_bad_subtype('b2', 1), test_set_subtype('else5', 74))",
        "subtype(IF(0, test_bad_subtype('b1', 1), 0, test_bad_subtype('b2', 1), test_set_subtype('else5', 74)))",
    ];

    for expr in inline_exprs {
        let vdbe_sql = format!("SELECT {expr};");
        let cte_sql = format!("WITH c AS (SELECT 1) SELECT {expr} FROM c;");
        let c_vdbe = run_c_sql(&vdbe_sql).unwrap_or_else(|e| panic!("C failed `{vdbe_sql}`: {e}"));
        let c_cte = run_c_sql(&cte_sql).unwrap_or_else(|e| panic!("C failed `{cte_sql}`: {e}"));
        assert_eq!(c_vdbe, c_cte, "C VDBE vs CTE mismatch for `{expr}`");

        let r_vdbe = run_rust_script(&conn, &vdbe_sql).unwrap_or_else(|e| {
            panic!("Rust VDBE failed `{vdbe_sql}`: {e}");
        });
        let r_cte = run_rust_script(&conn, &cte_sql).unwrap_or_else(|e| {
            panic!("Rust CTE interpreter failed `{cte_sql}`: {e}");
        });
        assert_eq!(r_vdbe, c_vdbe, "VDBE C-parity mismatch for `{expr}`");
        assert_eq!(
            r_cte, c_cte,
            "CTE interpreter C-parity mismatch for `{expr}`"
        );
    }

    // 2. Reject `*` on non-aggregate scalar functions:
    //    - Differential C SQLite 3.54.1 vs Rust check on `coalesce(*)`, `iif(*)`, `if(*)`, `abs(*)`
    //      across both compiled VDBE and CTE-forced interpreter;
    //    - Explicit Rust VDBE + CTE interpreter parity check on `random(*)` rejecting
    //      `FunctionArgs::Star` with `"wrong number of arguments to function random()"`.
    for bad_fn in ["coalesce", "iif", "if", "abs"] {
        for sql in [
            format!("SELECT {bad_fn}(*);"),
            format!("WITH c AS (SELECT 1) SELECT {bad_fn}(*) FROM c;"),
        ] {
            let expected_msg = format!("wrong number of arguments to function {bad_fn}()");
            let c_err = run_c_sql(&sql).expect_err(&sql);
            assert!(
                c_err.contains(&expected_msg),
                "C error for `{sql}` was `{c_err}`, expected `{expected_msg}`"
            );
            let r_err = conn.query(&sql, &[]).unwrap_err();
            assert_eq!(r_err.code, ErrorCode::Error);
            assert_eq!(r_err.message, expected_msg, "Rust mismatch on `{sql}`");
        }
    }
    for sql in [
        "SELECT random(*);",
        "WITH c AS (SELECT 1) SELECT random(*) FROM c;",
    ] {
        let r_err = conn.query(sql, &[]).unwrap_err();
        assert_eq!(r_err.code, ErrorCode::Error);
        assert_eq!(
            r_err.message, "wrong number of arguments to function random()",
            "Rust mismatch on `{sql}`"
        );
    }

    // 3. Custom collation in `CASE operand WHEN expr`, `BETWEEN`, and 1-column `IN (...)`
    //    across BOTH compiled VDBE and CTE-forced interpreter!
    //    First verify `rev` differentially against C SQLite 3.54.1 (`'c' < 'b' < 'a'` under `rev`):
    let rev_exprs = [
        "'b' COLLATE rev BETWEEN 'c' AND 'a'",
        "'b' COLLATE rev BETWEEN 'a' AND 'c'",
        "'b' BETWEEN 'c' COLLATE rev AND 'a' COLLATE rev",
        "CASE 'b' COLLATE rev WHEN 'a' THEN 1 WHEN 'b' THEN 2 ELSE 0 END",
        "'b' COLLATE rev IN ('a', 'b', 'c')",
        "'d' COLLATE rev IN ('a', 'b', 'c')",
    ];
    for expr in rev_exprs {
        let vdbe_sql = format!("SELECT {expr};");
        let cte_sql = format!("WITH c AS (SELECT 1) SELECT {expr} FROM c;");
        let c_out = run_c_sql(&vdbe_sql).unwrap();
        let r_vdbe = run_rust_script(&conn, &vdbe_sql).unwrap();
        let r_cte = run_rust_script(&conn, &cte_sql).unwrap();
        assert_eq!(r_vdbe, c_out, "rev VDBE mismatch for `{expr}`");
        assert_eq!(r_cte, c_out, "rev CTE mismatch for `{expr}`");
    }

    //    Now verify custom equality collation `fold_eq` (`'alpha' == 'ALPHA_ALT'` ONLY under
    //    `fold_eq`, NOT under `BINARY` or `NOCASE`!) on `CASE`, `BETWEEN`, and `IN (...)` across
    //    both compiled VDBE and CTE-forced interpreter:
    conn.execute(
        "CREATE TABLE coll_t(id INTEGER PRIMARY KEY, k TEXT);
         INSERT INTO coll_t VALUES (1, 'alpha'), (2, 'beta');",
    )
    .unwrap();

    let vdbe_coll_q = "SELECT
        CASE k COLLATE fold_eq WHEN 'ALPHA_ALT' THEN 'hit_op' ELSE 'miss' END,
        CASE k WHEN 'ALPHA_ALT' COLLATE fold_eq THEN 'hit_when' ELSE 'miss' END,
        CASE k COLLATE binary WHEN 'ALPHA_ALT' COLLATE fold_eq THEN 'hit_bin' ELSE 'miss_bin' END,
        k COLLATE fold_eq BETWEEN 'ALPHA_ALT' AND 'ALPHA_ALT',
        k COLLATE fold_eq IN ('zzz', 'ALPHA_ALT'),
        k COLLATE binary IN ('zzz', 'ALPHA_ALT' COLLATE fold_eq)
      FROM coll_t WHERE id = 1;";
    let cte_coll_q = "WITH c AS (SELECT id, k FROM coll_t WHERE id = 1)
      SELECT
        CASE k COLLATE fold_eq WHEN 'ALPHA_ALT' THEN 'hit_op' ELSE 'miss' END,
        CASE k WHEN 'ALPHA_ALT' COLLATE fold_eq THEN 'hit_when' ELSE 'miss' END,
        CASE k COLLATE binary WHEN 'ALPHA_ALT' COLLATE fold_eq THEN 'hit_bin' ELSE 'miss_bin' END,
        k COLLATE fold_eq BETWEEN 'ALPHA_ALT' AND 'ALPHA_ALT',
        k COLLATE fold_eq IN ('zzz', 'ALPHA_ALT'),
        k COLLATE binary IN ('zzz', 'ALPHA_ALT' COLLATE fold_eq)
      FROM c;";

    let expected_coll_row = [
        Value::Text("hit_op".into()),
        Value::Text("hit_when".into()),
        Value::Text("miss_bin".into()),
        Value::Integer(1),
        Value::Integer(1),
        Value::Integer(0),
    ];
    let r_coll_vdbe = conn.query(vdbe_coll_q, &[]).unwrap();
    let r_coll_cte = conn.query(cte_coll_q, &[]).unwrap();
    assert_eq!(r_coll_vdbe.rows[0].values(), &expected_coll_row);
    assert_eq!(r_coll_cte.rows[0].values(), &expected_coll_row);

    // 4. Custom `register_scalar_function("iif", 3, ...)` shadows 3-arg `iif(...)` on BOTH VDBE
    //    and CTE-forced interpreter, while 2-arg and 4-arg `iif(...)` still resolve to built-in
    //    short-circuiting `IIF` (`sqlite3FindFunction` overload resolution)!
    conn.register_scalar_function(
        "iif",
        3,
        FunctionFlags::DETERMINISTIC | FunctionFlags::RESULT_SUBTYPE,
        |_args| {
            Ok(RuntimeValue::with_subtype(
                Value::Text("custom_iif3".into()),
                91,
            ))
        },
    )
    .unwrap();

    for sql in [
        "SELECT iif(0, 'a', 'b'), subtype(iif(0, 'a', 'b')), iif(0, test_bad_subtype('b1', 1), 1, test_set_subtype('builtin4', 74)), subtype(iif(0, test_bad_subtype('b1', 1), 1, test_set_subtype('builtin4', 74))), iif(0, test_bad_subtype('b1', 1));",
        "WITH c AS (SELECT 1) SELECT iif(0, 'a', 'b'), subtype(iif(0, 'a', 'b')), iif(0, test_bad_subtype('b1', 1), 1, test_set_subtype('builtin4', 74)), subtype(iif(0, test_bad_subtype('b1', 1), 1, test_set_subtype('builtin4', 74))), iif(0, test_bad_subtype('b1', 1)) FROM c;",
    ] {
        let qr = conn.query(sql, &[]).unwrap();
        assert_eq!(
            qr.rows[0].values(),
            &[
                Value::Text("custom_iif3".into()),
                Value::Integer(91),
                Value::Text("builtin4".into()),
                Value::Integer(74),
                Value::Null,
            ],
            "failed overload resolution for `{sql}`"
        );
    }
}
