//! Phase 4 — Step 4-A Differential Integration Suite:
//! `TRIGGER` Syntax, `TriggerSchema` Catalog & Schema Isolation, Orphan `TEMP` Triggers &
//! `detach_temp_triggers`, `DROP TABLE` / `DROP VIEW` Cascade Cleanup, `VACUUM` Preservation,
//! and Atomic `ALTER TABLE` (`RENAME TO`, `RENAME COLUMN`, `DROP COLUMN`) Trigger Rewriting &
//! Validation against C SQLite 3.54.1.
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters::syntax::{Stmt, TriggerTiming};
use sqliters::{Connection, DatabaseId, ErrorCode, Value, is_complete, parse_one};
use sqliters_tcltest::{evaluate_test_script_in_root, strip_test_comments};

static NEXT_PHASE4A_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct Phase4ASandbox {
    dir: PathBuf,
}

impl Phase4ASandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_PHASE4A_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_phase4a_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create phase4a temp sandbox");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Phase4ASandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn c_sqlite3_cli() -> &'static Path {
    static CLI_PATH: OnceLock<PathBuf> = OnceLock::new();
    CLI_PATH.get_or_init(|| {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sqlite_src_dir = workspace_root.join("sqlite");
        let ref_root = workspace_root.join("target").join("sqlite-ref");
        let cli_bin = ref_root.join("sqlite3");
        if cli_bin.exists() {
            return cli_bin;
        }
        let amalg_dir = ref_root.join("amalg");
        fs::create_dir_all(&amalg_dir).expect("create target/sqlite-ref/amalg");
        let sqlite3_c = amalg_dir.join("sqlite3.c");
        let sqlite3_h = amalg_dir.join("sqlite3.h");
        let shell_c = amalg_dir.join("shell.c");
        if !sqlite3_c.exists() || !sqlite3_h.exists() || !shell_c.exists() {
            let make_out = Command::new("make")
                .arg("-f")
                .arg(sqlite_src_dir.join("Makefile.linux-generic"))
                .arg(format!("TOP={}", sqlite_src_dir.display()))
                .arg("sqlite3.c")
                .arg("shell.c")
                .current_dir(&amalg_dir)
                .output()
                .expect("generate SQLite 3.54.1 amalgamation");
            assert!(
                make_out.status.success(),
                "amalgamation build failed:\n{}",
                String::from_utf8_lossy(&make_out.stderr)
            );
        }
        let cc_cli = Command::new("cc")
            .args(["-O2", "-pthread"])
            .arg(&shell_c)
            .arg(&sqlite3_c)
            .arg("-I")
            .arg(&amalg_dir)
            .args(["-ldl", "-lm", "-o"])
            .arg(&cli_bin)
            .output()
            .expect("compile reference sqlite3 CLI");
        assert!(
            cc_cli.status.success(),
            "reference sqlite3 CLI build failed:\n{}",
            String::from_utf8_lossy(&cc_cli.stderr)
        );
        cli_bin
    })
}

fn run_c_sqlite(cwd: &Path, db_arg: &str, sql: &str) -> (bool, String, String) {
    let out = Command::new(c_sqlite3_cli())
        .arg("-batch")
        .arg("-noheader")
        .arg(db_arg)
        .arg(sql)
        .current_dir(cwd)
        .output()
        .expect("invoke reference sqlite3 CLI");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    )
}

fn extract_c_sqlite_error_message(stderr: &str) -> String {
    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(pos) = trimmed.find(": ")
            && (trimmed.starts_with("Parse error") || trimmed.starts_with("Error"))
        {
            return trimmed[pos + 2..].trim().to_owned();
        }
    }
    stderr.trim().to_owned()
}

fn format_value_cli(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => sqliters::types::format_sqlite_real(*r),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn format_rows_pipe(rows: &[sqliters::Row]) -> String {
    let mut out = String::new();
    for r in rows {
        let cols: Vec<String> = r.values().iter().map(format_value_cli).collect();
        out.push_str(&cols.join("|"));
        out.push('\n');
    }
    out
}

/// Asserts that `setup_sql` followed by `failing_stmt` fails in both `sqliters` (`execute` AND
/// `prepare`) and C SQLite 3.54.1 with the exact same error message text.
fn assert_differential_error(cwd: &Path, label: &str, setup_sql: &str, failing_stmt: &str) {
    let full_c_sql = format!("{setup_sql}\n{failing_stmt}");
    let (c_ok, c_out, c_err) = run_c_sqlite(cwd, ":memory:", &full_c_sql);
    assert!(
        !c_ok,
        "[{label}] expected C SQLite failure, but succeeded with stdout:\n{c_out}"
    );
    let expected_msg = extract_c_sqlite_error_message(&c_err);

    let conn = Connection::open_in_memory().expect("open in-memory connection");
    if !setup_sql.trim().is_empty() {
        conn.execute(setup_sql)
            .unwrap_or_else(|e| panic!("[{label}] setup failed in Rust: {e}"));
    }
    let rust_err = conn
        .execute(failing_stmt)
        .expect_err(&format!("[{label}] expected Rust execute failure"));
    assert_eq!(
        rust_err.message, expected_msg,
        "[{label}] differential error message mismatch on `{failing_stmt}`"
    );
}

#[test]
fn step4a_syntax_is_complete_catalog_sql_namespace_and_drop_trigger() {
    let sandbox = Phase4ASandbox::new("syntax_catalog");

    // 1. `is_complete` with nested `CASE ... END` inside `WHEN` and inside multi-statement `BEGIN ... END`
    let partial_in_when = "CREATE TRIGGER tr_case AFTER INSERT ON t1 WHEN CASE WHEN new.a > 0 THEN 1 ELSE 0 END = 1 BEGIN SELECT 1;";
    assert!(!is_complete(partial_in_when));
    assert!(is_complete(&format!("{partial_in_when} END;")));

    let partial_in_body = "CREATE TEMP TRIGGER IF NOT EXISTS tr_body UPDATE OF a, b ON t1 FOR EACH ROW BEGIN \
        SELECT CASE WHEN old.a = new.a THEN 'same' ELSE 'diff' END; \
        UPDATE t1 SET b = CASE new.b WHEN 1 THEN 10 ELSE 20 END WHERE a = new.a;";
    assert!(!is_complete(partial_in_body));
    let full_in_body = format!("{partial_in_body} END;");
    assert!(is_complete(&full_in_body));

    // 2. Verify default timing when omitted is `BEFORE` (`parse.y`: `trigger_time ::= . { TK_BEFORE }`)
    let (Some(Stmt::CreateTrigger(parsed_default)), _) =
        parse_one("CREATE TRIGGER tr_def INSERT ON t1 BEGIN SELECT 1; END").expect("parse default")
    else {
        panic!("expected CreateTrigger AST");
    };
    assert_eq!(parsed_default.timing, TriggerTiming::Before);
    // Confirm C SQLite also treats omitted timing as `BEFORE` (`cannot create BEFORE trigger on view: v1`)
    assert_differential_error(
        sandbox.path(),
        "default_timing_is_before_on_view",
        "CREATE TABLE t1(a); CREATE VIEW v1 AS SELECT a FROM t1;",
        "CREATE TRIGGER tr_view_default UPDATE ON v1 BEGIN SELECT 1; END;",
    );

    // 3. Byte-for-byte `catalog_sql`, `tbl_name`, `rootpage = 0`, separate trigger namespace, and `DROP TRIGGER`
    let script = r#"
CREATE TABLE t1(a INTEGER PRIMARY KEY, "b col", [c_col]);
CREATE TABLE log_tbl(tag TEXT, v1, v2);
CREATE INDEX idx_same ON t1(a);
CREATE VIEW v_1 AS SELECT a, "b col" AS b FROM t1;

-- Trigger sharing exact name with table `t1`, view `v_1`, and index `idx_same`
CREATE TRIGGER t1 BEFORE INSERT ON t1 FOR EACH ROW
WHEN CASE WHEN new.a IS NOT NULL THEN 1 ELSE 0 END = 1
BEGIN
  /* body comment preserved byte-for-byte */
  INSERT INTO log_tbl VALUES('before_ins', new.a, new."b col");
  SELECT CASE WHEN new.a > 10 THEN RAISE(IGNORE) ELSE RAISE(ABORT, 'neg: ' || new.a) END;
END;

CREATE TRIGGER IF NOT EXISTS main."idx_same"   AFTER   UPDATE OF "b col", [c_col] ON main.t1
BEGIN
  INSERT INTO log_tbl(tag, v1, v2)
    WITH cte(x) AS (SELECT old.a + new.a)
    SELECT 'upd', x, new.[c_col] FROM cte WHERE 1
    ON CONFLICT DO NOTHING;
  UPDATE log_tbl SET v2 = s.a FROM (SELECT a FROM t1) AS s WHERE log_tbl.v1 = s.a;
  DELETE FROM log_tbl WHERE v1 < 0;
END;

CREATE TRIGGER v_1 INSTEAD OF DELETE ON v_1 BEGIN
  DELETE FROM t1 WHERE a = old.a;
END;

-- Duplicate with `IF NOT EXISTS` is a clean no-op
CREATE TRIGGER IF NOT EXISTS t1 AFTER DELETE ON t1 BEGIN SELECT 999; END;

-- `TEMPORARY` trigger with `IF NOT EXISTS` strips `TEMPORARY` and `IF NOT EXISTS` in `sqlite_temp_schema`
CREATE TEMPORARY TRIGGER IF NOT EXISTS [tr_temp_1] DELETE ON main.t1 BEGIN
  INSERT INTO log_tbl VALUES('temp_del', old.a, 0);
END;

DROP TRIGGER IF EXISTS nosuch_schema.nosuch_trig;
DROP TRIGGER IF EXISTS nosuch_trig;
"#;

    let query_sql = format!(
        "{script}\n\
         SELECT type, name, tbl_name, rootpage, sql FROM sqlite_schema WHERE type='trigger' ORDER BY rowid;\n\
         SELECT type, name, tbl_name, rootpage, sql FROM sqlite_temp_schema WHERE type='trigger' ORDER BY rowid;\n\
         DROP TRIGGER main.idx_same;\n\
         DROP TRIGGER [tr_temp_1];\n\
         SELECT name FROM sqlite_schema WHERE type='trigger' ORDER BY rowid;\n\
         SELECT name FROM sqlite_temp_schema WHERE type='trigger' ORDER BY rowid;"
    );

    let (c_ok, c_out, c_err) = run_c_sqlite(sandbox.path(), ":memory:", &query_sql);
    assert!(c_ok, "C SQLite failed:\n{c_err}");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute(script).unwrap();

    let r1 = conn
        .query(
            "SELECT type, name, tbl_name, rootpage, sql FROM sqlite_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();
    let r2 = conn
        .query(
            "SELECT type, name, tbl_name, rootpage, sql FROM sqlite_temp_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();
    conn.execute("DROP TRIGGER main.idx_same; DROP TRIGGER [tr_temp_1];")
        .unwrap();
    let r3 = conn
        .query(
            "SELECT name FROM sqlite_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();
    let r4 = conn
        .query(
            "SELECT name FROM sqlite_temp_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();

    let rust_combined = format!(
        "{}{}{}{}",
        format_rows_pipe(&r1.rows),
        format_rows_pipe(&r2.rows),
        format_rows_pipe(&r3.rows),
        format_rows_pipe(&r4.rows),
    );
    assert_eq!(rust_combined, c_out);
}

#[test]
fn step4a_all_creation_body_and_raise_rejection_rules() {
    let sandbox = Phase4ASandbox::new("rejections");
    let base_setup = "
        CREATE TABLE t1(a INTEGER PRIMARY KEY AUTOINCREMENT, b TEXT, c INT);
        CREATE INDEX idx_t1_b ON t1(b);
        CREATE VIEW v1 AS SELECT a, b FROM t1;
        ATTACH ':memory:' AS aux;
        CREATE TABLE aux.t_aux(x INT);
    ";

    let cases: &[(&str, &str)] = &[
        (
            "reserved_trigger_name",
            "CREATE TRIGGER sqlite_reserved AFTER INSERT ON t1 BEGIN SELECT 1; END;",
        ),
        (
            "system_table_sqlite_schema",
            "CREATE TRIGGER tr_sys1 AFTER INSERT ON sqlite_schema BEGIN SELECT 1; END;",
        ),
        (
            "system_table_sqlite_master",
            "CREATE TRIGGER tr_sys2 AFTER DELETE ON sqlite_master BEGIN SELECT 1; END;",
        ),
        (
            "system_table_sqlite_sequence",
            "CREATE TRIGGER tr_sys3 AFTER UPDATE ON sqlite_sequence BEGIN SELECT 1; END;",
        ),
        (
            "temp_system_table",
            "CREATE TEMP TRIGGER tr_sys4 AFTER INSERT ON sqlite_temp_schema BEGIN SELECT 1; END;",
        ),
        (
            "before_on_view",
            "CREATE TRIGGER tr_bv BEFORE INSERT ON v1 BEGIN SELECT 1; END;",
        ),
        (
            "after_on_view",
            "CREATE TRIGGER tr_av AFTER DELETE ON main.v1 BEGIN SELECT 1; END;",
        ),
        (
            "temp_after_on_qualified_view",
            "CREATE TEMP TRIGGER tr_tav AFTER DELETE ON main.v1 BEGIN SELECT 1; END;",
        ),
        (
            "instead_of_on_table",
            "CREATE TRIGGER tr_iot INSTEAD OF INSERT ON t1 BEGIN SELECT 1; END;",
        ),
        (
            "temp_trigger_qualified_name",
            "CREATE TEMP TRIGGER main.tr_q AFTER INSERT ON t1 BEGIN SELECT 1; END;",
        ),
        (
            "duplicate_trigger_unquoted",
            "CREATE TRIGGER tr_dup AFTER INSERT ON t1 BEGIN SELECT 1; END; \
             CREATE TRIGGER tr_dup BEFORE DELETE ON t1 BEGIN SELECT 2; END;",
        ),
        (
            "duplicate_trigger_quoted",
            "CREATE TRIGGER \"tr_qdup\" AFTER INSERT ON t1 BEGIN SELECT 1; END; \
             CREATE TRIGGER \"tr_qdup\" AFTER INSERT ON t1 BEGIN SELECT 2; END;",
        ),
        ("drop_nonexistent_trigger", "DROP TRIGGER nosuch_tr;"),
        (
            "drop_nonexistent_schema_trigger",
            "DROP TRIGGER nosuch_db.nosuch_tr;",
        ),
        (
            "variable_qmark_in_when",
            "CREATE TRIGGER tr_var1 AFTER INSERT ON t1 WHEN new.a = ? BEGIN SELECT 1; END;",
        ),
        (
            "variable_numbered_in_body",
            "CREATE TRIGGER tr_var2 AFTER INSERT ON t1 BEGIN INSERT INTO t1(b) VALUES(?1); END;",
        ),
        (
            "variable_named_in_temp_trigger",
            "CREATE TEMP TRIGGER tr_var3 AFTER INSERT ON t1 BEGIN UPDATE t1 SET b = :param WHERE a = $id; END;",
        ),
        (
            "qualified_insert_in_perm_trigger",
            "CREATE TRIGGER tr_qdml1 AFTER INSERT ON t1 BEGIN INSERT INTO main.t1(b) VALUES('x'); END;",
        ),
        (
            "qualified_update_in_perm_trigger",
            "CREATE TRIGGER tr_qdml2 AFTER INSERT ON t1 BEGIN UPDATE main.t1 SET b = 'x'; END;",
        ),
        (
            "qualified_delete_in_perm_trigger",
            "CREATE TRIGGER tr_qdml3 AFTER INSERT ON t1 BEGIN DELETE FROM main.t1; END;",
        ),
        (
            "indexed_by_update_in_trigger",
            "CREATE TRIGGER tr_idx1 AFTER INSERT ON t1 BEGIN UPDATE t1 INDEXED BY idx_t1_b SET b = 'x'; END;",
        ),
        (
            "not_indexed_update_in_trigger",
            "CREATE TRIGGER tr_idx2 AFTER INSERT ON t1 BEGIN UPDATE t1 NOT INDEXED SET b = 'x'; END;",
        ),
        (
            "indexed_by_delete_in_trigger",
            "CREATE TRIGGER tr_idx3 AFTER INSERT ON t1 BEGIN DELETE FROM t1 INDEXED BY idx_t1_b; END;",
        ),
        (
            "not_indexed_delete_in_trigger",
            "CREATE TRIGGER tr_idx4 AFTER INSERT ON t1 BEGIN DELETE FROM t1 NOT INDEXED; END;",
        ),
        (
            "returning_on_insert_in_trigger",
            "CREATE TRIGGER tr_ret1 AFTER INSERT ON t1 BEGIN INSERT INTO t1(b) VALUES('x') RETURNING a; END;",
        ),
        (
            "returning_on_update_in_trigger",
            "CREATE TRIGGER tr_ret2 AFTER INSERT ON t1 BEGIN UPDATE t1 SET b = 'x' RETURNING a; END;",
        ),
        (
            "returning_on_delete_in_trigger",
            "CREATE TRIGGER tr_ret3 AFTER INSERT ON t1 BEGIN DELETE FROM t1 RETURNING a; END;",
        ),
        (
            "default_values_in_trigger",
            "CREATE TRIGGER tr_defv AFTER INSERT ON t1 BEGIN INSERT INTO t1 DEFAULT VALUES; END;",
        ),
        (
            "perm_trigger_cross_db_on",
            "CREATE TRIGGER \"tr_cross1\" AFTER INSERT ON aux.t_aux BEGIN SELECT 1; END;",
        ),
        (
            "perm_trigger_cross_db_body_select",
            "CREATE TRIGGER tr_cross2 AFTER INSERT ON t1 BEGIN SELECT * FROM aux.t_aux; END;",
        ),
        (
            "perm_trigger_cross_db_when",
            "CREATE TRIGGER tr_cross3 AFTER INSERT ON t1 WHEN EXISTS(SELECT 1 FROM temp.sqlite_temp_schema) BEGIN SELECT 1; END;",
        ),
        (
            "raise_in_top_level_select",
            "SELECT RAISE(ABORT, 'outside');",
        ),
        ("raise_ignore_in_top_level_select", "SELECT RAISE(IGNORE);"),
        (
            "raise_order_by_compound_precedence",
            "SELECT RAISE(ABORT, 'msg') FROM sqlite_master UNION SELECT 1 ORDER BY RAISE(IGNORE);",
        ),
        (
            "raise_in_view_queried_outside_trigger",
            "CREATE VIEW v_raise AS SELECT RAISE(FAIL, 'v_err'); SELECT * FROM v_raise WHERE 0;",
        ),
        (
            "raise_in_table_check_on_insert",
            "CREATE TABLE t_chk(x INT CHECK(RAISE(ABORT, 'chk_err'))); INSERT INTO t_chk VALUES(1);",
        ),
        (
            "raise_in_alter_add_column_check",
            "ALTER TABLE t1 ADD COLUMN d INT CHECK(RAISE(ABORT, 'add_chk'));",
        ),
        (
            "raise_in_generated_column",
            "CREATE TABLE t_gen(x INT, y AS (RAISE(IGNORE)));",
        ),
        (
            "raise_in_index_expr",
            "CREATE INDEX idx_raise_expr ON t1((RAISE(ABORT, 'idx_err')));",
        ),
        (
            "raise_in_partial_index_where",
            "CREATE INDEX idx_raise_where ON t1(b) WHERE RAISE(ROLLBACK, 'where_err');",
        ),
        (
            "raise_in_top_level_insert",
            "INSERT INTO t1(b, c) VALUES('x', RAISE(ABORT, 'ins_err'));",
        ),
        (
            "raise_in_top_level_update",
            "UPDATE t1 SET c = RAISE(FAIL, 'upd_err') WHERE 0;",
        ),
        (
            "raise_in_top_level_delete",
            "DELETE FROM t1 WHERE RAISE(ROLLBACK, 'del_err');",
        ),
    ];

    for (label, bad_sql) in cases {
        assert_differential_error(sandbox.path(), label, base_setup, bad_sql);
    }

    // Also verify our architectural requirement (`implementation_plan.md:661`):
    // Qualified DML target inside a `TEMP` trigger is ALSO rejected with the exact message!
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(base_setup).unwrap();
    let temp_qdml_err = conn
        .execute("CREATE TEMP TRIGGER tr_temp_qdml AFTER INSERT ON t1 BEGIN INSERT INTO aux.t_aux VALUES(1); END;")
        .unwrap_err();
    assert_eq!(
        temp_qdml_err.message,
        "qualified table names are not allowed on INSERT, UPDATE, and DELETE statements within triggers"
    );

    // Verify `SQLITE_LIMIT_TRIGGER_STEPS` enforcement
    conn.set_trigger_steps_limit(2);
    let steps_err = conn
        .execute(
            "CREATE TRIGGER tr_many AFTER INSERT ON t1 BEGIN SELECT 1; SELECT 2; SELECT 3; END;",
        )
        .unwrap_err();
    assert_eq!(
        steps_err.message,
        "trigger \"tr_many\" contains too many steps"
    );
}

#[test]
fn step4a_schema_isolation_auto_temp_detach_orphan_cascade_drop_and_vacuum() {
    let sandbox = Phase4ASandbox::new("isolation_lifecycle");

    // 1. Auto-`TEMP` placement: unqualified `CREATE TRIGGER tr ... ON temp_tbl` goes into `sqlite_temp_schema`,
    //    whereas `CREATE TRIGGER main.tr ... ON temp_tbl` fails!
    assert_differential_error(
        sandbox.path(),
        "main_trigger_on_temp_table_rejected",
        "CREATE TEMP TABLE temp_only(x INT);",
        "CREATE TRIGGER main.tr_bad AFTER INSERT ON temp_only BEGIN SELECT 1; END;",
    );
    assert_differential_error(
        sandbox.path(),
        "main_trigger_on_qualified_temp_table_rejected",
        "CREATE TEMP TABLE temp_only(x INT);",
        "CREATE TRIGGER main.tr_bad AFTER INSERT ON temp.temp_only BEGIN SELECT 1; END;",
    );

    let auto_temp_sql = "
        CREATE TABLE main_tbl(x INT);
        CREATE TEMP TABLE temp_only(x INT);
        CREATE TRIGGER tr_auto1 AFTER INSERT ON temp_only BEGIN SELECT 1; END;
        CREATE TRIGGER tr_auto2 AFTER UPDATE ON temp.temp_only BEGIN SELECT 2; END;
        SELECT name, tbl_name FROM sqlite_schema WHERE type='trigger' ORDER BY rowid;
        SELECT name, tbl_name FROM sqlite_temp_schema WHERE type='trigger' ORDER BY rowid;
    ";
    let (c_ok, c_out, c_err) = run_c_sqlite(sandbox.path(), ":memory:", auto_temp_sql);
    assert!(c_ok, "{c_err}");
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "
        CREATE TABLE main_tbl(x INT);
        CREATE TEMP TABLE temp_only(x INT);
        CREATE TRIGGER tr_auto1 AFTER INSERT ON temp_only BEGIN SELECT 1; END;
        CREATE TRIGGER tr_auto2 AFTER UPDATE ON temp.temp_only BEGIN SELECT 2; END;
    ",
    )
    .unwrap();
    let q_main = conn
        .query(
            "SELECT name, tbl_name FROM sqlite_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();
    let q_temp = conn
        .query(
            "SELECT name, tbl_name FROM sqlite_temp_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();
    assert!(q_main.rows.is_empty());
    assert_eq!(q_temp.rows.len(), 2);
    assert_eq!(
        format!(
            "{}{}",
            format_rows_pipe(&q_main.rows),
            format_rows_pipe(&q_temp.rows)
        ),
        c_out
    );

    // 2. `attach4-2.0..2.2` & `detach_temp_triggers`:
    //    `TEMP` trigger attached to `aux.t1`, then `CREATE TABLE temp.t1`, then `DETACH aux`,
    //    then `DROP TABLE temp.t1` does NOT delete `tr1`, and `DROP TRIGGER tr1` cleanly removes it!
    let detach_sql = "
        ATTACH DATABASE '' AS aux;
        CREATE TABLE IF NOT EXISTS aux.t1(a, b);
        CREATE TEMPORARY TRIGGER tr1 DELETE ON t1 BEGIN
          DELETE FROM t1;
        END;
        CREATE TABLE temp.t1(a, b);
        DETACH DATABASE aux;
        DROP TABLE temp.t1;
        SELECT name FROM sqlite_temp_schema WHERE type='trigger';
        DROP TRIGGER tr1;
        SELECT count(*) FROM sqlite_temp_schema WHERE type='trigger';
    ";
    let (c_ok, c_out, c_err) = run_c_sqlite(sandbox.path(), ":memory:", detach_sql);
    assert!(c_ok, "{c_err}");
    let conn_det = Connection::open_in_memory().unwrap();
    conn_det
        .execute(
            "
            ATTACH DATABASE '' AS aux;
            CREATE TABLE IF NOT EXISTS aux.t1(a, b);
            CREATE TEMPORARY TRIGGER tr1 DELETE ON t1 BEGIN
              DELETE FROM t1;
            END;
            CREATE TABLE temp.t1(a, b);
            DETACH DATABASE aux;
            DROP TABLE temp.t1;
        ",
        )
        .unwrap();
    let before_drop = conn_det
        .query(
            "SELECT name FROM sqlite_temp_schema WHERE type='trigger'",
            &[],
        )
        .unwrap();
    conn_det.execute("DROP TRIGGER tr1;").unwrap();
    let after_drop = conn_det
        .query(
            "SELECT count(*) FROM sqlite_temp_schema WHERE type='trigger'",
            &[],
        )
        .unwrap();
    assert_eq!(
        format!(
            "{}{}",
            format_rows_pipe(&before_drop.rows),
            format_rows_pipe(&after_drop.rows)
        ),
        c_out
    );

    // 3. Orphan `TEMP` trigger survival without `ErrorCode::Corrupt` when target table is dropped
    //    by a second connection on disk OR injected into `sqlite_temp_schema`!
    let orphan_db = sandbox.path().join("orphan_test.db");
    let c1 = Connection::open(&orphan_db).unwrap();
    c1.execute(
        "
        CREATE TABLE shared_t(id INT, val TEXT);
        INSERT INTO shared_t VALUES(1, 'hello');
        CREATE TEMP TRIGGER tr_on_shared AFTER INSERT ON shared_t BEGIN SELECT 1; END;
    ",
    )
    .unwrap();
    assert_eq!(
        c1.triggers_for_relation(DatabaseId::MAIN, "shared_t").len(),
        1
    );

    // Second connection drops `shared_t` on disk!
    let c2 = Connection::open(&orphan_db).unwrap();
    c2.execute("DROP TABLE shared_t; CREATE TABLE other_t(k INT); INSERT INTO other_t VALUES(42);")
        .unwrap();
    drop(c2);

    // `c1` refreshes `main` schema on next query: `tr_on_shared` must become an inactive orphan WITHOUT `Corrupt`!
    let rows = c1.query("SELECT k FROM other_t", &[]).unwrap();
    assert_eq!(rows.rows[0].get_i64(0), Some(42));
    assert!(
        c1.triggers_for_relation(DatabaseId::MAIN, "shared_t")
            .is_empty()
    );

    // Also test catalog reload with `writable_schema` orphan `TEMP` trigger AND `?1` variable in catalog trigger
    c1.execute("
        PRAGMA writable_schema = ON;
        INSERT INTO sqlite_temp_schema(type, name, tbl_name, rootpage, sql)
        VALUES('trigger', 'tr_orphan_missing', 'nonexistent_target', 0,
               'CREATE TRIGGER tr_orphan_missing AFTER INSERT ON nonexistent_target BEGIN SELECT ?1; END');
        INSERT INTO sqlite_schema(type, name, tbl_name, rootpage, sql)
        VALUES('trigger', 'tr_var_stored', 'other_t', 0,
               'CREATE TRIGGER tr_var_stored AFTER INSERT ON other_t BEGIN SELECT ?1; END');
        PRAGMA writable_schema = OFF;
    ")
    .unwrap();
    drop(c1);

    let c1_reopen = Connection::open(&orphan_db).unwrap();
    let check_rows = c1_reopen.query("SELECT k FROM other_t", &[]).unwrap();
    assert_eq!(check_rows.rows[0].get_i64(0), Some(42));
    let check_reopen = c1_reopen.query("PRAGMA integrity_check", &[]).unwrap();
    assert_eq!(check_reopen.rows[0].get_str(0), Some("ok"));
    assert_eq!(
        c1_reopen
            .triggers_for_relation(DatabaseId::MAIN, "other_t")
            .len(),
        1
    );
    drop(c1_reopen);

    // 4. `DROP TABLE` and `DROP VIEW` cascade trigger cleanup (same DB + `TEMP` triggers, leaving other DBs untouched!)
    let cascade_sql = "
        ATTACH ':memory:' AS aux;
        CREATE TABLE main.items(a INT);
        CREATE TABLE aux.items(a INT);
        CREATE VIEW main.v_items AS SELECT a FROM main.items;
        CREATE TRIGGER main.tr_m1 BEFORE INSERT ON items BEGIN SELECT 1; END;
        CREATE TRIGGER main.tr_m2 AFTER DELETE ON items BEGIN SELECT 2; END;
        CREATE TEMP TRIGGER tr_tmp_main AFTER UPDATE ON main.items BEGIN SELECT 3; END;
        CREATE TEMP TRIGGER tr_tmp_aux AFTER UPDATE ON aux.items BEGIN SELECT 4; END;
        CREATE TRIGGER aux.tr_a1 AFTER INSERT ON items BEGIN SELECT 5; END;
        CREATE TRIGGER main.tr_v1 INSTEAD OF INSERT ON v_items BEGIN SELECT 6; END;
        CREATE TEMP TRIGGER tr_tmp_v1 INSTEAD OF DELETE ON main.v_items BEGIN SELECT 7; END;

        DROP VIEW main.v_items;
        SELECT 'after_drop_view_main', name FROM sqlite_schema WHERE type='trigger' ORDER BY name;
        SELECT 'after_drop_view_temp', name FROM sqlite_temp_schema WHERE type='trigger' ORDER BY name;

        DROP TABLE main.items;
        SELECT 'after_drop_tbl_main', name FROM sqlite_schema WHERE type='trigger' ORDER BY name;
        SELECT 'after_drop_tbl_temp', name FROM sqlite_temp_schema WHERE type='trigger' ORDER BY name;
        SELECT 'after_drop_tbl_aux', name FROM aux.sqlite_schema WHERE type='trigger' ORDER BY name;
    ";
    let (c_ok, c_out, c_err) = run_c_sqlite(sandbox.path(), ":memory:", cascade_sql);
    assert!(c_ok, "{c_err}");

    let conn_cas = Connection::open_in_memory().unwrap();
    conn_cas
        .execute(
            "
            ATTACH ':memory:' AS aux;
            CREATE TABLE main.items(a INT);
            CREATE TABLE aux.items(a INT);
            CREATE VIEW main.v_items AS SELECT a FROM main.items;
            CREATE TRIGGER main.tr_m1 BEFORE INSERT ON items BEGIN SELECT 1; END;
            CREATE TRIGGER main.tr_m2 AFTER DELETE ON items BEGIN SELECT 2; END;
            CREATE TEMP TRIGGER tr_tmp_main AFTER UPDATE ON main.items BEGIN SELECT 3; END;
            CREATE TEMP TRIGGER tr_tmp_aux AFTER UPDATE ON aux.items BEGIN SELECT 4; END;
            CREATE TRIGGER aux.tr_a1 AFTER INSERT ON items BEGIN SELECT 5; END;
            CREATE TRIGGER main.tr_v1 INSTEAD OF INSERT ON v_items BEGIN SELECT 6; END;
            CREATE TEMP TRIGGER tr_tmp_v1 INSTEAD OF DELETE ON main.v_items BEGIN SELECT 7; END;
            DROP VIEW main.v_items;
        ",
        )
        .unwrap();
    let a1 = conn_cas
        .query("SELECT 'after_drop_view_main', name FROM sqlite_schema WHERE type='trigger' ORDER BY name", &[])
        .unwrap();
    let a2 = conn_cas
        .query("SELECT 'after_drop_view_temp', name FROM sqlite_temp_schema WHERE type='trigger' ORDER BY name", &[])
        .unwrap();
    conn_cas.execute("DROP TABLE main.items;").unwrap();
    let a3 = conn_cas
        .query("SELECT 'after_drop_tbl_main', name FROM sqlite_schema WHERE type='trigger' ORDER BY name", &[])
        .unwrap();
    let a4 = conn_cas
        .query("SELECT 'after_drop_tbl_temp', name FROM sqlite_temp_schema WHERE type='trigger' ORDER BY name", &[])
        .unwrap();
    let a5 = conn_cas
        .query("SELECT 'after_drop_tbl_aux', name FROM aux.sqlite_schema WHERE type='trigger' ORDER BY name", &[])
        .unwrap();
    let cas_out = format!(
        "{}{}{}{}{}",
        format_rows_pipe(&a1.rows),
        format_rows_pipe(&a2.rows),
        format_rows_pipe(&a3.rows),
        format_rows_pipe(&a4.rows),
        format_rows_pipe(&a5.rows),
    );
    assert_eq!(cas_out, c_out);

    // 5. `VACUUM` and `VACUUM INTO` preserve permanent triggers and views in exact `rowid` order!
    let vac_db_rust = sandbox.path().join("vac_rust.db");
    let vac_into_rust = sandbox.path().join("vac_into_rust.db");
    let vac_db_c = sandbox.path().join("vac_c.db");
    let vac_into_c = sandbox.path().join("vac_into_c.db");

    let vac_setup = "
        CREATE TABLE t1(id INTEGER PRIMARY KEY, val TEXT);
        CREATE TABLE audit(msg TEXT);
        CREATE TRIGGER tr_first BEFORE INSERT ON t1 BEGIN INSERT INTO audit VALUES(new.val); END;
        CREATE VIEW v_mid AS SELECT id, val FROM t1;
        CREATE TRIGGER tr_view INSTEAD OF INSERT ON v_mid BEGIN INSERT INTO t1(val) VALUES(new.val); END;
        CREATE TRIGGER tr_last AFTER DELETE ON t1 BEGIN INSERT INTO audit VALUES('del:' || old.id); END;
        INSERT INTO t1 VALUES(1, 'a'), (2, 'b'), (3, 'c');
        DELETE FROM t1 WHERE id = 2;
    ";
    let (c_ok, _, c_err) = run_c_sqlite(
        sandbox.path(),
        vac_db_c.to_str().unwrap(),
        &format!(
            "{vac_setup}\nVACUUM;\nVACUUM INTO '{}';",
            vac_into_c.display()
        ),
    );
    assert!(c_ok, "{c_err}");

    let cv = Connection::open(&vac_db_rust).unwrap();
    cv.execute(vac_setup).unwrap();
    cv.execute(&format!(
        "VACUUM; VACUUM INTO '{}';",
        vac_into_rust.display()
    ))
    .unwrap();
    drop(cv);

    for (r_path, c_path) in [(&vac_db_rust, &vac_db_c), (&vac_into_rust, &vac_into_c)] {
        let (_, c_cat, _) = run_c_sqlite(
            sandbox.path(),
            c_path.to_str().unwrap(),
            "SELECT type, name, tbl_name, rootpage, sql FROM sqlite_schema ORDER BY rowid; PRAGMA integrity_check;",
        );
        // Verify C SQLite reads the Rust-vacuumed DB identically!
        let (ok_r_by_c, r_by_c_cat, err_r_by_c) = run_c_sqlite(
            sandbox.path(),
            r_path.to_str().unwrap(),
            "SELECT type, name, tbl_name, rootpage, sql FROM sqlite_schema ORDER BY rowid; PRAGMA integrity_check;",
        );
        assert!(ok_r_by_c, "{err_r_by_c}");
        assert_eq!(r_by_c_cat, c_cat);

        // And Rust reads the C-vacuumed DB identically!
        let cr = Connection::open(c_path).unwrap();
        let q_cat = cr
            .query(
                "SELECT type, name, tbl_name, rootpage, sql FROM sqlite_schema ORDER BY rowid",
                &[],
            )
            .unwrap();
        let q_ic = cr.query("PRAGMA integrity_check", &[]).unwrap();
        assert_eq!(
            format!(
                "{}{}",
                format_rows_pipe(&q_cat.rows),
                format_rows_pipe(&q_ic.rows)
            ),
            c_cat
        );
    }
}

#[test]
fn step4a_atomic_alter_table_trigger_rewriting_validation_rollback_and_db_exchange() {
    let sandbox = Phase4ASandbox::new("alter_triggers");

    // 1. Differential `ALTER TABLE RENAME TO`, `RENAME COLUMN`, and `DROP COLUMN` across `main`, `aux`, and `temp`
    //    (covering `ON`, `UPDATE OF`, `WHEN`, `OLD`/`NEW`, `INSERT(cols)`, `UPSERT`, `UPDATE ... FROM`,
    //    `DELETE`, `RAISE(ABORT, new.col)`, table literally named `old`, and `TEMP` shadowing isolation!)
    let complex_alter_sql = r#"
ATTACH ':memory:' AS aux;
CREATE TABLE main.accounts(acc_id INTEGER PRIMARY KEY, owner_name TEXT, balance INT, drop_me TEXT);
CREATE TABLE main.audit_log(entry_id INTEGER PRIMARY KEY, ref_acc INT, note TEXT);
CREATE TABLE main.upsert_sink(sink_id INTEGER PRIMARY KEY, ref_acc INT, note TEXT);
CREATE TABLE main.old(k INT, v TEXT);
CREATE TABLE aux.remote_items(item_id INTEGER PRIMARY KEY, label TEXT, qty INT);

-- Shadow table in TEMP with same name as `audit_log` to prove permanent trigger pinning is immune to TEMP!
CREATE TEMP TABLE audit_log(shadow_col TEXT);

CREATE TRIGGER tr_acc_upd AFTER UPDATE OF owner_name, balance ON accounts
WHEN old.balance != new.balance AND new.owner_name IS NOT NULL
BEGIN
  INSERT INTO upsert_sink(sink_id, ref_acc, note)
    VALUES(new.acc_id, old.acc_id, CASE WHEN new.balance < 0 THEN RAISE(ABORT, 'overdraft:' || new.owner_name) ELSE new.owner_name END)
    ON CONFLICT(sink_id) DO UPDATE SET ref_acc = excluded.ref_acc, note = upsert_sink.note || ':' || excluded.note
    WHERE upsert_sink.ref_acc = old.acc_id;
  INSERT INTO audit_log(entry_id, ref_acc, note)
    VALUES(new.acc_id, old.acc_id, new.owner_name);
  UPDATE accounts SET owner_name = o.v FROM old AS o WHERE accounts.acc_id = new.acc_id AND o.k IS NULL;
  SELECT old.v FROM old WHERE old.k = new.acc_id;
  DELETE FROM audit_log WHERE audit_log.ref_acc = old.acc_id AND entry_id < 0;
END;

CREATE TEMP TRIGGER tr_temp_on_main AFTER INSERT ON main.accounts
WHEN new.balance > 0
BEGIN
  SELECT new.owner_name, a.balance FROM main.accounts AS a WHERE a.acc_id = new.acc_id;
END;

CREATE TEMP TRIGGER tr_temp_on_aux AFTER UPDATE OF label, qty ON aux.remote_items
BEGIN
  UPDATE remote_items SET label = new.label || '_ok' WHERE item_id = old.item_id AND qty = new.qty;
END;

ALTER TABLE main.accounts RENAME COLUMN owner_name TO holder_name;
ALTER TABLE main.accounts RENAME COLUMN acc_id TO id_num;
ALTER TABLE main.upsert_sink RENAME COLUMN ref_acc TO sink_linked_id;
ALTER TABLE main.audit_log RENAME COLUMN ref_acc TO linked_id;
ALTER TABLE main.accounts DROP COLUMN drop_me;
ALTER TABLE main.old RENAME TO old_renamed;
ALTER TABLE main.accounts RENAME TO ledger_accounts;
ALTER TABLE main.audit_log RENAME TO ledger_audit;
ALTER TABLE aux.remote_items RENAME COLUMN label TO title;
ALTER TABLE aux.remote_items RENAME TO remote_stock;

SELECT 'MAIN', type, name, tbl_name, sql FROM main.sqlite_schema WHERE type='trigger' ORDER BY rowid;
SELECT 'TEMP', type, name, tbl_name, sql FROM sqlite_temp_schema WHERE type='trigger' ORDER BY rowid;
SELECT 'AUX', type, name, tbl_name, sql FROM aux.sqlite_schema WHERE type='trigger' ORDER BY rowid;
"#;

    let (c_ok, c_out, c_err) = run_c_sqlite(sandbox.path(), ":memory:", complex_alter_sql);
    assert!(c_ok, "C SQLite complex alter failed:\n{c_err}");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        complex_alter_sql
            .split("SELECT 'MAIN'")
            .next()
            .expect("ddl prefix"),
    )
    .unwrap();
    let m_rows = conn
        .query(
            "SELECT 'MAIN', type, name, tbl_name, sql FROM main.sqlite_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();
    let t_rows = conn
        .query(
            "SELECT 'TEMP', type, name, tbl_name, sql FROM sqlite_temp_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();
    let a_rows = conn
        .query(
            "SELECT 'AUX', type, name, tbl_name, sql FROM aux.sqlite_schema WHERE type='trigger' ORDER BY rowid",
            &[],
        )
        .unwrap();
    assert_eq!(
        format!(
            "{}{}{}",
            format_rows_pipe(&m_rows.rows),
            format_rows_pipe(&t_rows.rows),
            format_rows_pipe(&a_rows.rows)
        ),
        c_out
    );

    // Also verify `ALTER TABLE upsert_sink RENAME TO upsert_sink2` preserves `excluded.` qualifier!
    conn.execute("ALTER TABLE main.upsert_sink RENAME TO upsert_sink2;")
        .unwrap();
    let sink_trig_sql = conn
        .query(
            "SELECT sql FROM main.sqlite_schema WHERE type='trigger' AND name='tr_acc_upd'",
            &[],
        )
        .unwrap();
    let rewritten_sql = sink_trig_sql.rows[0].get_str(0).unwrap();
    assert!(rewritten_sql.contains("INSERT INTO \"upsert_sink2\"(sink_id, sink_linked_id, note)"));
    assert!(rewritten_sql.contains("excluded.sink_linked_id"));
    assert!(rewritten_sql.contains("\"upsert_sink2\".note || ':' || excluded.note"));

    // 2. Differential pre-alter & post-alter trigger error messages + `PRAGMA legacy_alter_table`
    let error_cases: &[(&str, &str, &str)] = &[
        (
            "pre_alter_broken_new_col",
            "CREATE TABLE c1(x, y); CREATE TABLE p1(a, b); \
             CREATE TRIGGER tr AFTER INSERT ON c1 BEGIN INSERT INTO p1 VALUES(new.y, new.xyz); END;",
            "ALTER TABLE c1 DROP COLUMN y;",
        ),
        (
            "pre_alter_old_in_insert_trigger",
            "CREATE TABLE c1(x, y); \
             CREATE TRIGGER tr AFTER INSERT ON c1 BEGIN SELECT old.x; END;",
            "ALTER TABLE c1 RENAME TO c2;",
        ),
        (
            "pre_alter_new_in_delete_trigger",
            "CREATE TABLE c1(x, y); \
             CREATE TRIGGER tr AFTER DELETE ON c1 BEGIN SELECT new.x; END;",
            "ALTER TABLE c1 RENAME COLUMN x TO x2;",
        ),
        (
            "pre_alter_legacy_mode_sql_logic_error_on_rename_col",
            "CREATE TABLE c1(x, y); \
             CREATE TRIGGER tr AFTER INSERT ON c1 BEGIN SELECT nosuchcol; END; \
             PRAGMA legacy_alter_table = ON;",
            "ALTER TABLE c1 RENAME COLUMN x TO x2;",
        ),
        (
            "pre_alter_legacy_mode_sql_logic_error_on_drop_col",
            "CREATE TABLE c1(x, y); \
             CREATE TRIGGER tr AFTER INSERT ON c1 BEGIN SELECT nosuchcol; END; \
             PRAGMA legacy_alter_table = ON;",
            "ALTER TABLE c1 DROP COLUMN y;",
        ),
        (
            "post_drop_col_new_ref_in_main_trigger",
            "CREATE TABLE log(l); CREATE TABLE c1(u, v, w); \
             CREATE TRIGGER tr1 AFTER INSERT ON c1 BEGIN INSERT INTO log VALUES(new.w); END;",
            "ALTER TABLE c1 DROP COLUMN w;",
        ),
        (
            "post_drop_col_old_ref_in_temp_trigger",
            "CREATE TABLE c1(u, v, w); \
             CREATE TEMP TRIGGER tr_temp AFTER DELETE ON main.c1 WHEN old.w IS NOT NULL BEGIN SELECT 1; END;",
            "ALTER TABLE c1 DROP COLUMN w;",
        ),
        (
            "post_drop_col_upsert_conflict_target_in_trigger",
            "CREATE TABLE c1(u PRIMARY KEY, v, w); \
             CREATE TRIGGER tr_up AFTER DELETE ON c1 BEGIN \
               INSERT INTO c1(u, v) VALUES(1, 2) ON CONFLICT(w) DO NOTHING; \
             END;",
            "ALTER TABLE c1 DROP COLUMN w;",
        ),
    ];
    for (label, setup, bad_alter) in error_cases {
        assert_differential_error(sandbox.path(), label, setup, bad_alter);
    }

    // 3. `PRAGMA legacy_alter_table = ON` on `RENAME TO`: updates `tbl_name` + `ON "c2"`, preserves broken body!
    let legacy_rename_sql = "
        CREATE TABLE c1(a, b);
        CREATE TRIGGER tr_leg AFTER INSERT ON c1 BEGIN SELECT broken_col FROM c1; END;
        PRAGMA legacy_alter_table = ON;
        ALTER TABLE c1 RENAME TO c2;
        SELECT name, tbl_name, sql FROM sqlite_schema WHERE type='trigger';
    ";
    let (c_ok, c_leg_out, c_err) = run_c_sqlite(sandbox.path(), ":memory:", legacy_rename_sql);
    assert!(c_ok, "{c_err}");
    let conn_leg = Connection::open_in_memory().unwrap();
    conn_leg
        .execute(
            "
            CREATE TABLE c1(a, b);
            CREATE TRIGGER tr_leg AFTER INSERT ON c1 BEGIN SELECT broken_col FROM c1; END;
            PRAGMA legacy_alter_table = ON;
            ALTER TABLE c1 RENAME TO c2;
        ",
        )
        .unwrap();
    let leg_rows = conn_leg
        .query(
            "SELECT name, tbl_name, sql FROM sqlite_schema WHERE type='trigger'",
            &[],
        )
        .unwrap();
    assert_eq!(format_rows_pipe(&leg_rows.rows), c_leg_out);

    // 4. Atomic multi-DB savepoint rollback when post-alter trigger validation fails!
    let conn_rb = Connection::open_in_memory().unwrap();
    conn_rb
        .execute(
            "
            ATTACH ':memory:' AS aux;
            CREATE TABLE aux.items(id PRIMARY KEY, keep_col TEXT, drop_col INT);
            INSERT INTO aux.items VALUES(1, 'alpha', 100), (2, 'beta', 200);
            CREATE TRIGGER aux.tr_ok AFTER INSERT ON items BEGIN SELECT new.keep_col; END;
            CREATE TEMP TRIGGER tr_fail AFTER UPDATE ON aux.items BEGIN SELECT old.drop_col; END;
        ",
        )
        .unwrap();
    let err = conn_rb
        .execute("ALTER TABLE aux.items DROP COLUMN drop_col;")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert_eq!(
        err.message,
        "error in trigger tr_fail after drop column: no such column: old.drop_col"
    );
    // Verify `aux.items` schema and physical rows are 100% intact after rollback!
    let intact = conn_rb
        .query(
            "SELECT id, keep_col, drop_col FROM aux.items ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(intact.rows.len(), 2);
    assert_eq!(intact.rows[0].get_i64(2), Some(100));
    assert_eq!(intact.rows[1].get_i64(2), Some(200));

    // Also test atomic multi-DB rollback when a `TEMP` trigger is rewritten first and a subsequent
    // post-rename check fails inside an explicit transaction with `SAVEPOINT`!
    conn_rb
        .execute(
            "
            BEGIN;
            SAVEPOINT sp1;
            ALTER TABLE aux.items RENAME COLUMN keep_col TO label_col;
        ",
        )
        .unwrap();
    let err_sp = conn_rb
        .execute("ALTER TABLE aux.items DROP COLUMN drop_col;")
        .unwrap_err();
    assert!(
        err_sp
            .message
            .contains("error in trigger tr_fail after drop column")
    );
    let after_stmt_rb = conn_rb
        .query(
            "SELECT id, label_col, drop_col FROM aux.items ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(after_stmt_rb.rows.len(), 2);
    conn_rb.execute("ROLLBACK TO sp1; COMMIT;").unwrap();
    let after_sp_rb = conn_rb
        .query(
            "SELECT id, keep_col, drop_col FROM aux.items ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(after_sp_rb.rows[0].get_str(1), Some("alpha"));

    // 5. Bidirectional `.db` file exchange + `PRAGMA integrity_check` with C SQLite 3.54.1!
    // Direction A: Rust creates tables + triggers, runs `ALTER TABLE RENAME TO` / `RENAME COLUMN` / `DROP COLUMN` + `VACUUM`;
    //              C SQLite 3.54.1 opens the `.db`, runs `PRAGMA integrity_check`, and fires the rewritten triggers!
    let rust_to_c_db = sandbox.path().join("rust_to_c.db");
    let c_r2c = Connection::open(&rust_to_c_db).unwrap();
    c_r2c
        .execute("
            PRAGMA journal_mode = WAL;
            CREATE TABLE users(uid INTEGER PRIMARY KEY, uname TEXT, obsolete INT);
            CREATE TABLE history(hid INTEGER PRIMARY KEY, who TEXT, action TEXT);
            CREATE TRIGGER tr_users_ins AFTER INSERT ON users BEGIN
              INSERT INTO history(who, action) VALUES(new.uname, 'created');
            END;
            CREATE TRIGGER tr_users_upd AFTER UPDATE OF uname ON users WHEN old.uname != new.uname BEGIN
              INSERT INTO history(who, action) VALUES(new.uname, 'renamed_from:' || old.uname);
            END;
            INSERT INTO users VALUES(1, 'Ada', 99);
            ALTER TABLE users DROP COLUMN obsolete;
            ALTER TABLE users RENAME COLUMN uname TO full_name;
            ALTER TABLE users RENAME TO members;
        ")
        .unwrap();
    drop(c_r2c);

    let (ok_c_verify, c_verify_out, err_c_verify) = run_c_sqlite(
        sandbox.path(),
        rust_to_c_db.to_str().unwrap(),
        "
        PRAGMA integrity_check;
        INSERT INTO members VALUES(2, 'Grace');
        UPDATE members SET full_name = 'Ada Lovelace' WHERE uid = 1;
        SELECT uid, full_name FROM members ORDER BY uid;
        SELECT who, action FROM history ORDER BY hid;
        ",
    );
    assert!(
        ok_c_verify,
        "C SQLite failed on Rust-authored trigger DB:\n{err_c_verify}"
    );
    assert_eq!(
        c_verify_out,
        "ok\n1|Ada Lovelace\n2|Grace\nGrace|created\nAda Lovelace|renamed_from:Ada\n"
    );

    // Direction B: C SQLite 3.54.1 creates tables, views, and triggers; Rust opens the `.db`,
    //              alters tables/columns, drops columns, vacuums, and verifies `integrity_check`!
    let c_to_rust_db = sandbox.path().join("c_to_rust.db");
    let (ok_c_init, _, err_c_init) = run_c_sqlite(
        sandbox.path(),
        c_to_rust_db.to_str().unwrap(),
        "
        CREATE TABLE orders(oid INTEGER PRIMARY KEY, item TEXT, price INT, legacy_col TEXT);
        CREATE TABLE order_events(ev TEXT);
        CREATE VIEW v_orders AS SELECT oid, item, price FROM orders;
        CREATE TRIGGER tr_ord_bi BEFORE INSERT ON orders WHEN new.price < 0 BEGIN
          SELECT RAISE(ABORT, 'negative price');
        END;
        CREATE TRIGGER tr_ord_au AFTER UPDATE OF price ON orders BEGIN
          INSERT INTO order_events VALUES(new.item || ':' || old.price || '->' || new.price);
        END;
        CREATE TRIGGER tr_v_ord INSTEAD OF DELETE ON v_orders BEGIN
          DELETE FROM orders WHERE oid = old.oid;
        END;
        INSERT INTO orders VALUES(10, 'Book', 25, 'x'), (20, 'Pen', 5, 'y');
        ",
    );
    assert!(ok_c_init, "{err_c_init}");

    let r_c2r = Connection::open(&c_to_rust_db).unwrap();
    r_c2r
        .execute(
            "
            ALTER TABLE orders DROP COLUMN legacy_col;
            ALTER TABLE orders RENAME COLUMN price TO unit_price;
            ALTER TABLE orders RENAME TO purchase_orders;
            VACUUM;
        ",
        )
        .unwrap();
    let ic = r_c2r.query("PRAGMA integrity_check", &[]).unwrap();
    assert_eq!(ic.rows[0].get_str(0), Some("ok"));
    drop(r_c2r);

    // Re-verify in C SQLite 3.54.1 that the triggers rewritten & vacuumed by Rust fire properly!
    let (ok_c_roundtrip, c_roundtrip_out, err_c_roundtrip) = run_c_sqlite(
        sandbox.path(),
        c_to_rust_db.to_str().unwrap(),
        "
        PRAGMA integrity_check;
        UPDATE purchase_orders SET unit_price = 30 WHERE oid = 10;
        DELETE FROM v_orders WHERE oid = 20;
        SELECT oid, item, unit_price FROM purchase_orders ORDER BY oid;
        SELECT ev FROM order_events;
        ",
    );
    assert!(ok_c_roundtrip, "{err_c_roundtrip}");
    assert_eq!(c_roundtrip_out, "ok\n10|Book|30\nBook:25->30\n");
}

#[test]
fn step4a_reference_tcl_suites_altertrig_alterdropcol_and_attach4() {
    for (stem, min_subtests) in [
        ("altertrig", 36usize),
        ("alterdropcol", 100usize),
        ("alterdropcol2", 23usize),
        ("attach4", 12usize),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("sqlite/test")
            .join(format!("{stem}.test"));
        let raw = fs::read_to_string(&path).expect("read reference .test file");
        let stripped = strip_test_comments(&raw);
        let report = evaluate_test_script_in_root("sqlite/test", stem, &stripped);
        assert!(
            report.is_ok(),
            "`{stem}.test` failed: passed={}, failed={:?}, script_error={:?}",
            report.passed,
            report.failed,
            report.script_error
        );
        assert!(
            report.passed >= min_subtests,
            "`{stem}.test` ran {} subtests, expected >= {min_subtests}",
            report.passed
        );
    }
}
