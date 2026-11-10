//! Phase 4 — Step 4-B Differential Integration Suite:
//! `TRIGGER` Execution Engine (`BEFORE` / `AFTER` / `INSTEAD OF`), `OLD.*` & `NEW.*` Scoping,
//! Exact Firing Order & `SQLITE_DBCONFIG_ENABLE_TRIGGER`, Counter Isolation (`changes()`,
//! `total_changes()`, `last_insert_rowid()`), `PRAGMA recursive_triggers` &
//! `SQLITE_MAX_TRIGGER_DEPTH = 1000`, Writable `VIEW`s, Conflict Policy Inheritance, `RAISE(...)`,
//! Group 38 `FAIL` + FK `sqlite3VdbeHalt` Matrix, and Group 58 Shared Row-Mutation Pipeline
//! against C SQLite 3.54.1.
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters::types::extended;
use sqliters::{Connection, ErrorCode, SQLITE_MAX_TRIGGER_DEPTH, Value};
use sqliters_tcltest::{evaluate_test_script_in_root, strip_test_comments};

static NEXT_PHASE4B_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct Phase4BSandbox {
    dir: PathBuf,
}

impl Phase4BSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_PHASE4B_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_phase4b_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create phase4b temp sandbox");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Phase4BSandbox {
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

fn query_pipe(conn: &Connection, sql: &str) -> String {
    let res = conn
        .query(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"));
    format_rows_pipe(&res.rows)
}

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
fn step4b_before_after_firing_order_old_new_rowid_affinities_generated_and_counters() {
    let sandbox = Phase4BSandbox::new("core_exec");

    // 1. Firing order (`TEMP` FIFO + permanent LIFO on `main` table; `TEMP` LIFO on `TEMP` table),
    //    `UPDATE OF` syntactic column overlap (including `SET a = a` unchanged value),
    //    `DELETE FROM tbl` without `WHERE` truncate bypass, `INSERT ... SELECT *` xfer bypass,
    //    `OLD`/`NEW` affinities (`REAL` typeof, `VIRTUAL` & `STORED` generated columns),
    //    `NEW.rowid == -1` in `BEFORE INSERT` vs allocated rowid in `AFTER INSERT`,
    //    and `changes()` / `total_changes()` / `last_insert_rowid()` isolation!
    let script = "
        CREATE TABLE log(seq INTEGER PRIMARY KEY AUTOINCREMENT, tag TEXT);
        CREATE TABLE items(
            id INTEGER PRIMARY KEY,
            label TEXT,
            qty INTEGER,
            price REAL,
            v_total REAL GENERATED ALWAYS AS (qty * price) VIRTUAL,
            s_tag TEXT GENERATED ALWAYS AS (label || ':' || qty) STORED
        );
        CREATE TRIGGER main_bi_1 BEFORE INSERT ON items BEGIN
            INSERT INTO log(tag) VALUES(
                'main_bi_1:' || new.rowid || ':' || new.id || ':' ||
                typeof(new.label) || ':' || typeof(new.qty) || ':' ||
                new.price || ':' || typeof(new.price) || ':' ||
                new.v_total || ':' || new.s_tag
            );
        END;
        CREATE TRIGGER main_bi_2 BEFORE INSERT ON items BEGIN
            INSERT INTO log(tag) VALUES('main_bi_2');
        END;
        CREATE TEMP TRIGGER temp_bi_1 BEFORE INSERT ON main.items BEGIN
            INSERT INTO log(tag) VALUES('temp_bi_1');
        END;
        CREATE TEMP TRIGGER temp_bi_2 BEFORE INSERT ON main.items BEGIN
            INSERT INTO log(tag) VALUES('temp_bi_2');
        END;
        CREATE TRIGGER main_ai AFTER INSERT ON items BEGIN
            INSERT INTO log(tag) VALUES(
                'main_ai:' || new.rowid || ':' || new.id || ':' || new.v_total || ':' || new.s_tag ||
                ':inner_pre=' || changes() || ',' || last_insert_rowid()
            );
            INSERT INTO log(tag) VALUES(
                'main_ai_post:' || changes() || ',' || total_changes()
            );
        END;
        CREATE TRIGGER main_bu_of_qty BEFORE UPDATE OF qty ON items BEGIN
            INSERT INTO log(tag) VALUES(
                'bu_qty:' || old.qty || '->' || new.qty || ':' || old.v_total || '->' || new.v_total
            );
        END;
        CREATE TRIGGER main_au_of_price AFTER UPDATE OF price ON items BEGIN
            INSERT INTO log(tag) VALUES(
                'au_price:' || old.price || '(' || typeof(old.price) || ')->' ||
                new.price || '(' || typeof(new.price) || '):' || new.v_total
            );
        END;
        CREATE TRIGGER main_bd BEFORE DELETE ON items BEGIN
            INSERT INTO log(tag) VALUES('bd:' || old.id || ':' || old.s_tag);
        END;
        CREATE TRIGGER main_ad AFTER DELETE ON items BEGIN
            INSERT INTO log(tag) VALUES('ad:' || old.id || ':' || typeof(old.price));
        END;

        INSERT INTO items(label, qty, price) VALUES(100, '3', '5');
        INSERT INTO log(tag) VALUES('after_ins1:chg=' || changes() || ',lid=' || last_insert_rowid() || ',tot=' || total_changes());

        INSERT INTO items(id, label, qty, price) VALUES(42, 'pen', 2, 10);
        INSERT INTO log(tag) VALUES('after_ins2:chg=' || changes() || ',lid=' || last_insert_rowid());

        -- Syntactic `UPDATE OF qty` fires even when `qty = qty` leaves value identical!
        UPDATE items SET qty = qty WHERE id = 1;
        -- `SET label = ...` does NOT overlap `OF qty` or `OF price`!
        UPDATE items SET label = 'book' WHERE id = 1;
        -- `SET price = ...` fires `au_price`!
        UPDATE items SET price = '7' WHERE id = 1;

        -- Unconditional `DELETE FROM items` must NOT take table-clear shortcut when triggers exist!
        DELETE FROM items;
        INSERT INTO log(tag) VALUES('after_del_all:chg=' || changes() || ',lid=' || last_insert_rowid());

        -- `TEMP` table with multiple `TEMP` triggers fires in LIFO order:
        CREATE TEMP TABLE tt(x);
        CREATE TEMP TRIGGER tt_1 BEFORE INSERT ON tt BEGIN INSERT INTO log(tag) VALUES('tt_1'); END;
        CREATE TEMP TRIGGER tt_2 BEFORE INSERT ON tt BEGIN INSERT INTO log(tag) VALUES('tt_2'); END;
        INSERT INTO tt VALUES(9);

        -- `WITHOUT ROWID` table with `BEFORE` & `AFTER` triggers:
        CREATE TABLE wor(k TEXT PRIMARY KEY, v INT) WITHOUT ROWID;
        CREATE TRIGGER wor_bu BEFORE UPDATE ON wor BEGIN
            INSERT INTO log(tag) VALUES('wor_bu:' || old.k || ':' || old.v || '->' || new.k || ':' || new.v);
        END;
        INSERT INTO wor VALUES('k1', 10);
        UPDATE wor SET k = 'k2', v = 20 WHERE k = 'k1';

        -- `INSERT ... SELECT *` transfer optimization must be bypassed when destination has triggers:
        CREATE TABLE xfer_src(a INTEGER PRIMARY KEY, b TEXT);
        CREATE TABLE xfer_dst(a INTEGER PRIMARY KEY, b TEXT);
        INSERT INTO xfer_src VALUES(1, 's1'), (2, 's2');
        CREATE TRIGGER xfer_dst_ai AFTER INSERT ON xfer_dst BEGIN
            INSERT INTO log(tag) VALUES('xfer_ai:' || new.a || ':' || new.b);
        END;
        INSERT INTO xfer_dst SELECT * FROM xfer_src;
    ";
    let check_sql = "SELECT seq, tag FROM log ORDER BY seq;";

    let (c_ok, c_out, c_err) = run_c_sqlite(
        sandbox.path(),
        ":memory:",
        &format!("{script}\n{check_sql}"),
    );
    assert!(c_ok, "C SQLite failed:\n{c_err}");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute(script).unwrap();
    let rust_out = query_pipe(&conn, check_sql);
    assert_eq!(rust_out, c_out);

    // 2. `SQLITE_DBCONFIG_ENABLE_TRIGGER` (`Connection::db_config_enable_trigger`):
    //    `enable_trigger = false` disables permanent `main` triggers while keeping `TEMP` triggers active!
    assert!(conn.db_config_enable_trigger(None));
    assert!(!conn.db_config_enable_trigger(Some(false)));
    assert!(!conn.db_config_enable_trigger(None));
    conn.execute(
        "DELETE FROM log; INSERT INTO items(id, label, qty, price) VALUES(77, 'cfg', 1, 2);",
    )
    .unwrap();
    let cfg_tags = query_pipe(&conn, "SELECT tag FROM log ORDER BY seq;");
    assert_eq!(
        cfg_tags, "temp_bi_1\ntemp_bi_2\n",
        "only TEMP triggers should fire when db_config_enable_trigger is false"
    );
    assert!(conn.db_config_enable_trigger(Some(true)));

    // 3. Target-row disappearance & unmodified-column reload (`triggerC-7.*` & `triggerC-10.1`):
    let disappearance_sql = "
        CREATE TABLE ev(msg TEXT);
        CREATE TABLE target_u(id INTEGER PRIMARY KEY, a TEXT, cnt INT DEFAULT 0);
        INSERT INTO target_u VALUES(1, 'init', 0), (2, 'gone', 0);

        CREATE TRIGGER tu_bu BEFORE UPDATE OF a ON target_u BEGIN
            UPDATE target_u SET cnt = cnt + 1 WHERE id = old.id AND old.id = 1;
            DELETE FROM target_u WHERE id = old.id AND old.id = 2;
        END;
        CREATE TRIGGER tu_au AFTER UPDATE ON target_u BEGIN
            INSERT INTO ev VALUES('au:' || old.id || ':' || new.a || ':' || new.cnt);
        END;

        UPDATE target_u SET a = 'changed';

        CREATE TABLE target_d(id INTEGER PRIMARY KEY, v TEXT);
        INSERT INTO target_d VALUES(10, 'x');
        CREATE TRIGGER td_bd BEFORE DELETE ON target_d BEGIN
            DELETE FROM target_d WHERE id = old.id;
        END;
        CREATE TRIGGER td_ad AFTER DELETE ON target_d BEGIN
            INSERT INTO ev VALUES('ad:' || old.id);
        END;
        DELETE FROM target_d WHERE id = 10;
    ";
    let dis_query = "
        SELECT 'u', id, a, cnt FROM target_u ORDER BY id;
        SELECT 'd', count(*) FROM target_d;
        SELECT 'ev', msg FROM ev;
    ";
    let (c_ok2, c_out2, c_err2) = run_c_sqlite(
        sandbox.path(),
        ":memory:",
        &format!("{disappearance_sql}\n{dis_query}"),
    );
    assert!(c_ok2, "{c_err2}");
    let conn2 = Connection::open_in_memory().unwrap();
    conn2.execute(disappearance_sql).unwrap();
    let mut r_out2 = String::new();
    r_out2.push_str(&query_pipe(
        &conn2,
        "SELECT 'u', id, a, cnt FROM target_u ORDER BY id;",
    ));
    r_out2.push_str(&query_pipe(&conn2, "SELECT 'd', count(*) FROM target_d;"));
    r_out2.push_str(&query_pipe(&conn2, "SELECT 'ev', msg FROM ev;"));
    assert_eq!(r_out2, c_out2);

    // 4. Implicit `rowid` errors on `WITHOUT ROWID` triggers and `datatype mismatch` before `BEFORE`:
    assert_differential_error(
        sandbox.path(),
        "without_rowid_new_rowid",
        "CREATE TABLE wr(k INT PRIMARY KEY, v TEXT) WITHOUT ROWID;
         CREATE TRIGGER wr_bi BEFORE INSERT ON wr BEGIN SELECT new.rowid; END;",
        "INSERT INTO wr VALUES(1, 'a');",
    );
    assert_differential_error(
        sandbox.path(),
        "without_rowid_old_rowid",
        "CREATE TABLE wr(k INT PRIMARY KEY, v TEXT) WITHOUT ROWID;
         INSERT INTO wr VALUES(1, 'a');
         CREATE TRIGGER wr_bd BEFORE DELETE ON wr BEGIN SELECT old.rowid; END;",
        "DELETE FROM wr WHERE k = 1;",
    );
    assert_differential_error(
        sandbox.path(),
        "ipk_mismatch_before_insert",
        "CREATE TABLE ipk_t(id INTEGER PRIMARY KEY, v TEXT);
         CREATE TRIGGER ipk_bi BEFORE INSERT ON ipk_t BEGIN SELECT RAISE(IGNORE); END;",
        "INSERT INTO ipk_t VALUES('not_an_int', 'a');",
    );
    assert_differential_error(
        sandbox.path(),
        "ipk_null_mismatch_before_update",
        "CREATE TABLE ipk_u(id INTEGER PRIMARY KEY, v TEXT);
         INSERT INTO ipk_u VALUES(1, 'a');
         CREATE TRIGGER ipk_bu BEFORE UPDATE ON ipk_u BEGIN SELECT RAISE(IGNORE); END;",
        "UPDATE ipk_u SET id = NULL;",
    );
}

#[test]
fn step4b_writable_views_instead_of_triggers_returning_and_upsert_rejection() {
    let sandbox = Phase4BSandbox::new("instead_of_views");

    let sql = "
        CREATE TABLE base_emp(id INTEGER PRIMARY KEY, name TEXT, salary INT DEFAULT 100);
        CREATE TABLE audit(entry TEXT);
        CREATE VIEW v_emp AS SELECT id, name, salary FROM base_emp;

        CREATE TRIGGER v_emp_ins INSTEAD OF INSERT ON v_emp BEGIN
            INSERT INTO base_emp(id, name, salary)
            VALUES(coalesce(new.id, 900), coalesce(new.name, 'anon'), coalesce(new.salary, 50));
            INSERT INTO audit VALUES('ins:' || coalesce(new.id, -1) || ':' || coalesce(new.name, 'NULL'));
        END;

        CREATE TRIGGER v_emp_upd INSTEAD OF UPDATE OF name, salary ON v_emp BEGIN
            UPDATE base_emp SET name = new.name, salary = new.salary WHERE id = old.id;
            INSERT INTO audit VALUES('upd:' || old.id || ':' || old.salary || '->' || new.salary);
        END;

        CREATE TRIGGER v_emp_del INSTEAD OF DELETE ON v_emp BEGIN
            DELETE FROM base_emp WHERE id = old.id;
            INSERT INTO audit VALUES('del:' || old.id || ':' || old.name);
        END;

        -- 1. `VALUES`, `SELECT`, and `DEFAULT VALUES` on view:
        INSERT INTO v_emp VALUES(1, 'Ada', 200), (2, 'Grace', 300);
        INSERT INTO audit VALUES('chg_after_ins=' || changes() || ',tot=' || total_changes());
        INSERT INTO v_emp DEFAULT VALUES;

        -- 2. Row-value `SET` and `UPDATE ... FROM` on view:
        CREATE TABLE bonus(emp_id INT, delta INT);
        INSERT INTO bonus VALUES(1, 25), (1, 50), (2, 10);
        UPDATE v_emp SET (name, salary) = (v_emp.name || '!', v_emp.salary + bonus.delta)
        FROM bonus WHERE v_emp.id = bonus.emp_id;
        INSERT INTO audit VALUES('chg_after_upd=' || changes());

        -- 3. `DELETE` on view:
        DELETE FROM v_emp WHERE id = 900;
        INSERT INTO audit VALUES('chg_after_del=' || changes());
    ";
    let ret_sql = "
        INSERT INTO v_emp VALUES(3, 'Linus', 400) RETURNING id, name, salary * 2;
        UPDATE v_emp SET salary = salary + 5 WHERE id = 3 RETURNING id, salary;
        DELETE FROM v_emp WHERE id = 3 RETURNING id, name;
        SELECT 'base', id, name, salary FROM base_emp ORDER BY id;
        SELECT 'audit', entry FROM audit;
    ";

    let (c_ok, c_out, c_err) =
        run_c_sqlite(sandbox.path(), ":memory:", &format!("{sql}\n{ret_sql}"));
    assert!(c_ok, "C SQLite failed:\n{c_err}");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute(sql).unwrap();
    let mut rust_out = String::new();
    rust_out.push_str(&query_pipe(
        &conn,
        "INSERT INTO v_emp VALUES(3, 'Linus', 400) RETURNING id, name, salary * 2;",
    ));
    assert_eq!(conn.changes(), 0);
    rust_out.push_str(&query_pipe(
        &conn,
        "UPDATE v_emp SET salary = salary + 5 WHERE id = 3 RETURNING id, salary;",
    ));
    assert_eq!(conn.changes(), 0);
    rust_out.push_str(&query_pipe(
        &conn,
        "DELETE FROM v_emp WHERE id = 3 RETURNING id, name;",
    ));
    assert_eq!(conn.changes(), 0);
    rust_out.push_str(&query_pipe(
        &conn,
        "SELECT 'base', id, name, salary FROM base_emp ORDER BY id;",
    ));
    rust_out.push_str(&query_pipe(&conn, "SELECT 'audit', entry FROM audit;"));
    assert_eq!(rust_out, c_out);

    // `TEMP` `INSTEAD OF` trigger on permanent `main` view writing only to `TEMP` table:
    conn.execute(
        "CREATE VIEW v_ro AS SELECT 1 AS x;
         CREATE TEMP TABLE temp_sink(v INT);
         CREATE TEMP TRIGGER tr_v_ro INSTEAD OF INSERT ON main.v_ro BEGIN
             INSERT INTO temp_sink VALUES(new.x * 10);
         END;
         INSERT INTO v_ro VALUES(7);",
    )
    .unwrap();
    assert_eq!(query_pipe(&conn, "SELECT v FROM temp_sink;"), "70\n");

    // `INSTEAD OF UPDATE OF a` does NOT make `UPDATE v SET b = 1` writable,
    // `UPSERT` on a view is rejected even when `INSTEAD OF INSERT` exists,
    // and `NEW.rowid` / `OLD.rowid` inside `INSTEAD OF` view trigger is rejected:
    assert_differential_error(
        sandbox.path(),
        "view_update_unhandled_column",
        "CREATE TABLE t(a, b);
         CREATE VIEW v AS SELECT a, b FROM t;
         CREATE TRIGGER tr_va INSTEAD OF UPDATE OF a ON v BEGIN UPDATE t SET a = new.a; END;",
        "UPDATE v SET b = 99;",
    );
    assert_differential_error(
        sandbox.path(),
        "cannot_upsert_view_with_instead_of",
        "CREATE TABLE t(a PRIMARY KEY, b);
         CREATE VIEW v AS SELECT a, b FROM t;
         CREATE TRIGGER tr_vi INSTEAD OF INSERT ON v BEGIN INSERT INTO t VALUES(new.a, new.b); END;",
        "INSERT INTO v VALUES(1, 'x') ON CONFLICT(a) DO NOTHING;",
    );
    assert_differential_error(
        sandbox.path(),
        "view_instead_of_new_rowid",
        "CREATE TABLE t(a, b);
         CREATE VIEW v AS SELECT a, b FROM t;
         CREATE TRIGGER tr_vi INSTEAD OF INSERT ON v BEGIN SELECT new.rowid; END;",
        "INSERT INTO v VALUES(1, 2);",
    );
}

#[test]
fn step4b_conflict_policy_inheritance_raise_semantics_and_group38_fail_fk_matrix() {
    let sandbox = Phase4BSandbox::new("conflict_fk_matrix");

    // 1. Conflict policy inheritance (`outer_orconf` overrides step `OR`, `OE_Default` preserves step `OR`)
    //    AND explicit `RAISE(...)` is NEVER overridden by `outer_orconf`, plus nested `RAISE(IGNORE)`!
    let inherit_sql = "
        CREATE TABLE u_sink(k INT PRIMARY KEY, src TEXT);
        CREATE TABLE driver(x INT);
        CREATE TRIGGER drv_bi BEFORE INSERT ON driver BEGIN
            INSERT OR ABORT INTO u_sink VALUES(new.x, 'from_trig');
        END;
        INSERT INTO u_sink VALUES(1, 'orig');
        -- Outer `OR IGNORE` overrides inner `INSERT OR ABORT`:
        INSERT OR IGNORE INTO driver VALUES(1), (2);
        -- Outer `OR REPLACE` overrides inner `INSERT OR ABORT`:
        INSERT OR REPLACE INTO driver VALUES(1);

        -- Nested `RAISE(IGNORE)` skips ONLY the inner mutation and resumes the outer trigger:
        CREATE TABLE trace(msg TEXT);
        CREATE TABLE outer_t(a INT);
        CREATE TABLE inner_t(b INT);
        CREATE TRIGGER inner_bi BEFORE INSERT ON inner_t WHEN new.b = 2 BEGIN
            INSERT INTO trace VALUES('inner_before_ignore');
            SELECT RAISE(IGNORE);
            INSERT INTO trace VALUES('unreachable_inner');
        END;
        CREATE TRIGGER outer_bi BEFORE INSERT ON outer_t BEGIN
            INSERT INTO trace VALUES('outer_step1');
            INSERT INTO inner_t VALUES(1);
            INSERT INTO inner_t VALUES(2);
            INSERT INTO inner_t VALUES(3);
            INSERT INTO trace VALUES('outer_step2');
        END;
        INSERT INTO outer_t VALUES(100);
    ";
    let inherit_q = "
        SELECT 'u_sink', k, src FROM u_sink ORDER BY k;
        SELECT 'driver', x FROM driver ORDER BY x;
        SELECT 'inner_t', b FROM inner_t ORDER BY b;
        SELECT 'trace', msg FROM trace;
    ";
    let (c_ok, c_out, c_err) = run_c_sqlite(
        sandbox.path(),
        ":memory:",
        &format!("{inherit_sql}\n{inherit_q}"),
    );
    assert!(c_ok, "{c_err}");
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(inherit_sql).unwrap();
    let mut r_out = String::new();
    r_out.push_str(&query_pipe(
        &conn,
        "SELECT 'u_sink', k, src FROM u_sink ORDER BY k;",
    ));
    r_out.push_str(&query_pipe(
        &conn,
        "SELECT 'driver', x FROM driver ORDER BY x;",
    ));
    r_out.push_str(&query_pipe(
        &conn,
        "SELECT 'inner_t', b FROM inner_t ORDER BY b;",
    ));
    r_out.push_str(&query_pipe(&conn, "SELECT 'trace', msg FROM trace;"));
    assert_eq!(r_out, c_out);

    // Explicit `RAISE(ABORT|FAIL|ROLLBACK, msg)` under outer `INSERT OR IGNORE` still raises
    // `SQLITE_CONSTRAINT_TRIGGER` (1811) with exact message, and `RAISE(IGNORE)` under `INSERT OR ABORT` ignores!
    let conn_raise = Connection::open_in_memory().unwrap();
    conn_raise
        .execute(
            "CREATE TABLE t(a INT);
             CREATE TRIGGER t_bi BEFORE INSERT ON t BEGIN
                 SELECT CASE
                     WHEN new.a = 1 THEN RAISE(ABORT, 'explicit abort msg')
                     WHEN new.a = 2 THEN RAISE(FAIL, 'explicit fail msg')
                     WHEN new.a = 3 THEN RAISE(ROLLBACK, 'explicit rollback msg')
                     WHEN new.a = 4 THEN RAISE(IGNORE)
                 END;
             END;",
        )
        .unwrap();
    let e_abort = conn_raise
        .execute("INSERT OR IGNORE INTO t VALUES(1);")
        .unwrap_err();
    assert_eq!(e_abort.code, ErrorCode::Constraint);
    assert_eq!(e_abort.extended_code, extended::SQLITE_CONSTRAINT_TRIGGER);
    assert_eq!(e_abort.message, "explicit abort msg");

    let e_fail = conn_raise
        .execute("INSERT OR IGNORE INTO t VALUES(2);")
        .unwrap_err();
    assert_eq!(e_fail.code, ErrorCode::Constraint);
    assert_eq!(e_fail.extended_code, extended::SQLITE_CONSTRAINT_TRIGGER);
    assert_eq!(e_fail.message, "explicit fail msg");

    conn_raise
        .execute("BEGIN; INSERT INTO t VALUES(99);")
        .unwrap();
    assert!(!conn_raise.is_autocommit());
    let e_rb = conn_raise
        .execute("INSERT OR IGNORE INTO t VALUES(3);")
        .unwrap_err();
    assert_eq!(e_rb.code, ErrorCode::Constraint);
    assert_eq!(e_rb.extended_code, extended::SQLITE_CONSTRAINT_TRIGGER);
    assert_eq!(e_rb.message, "explicit rollback msg");
    assert!(
        conn_raise.is_autocommit(),
        "RAISE(ROLLBACK) must exit transaction"
    );
    assert_eq!(query_pipe(&conn_raise, "SELECT * FROM t;"), "");

    conn_raise
        .execute("INSERT OR ABORT INTO t VALUES(4), (5);")
        .unwrap();
    assert_eq!(query_pipe(&conn_raise, "SELECT a FROM t;"), "5\n");

    // 2. Full Group 38 `RAISE(FAIL)` / `OR FAIL` + FK `sqlite3VdbeHalt` Matrix on disk!
    // Case A: Autocommit + NO FK violation -> prior rows COMMITTED to disk, caller gets FAIL error!
    let c_ac_nofk = sandbox.path().join("c_ac_nofk.db");
    let r_ac_nofk = sandbox.path().join("r_ac_nofk.db");
    let ac_nofk_setup = "
        PRAGMA foreign_keys = ON;
        CREATE TABLE parent(id INT PRIMARY KEY);
        INSERT INTO parent VALUES(10), (20), (30);
        CREATE TABLE child(id INT PRIMARY KEY, pid INT REFERENCES parent(id));
        CREATE TABLE side(msg TEXT);
        CREATE TRIGGER ch_ai AFTER INSERT ON child BEGIN
            INSERT INTO side VALUES('seen:' || new.id);
            SELECT CASE WHEN new.id = 3 THEN RAISE(FAIL, 'stop_at_3') END;
        END;
    ";
    let ac_nofk_dml =
        "PRAGMA foreign_keys = ON; INSERT INTO child VALUES(1, 10), (2, 20), (3, 30), (4, 10);";
    let _ = run_c_sqlite(sandbox.path(), c_ac_nofk.to_str().unwrap(), ac_nofk_setup);
    let (c_ok_a, _, c_err_a) =
        run_c_sqlite(sandbox.path(), c_ac_nofk.to_str().unwrap(), ac_nofk_dml);
    assert!(!c_ok_a);
    let (_, c_state_a, _) = run_c_sqlite(
        sandbox.path(),
        c_ac_nofk.to_str().unwrap(),
        "SELECT id, pid FROM child ORDER BY id; SELECT msg FROM side;",
    );

    let rc_a = Connection::open(&r_ac_nofk).unwrap();
    rc_a.execute(ac_nofk_setup).unwrap();
    let r_err_a = rc_a.execute(ac_nofk_dml).unwrap_err();
    assert_eq!(r_err_a.extended_code, extended::SQLITE_CONSTRAINT_TRIGGER);
    assert_eq!(r_err_a.message, extract_c_sqlite_error_message(&c_err_a));
    assert!(rc_a.is_autocommit());
    drop(rc_a);
    // Verify Rust's persisted `.db` file in C SQLite 3.54.1!
    let (ok_verify_a, r_state_a, err_verify_a) = run_c_sqlite(
        sandbox.path(),
        r_ac_nofk.to_str().unwrap(),
        "PRAGMA integrity_check; SELECT id, pid FROM child ORDER BY id; SELECT msg FROM side;",
    );
    assert!(ok_verify_a, "{err_verify_a}");
    assert_eq!(r_state_a, format!("ok\n{c_state_a}"));

    // Case B: Explicit `BEGIN` + NO FK violation -> prior rows preserved in open tx!
    let rc_b = Connection::open_in_memory().unwrap();
    rc_b.execute(ac_nofk_setup).unwrap();
    rc_b.execute("BEGIN; INSERT INTO child VALUES(0, 10);")
        .unwrap();
    let r_err_b = rc_b.execute(ac_nofk_dml).unwrap_err();
    assert_eq!(r_err_b.message, "stop_at_3");
    assert!(!rc_b.is_autocommit());
    rc_b.execute("COMMIT;").unwrap();
    assert_eq!(
        query_pipe(&rc_b, "SELECT id FROM child ORDER BY id;"),
        "0\n1\n2\n3\n"
    );

    // Case C: Immediate FK violation + `RAISE(FAIL)` / `OR FAIL` (both autocommit & explicit `BEGIN`)
    //         -> `OE_Fail` escalates to `OE_Abort` (`FOREIGN KEY constraint failed`) and rolls back statement!
    let imm_fk_setup = "
        PRAGMA foreign_keys = ON;
        CREATE TABLE p(id INT PRIMARY KEY);
        INSERT INTO p VALUES(1);
        CREATE TABLE c(id INT PRIMARY KEY, pid INT REFERENCES p(id));
        CREATE TRIGGER c_ai AFTER INSERT ON c BEGIN
            SELECT CASE WHEN new.id = 2 THEN RAISE(FAIL, 'trig_fail') END;
        END;
    ";
    let imm_fk_dml = "INSERT INTO c VALUES(1, 1), (2, 999);";
    assert_differential_error(
        sandbox.path(),
        "imm_fk_escalates_fail_ac",
        imm_fk_setup,
        imm_fk_dml,
    );

    let rc_c = Connection::open_in_memory().unwrap();
    rc_c.execute(imm_fk_setup).unwrap();
    rc_c.execute("BEGIN; INSERT INTO c VALUES(10, 1);").unwrap();
    let r_err_c = rc_c.execute(imm_fk_dml).unwrap_err();
    assert_eq!(
        r_err_c.extended_code,
        extended::SQLITE_CONSTRAINT_FOREIGNKEY
    );
    assert_eq!(r_err_c.message, "FOREIGN KEY constraint failed");
    assert!(!rc_c.is_autocommit());
    assert_eq!(
        query_pipe(&rc_c, "SELECT id FROM c ORDER BY id;"),
        "10\n",
        "statement savepoint must roll back row (1, 1) while keeping prior tx row (10, 1)"
    );
    rc_c.execute("COMMIT;").unwrap();

    // Case D: Deferred FK violation + `RAISE(FAIL)` in autocommit vs explicit `BEGIN`!
    let def_fk_setup = "
        PRAGMA foreign_keys = ON;
        CREATE TABLE p(id INT PRIMARY KEY);
        INSERT INTO p VALUES(1);
        CREATE TABLE c(id INT PRIMARY KEY, pid INT REFERENCES p(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER c_ai AFTER INSERT ON c BEGIN
            SELECT CASE WHEN new.id = 2 THEN RAISE(FAIL, 'def_trig_fail') END;
        END;
    ";
    let def_fk_dml = "PRAGMA foreign_keys = ON; INSERT INTO c VALUES(1, 999), (2, 1);";
    let c_ac_deffk = sandbox.path().join("c_ac_deffk.db");
    let r_ac_deffk = sandbox.path().join("r_ac_deffk.db");
    let _ = run_c_sqlite(sandbox.path(), c_ac_deffk.to_str().unwrap(), def_fk_setup);
    let (c_ok_d, _, c_err_d) =
        run_c_sqlite(sandbox.path(), c_ac_deffk.to_str().unwrap(), def_fk_dml);
    assert!(!c_ok_d);
    let (_, c_state_d, _) = run_c_sqlite(
        sandbox.path(),
        c_ac_deffk.to_str().unwrap(),
        "SELECT count(*) FROM c;",
    );
    assert_eq!(c_state_d, "0\n");

    let rc_d = Connection::open(&r_ac_deffk).unwrap();
    rc_d.execute(def_fk_setup).unwrap();
    let r_err_d = rc_d
        .execute("INSERT INTO c VALUES(1, 999), (2, 1);")
        .unwrap_err();
    assert_eq!(r_err_d.message, extract_c_sqlite_error_message(&c_err_d));
    assert!(rc_d.is_autocommit());
    drop(rc_d);
    let (ok_verify_d, r_state_d, _) = run_c_sqlite(
        sandbox.path(),
        r_ac_deffk.to_str().unwrap(),
        "PRAGMA integrity_check; SELECT count(*) FROM c;",
    );
    assert!(ok_verify_d);
    assert_eq!(r_state_d, "ok\n0\n");

    // In explicit `BEGIN`, deferred FK violation does NOT escalate at statement end; `COMMIT` catches it!
    let rc_d_tx = Connection::open_in_memory().unwrap();
    rc_d_tx.execute(def_fk_setup).unwrap();
    rc_d_tx.execute("BEGIN;").unwrap();
    let e_d_stmt = rc_d_tx
        .execute("INSERT INTO c VALUES(1, 999), (2, 1);")
        .unwrap_err();
    assert_eq!(e_d_stmt.message, "def_trig_fail");
    assert!(!rc_d_tx.is_autocommit());
    assert_eq!(
        query_pipe(&rc_d_tx, "SELECT id FROM c ORDER BY id;"),
        "1\n2\n"
    );
    let e_d_commit = rc_d_tx.execute("COMMIT;").unwrap_err();
    assert_eq!(
        e_d_commit.extended_code,
        extended::SQLITE_CONSTRAINT_FOREIGNKEY
    );
    assert_eq!(e_d_commit.message, "FOREIGN KEY constraint failed");
    rc_d_tx.execute("ROLLBACK;").unwrap();
}

#[test]
fn step4b_recursive_triggers_pragma_and_depth_1000_stack_safety() {
    let sandbox = Phase4BSandbox::new("recursion_1000");

    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(SQLITE_MAX_TRIGGER_DEPTH, 1000);
    assert!(!conn.recursive_triggers(None));
    assert_eq!(query_pipe(&conn, "PRAGMA recursive_triggers;"), "0\n");

    // 1. With `recursive_triggers = OFF` (default), Trigger A on `t_off` inserting into `t_off`
    //    fires Trigger B on `t_off`, while blocking re-entry into Trigger A!
    let off_sql = "
        PRAGMA recursive_triggers = OFF;
        CREATE TABLE log_off(entry TEXT);
        CREATE TABLE t_off(x INT);
        CREATE TRIGGER tr_a AFTER INSERT ON t_off WHEN new.x = 1 BEGIN
            INSERT INTO log_off VALUES('enter_A:' || new.x);
            INSERT INTO t_off VALUES(1);
        END;
        CREATE TRIGGER tr_b AFTER INSERT ON t_off BEGIN
            INSERT INTO log_off VALUES('enter_B:' || new.x);
        END;
        INSERT INTO t_off VALUES(1);
    ";
    let (c_ok, c_out, c_err) = run_c_sqlite(
        sandbox.path(),
        ":memory:",
        &format!("{off_sql}\nSELECT count(*) FROM t_off;\nSELECT entry FROM log_off;"),
    );
    assert!(c_ok, "{c_err}");
    conn.execute(off_sql).unwrap();
    let r_out = format!(
        "{}{}",
        query_pipe(&conn, "SELECT count(*) FROM t_off;"),
        query_pipe(&conn, "SELECT entry FROM log_off;")
    );
    assert_eq!(r_out, c_out);

    // 2. Setter / getter round-trip (`ON`/`OFF`/`1`/`0`/`true`/`false`):
    conn.execute("PRAGMA recursive_triggers = ON;").unwrap();
    assert!(conn.recursive_triggers(None));
    assert_eq!(query_pipe(&conn, "PRAGMA recursive_triggers;"), "1\n");
    conn.execute("PRAGMA recursive_triggers = false;").unwrap();
    assert!(!conn.recursive_triggers(None));
    assert!(conn.recursive_triggers(Some(true)));
    assert!(conn.recursive_triggers(None));

    // 3. Exact 1000-level recursion (`SQLITE_MAX_TRIGGER_DEPTH = 1000`) without overflowing thread stack,
    //    plus differential verification of exceeding depth 1000 (`triggers nested too deep`)!
    let depth_setup = "
        PRAGMA recursive_triggers = ON;
        CREATE TABLE t_depth(n INT);
        CREATE TRIGGER tr_depth AFTER INSERT ON t_depth WHEN new.n < 2000 BEGIN
            INSERT INTO t_depth VALUES(new.n + 1);
        END;
    ";
    let (c_ok_1000, c_out_1000, c_err_1000) = run_c_sqlite(
        sandbox.path(),
        ":memory:",
        &format!(
            "{depth_setup}\nINSERT INTO t_depth VALUES(1001);\nSELECT count(*), min(n), max(n) FROM t_depth;"
        ),
    );
    assert!(c_ok_1000, "{c_err_1000}");
    assert_eq!(c_out_1000, "1000|1001|2000\n");

    conn.execute(depth_setup).unwrap();
    conn.execute("INSERT INTO t_depth VALUES(1001);").unwrap();
    assert_eq!(
        query_pipe(&conn, "SELECT count(*), min(n), max(n) FROM t_depth;"),
        c_out_1000
    );

    // Exceeding depth 1000 (`VALUES(1000)` -> 1001 trigger invocations) fails cleanly and rolls back!
    let (c_ok_over, _, c_err_over) = run_c_sqlite(
        sandbox.path(),
        ":memory:",
        &format!("{depth_setup}\nINSERT INTO t_depth VALUES(1000);"),
    );
    assert!(!c_ok_over);
    let expected_depth_msg = extract_c_sqlite_error_message(&c_err_over);
    assert_eq!(expected_depth_msg, "triggers nested too deep");

    conn.execute("DELETE FROM t_depth;").unwrap();
    let r_over_err = conn
        .execute("INSERT INTO t_depth VALUES(1000);")
        .unwrap_err();
    assert_eq!(r_over_err.code, ErrorCode::Error);
    assert_eq!(r_over_err.message, expected_depth_msg);
    assert_eq!(
        query_pipe(&conn, "SELECT count(*) FROM t_depth;"),
        "0\n",
        "exceeding trigger recursion depth must roll back all partial inserts"
    );
}

#[test]
fn step4b_shared_mutation_pipeline_fk_actions_or_replace_recheck_and_upsert() {
    let sandbox = Phase4BSandbox::new("shared_pipeline");

    // 1. FK `CASCADE` / `SET NULL` / `SET DEFAULT` firing child `BEFORE`/`AFTER` triggers even when
    //    `recursive_triggers = OFF`, `OR REPLACE` firing `DELETE` triggers iff `recursive_triggers = ON`,
    //    and `UPSERT` (`DO UPDATE` & `DO NOTHING`) exact trigger sequence!
    let pipeline_sql = "
        PRAGMA foreign_keys = ON;
        PRAGMA recursive_triggers = OFF;
        CREATE TABLE ev(seq INTEGER PRIMARY KEY AUTOINCREMENT, msg TEXT);

        CREATE TABLE dept(id INT PRIMARY KEY, code TEXT UNIQUE);
        CREATE TABLE emp_cas(id INT PRIMARY KEY, did INT REFERENCES dept(id) ON DELETE CASCADE ON UPDATE CASCADE);
        CREATE TABLE emp_sn(id INT PRIMARY KEY, did INT REFERENCES dept(id) ON DELETE SET NULL ON UPDATE SET NULL);
        CREATE TABLE emp_sd(id INT PRIMARY KEY, did INT DEFAULT 99 REFERENCES dept(id) ON DELETE SET DEFAULT ON UPDATE SET DEFAULT);

        CREATE TRIGGER cas_bd BEFORE DELETE ON emp_cas BEGIN INSERT INTO ev(msg) VALUES('cas_bd:' || old.id); END;
        CREATE TRIGGER cas_ad AFTER DELETE ON emp_cas BEGIN INSERT INTO ev(msg) VALUES('cas_ad:' || old.id); END;
        CREATE TRIGGER cas_bu BEFORE UPDATE OF did ON emp_cas BEGIN INSERT INTO ev(msg) VALUES('cas_bu:' || old.did || '->' || new.did); END;
        CREATE TRIGGER sn_au AFTER UPDATE OF did ON emp_sn BEGIN INSERT INTO ev(msg) VALUES('sn_au:' || old.did || '->' || coalesce(new.did, -1)); END;
        CREATE TRIGGER sd_au AFTER UPDATE OF did ON emp_sd BEGIN INSERT INTO ev(msg) VALUES('sd_au:' || old.did || '->' || new.did); END;

        INSERT INTO dept VALUES(1, 'eng'), (2, 'ops'), (99, 'bench');
        INSERT INTO emp_cas VALUES(10, 1), (20, 2);
        INSERT INTO emp_sn VALUES(11, 1), (21, 2);
        INSERT INTO emp_sd VALUES(12, 1), (22, 2);

        UPDATE dept SET id = 100 WHERE id = 1;
        DELETE FROM dept WHERE id = 2;

        -- `OR REPLACE` with `recursive_triggers = OFF` vs `ON`:
        CREATE TABLE rep_t(id INTEGER PRIMARY KEY, u TEXT UNIQUE);
        CREATE TRIGGER rep_bd BEFORE DELETE ON rep_t BEGIN
            INSERT INTO ev(msg) VALUES('rep_bd:' || old.id || ':' || old.u);
        END;
        CREATE TRIGGER rep_ad AFTER DELETE ON rep_t BEGIN
            INSERT INTO ev(msg) VALUES('rep_ad:' || old.id || ':' || old.u);
        END;
        INSERT INTO rep_t VALUES(1, 'a'), (2, 'b');
        INSERT OR REPLACE INTO rep_t VALUES(1, 'b');

        PRAGMA recursive_triggers = ON;
        INSERT INTO rep_t VALUES(3, 'c');
        INSERT OR REPLACE INTO rep_t VALUES(1, 'c');

        -- `UPSERT` (`DO UPDATE` vs `DO NOTHING` vs false `WHERE`):
        CREATE TABLE ups(k INT PRIMARY KEY, v INT);
        CREATE TRIGGER ups_bi BEFORE INSERT ON ups BEGIN INSERT INTO ev(msg) VALUES('ups_bi:' || new.k || ':' || new.v); END;
        CREATE TRIGGER ups_ai AFTER INSERT ON ups BEGIN INSERT INTO ev(msg) VALUES('ups_ai:' || new.k || ':' || new.v); END;
        CREATE TRIGGER ups_bu BEFORE UPDATE ON ups BEGIN INSERT INTO ev(msg) VALUES('ups_bu:' || old.v || '->' || new.v); END;
        CREATE TRIGGER ups_au AFTER UPDATE ON ups BEGIN INSERT INTO ev(msg) VALUES('ups_au:' || old.v || '->' || new.v); END;

        INSERT INTO ups VALUES(1, 10) ON CONFLICT(k) DO UPDATE SET v = ups.v + excluded.v;
        INSERT INTO ups VALUES(1, 5) ON CONFLICT(k) DO UPDATE SET v = ups.v + excluded.v;
        INSERT INTO ups VALUES(1, 99) ON CONFLICT(k) DO NOTHING;
        INSERT INTO ups VALUES(1, 77) ON CONFLICT(k) DO UPDATE SET v = 0 WHERE ups.v < 0;
    ";
    let check_q = "
        SELECT 'cas', id, did FROM emp_cas ORDER BY id;
        SELECT 'sn', id, coalesce(did, -1) FROM emp_sn ORDER BY id;
        SELECT 'sd', id, did FROM emp_sd ORDER BY id;
        SELECT 'rep', id, u FROM rep_t ORDER BY id;
        SELECT 'ups', k, v FROM ups ORDER BY k;
        SELECT 'ev', seq, msg FROM ev ORDER BY seq;
    ";
    let (c_ok, c_out, c_err) = run_c_sqlite(
        sandbox.path(),
        ":memory:",
        &format!("{pipeline_sql}\n{check_q}"),
    );
    assert!(c_ok, "{c_err}");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute(pipeline_sql).unwrap();
    let mut r_out = String::new();
    for q in [
        "SELECT 'cas', id, did FROM emp_cas ORDER BY id;",
        "SELECT 'sn', id, coalesce(did, -1) FROM emp_sn ORDER BY id;",
        "SELECT 'sd', id, did FROM emp_sd ORDER BY id;",
        "SELECT 'rep', id, u FROM rep_t ORDER BY id;",
        "SELECT 'ups', k, v FROM ups ORDER BY k;",
        "SELECT 'ev', seq, msg FROM ev ORDER BY seq;",
    ] {
        r_out.push_str(&query_pipe(&conn, q));
    }
    assert_eq!(r_out, c_out);

    // 2. `OR REPLACE` `addrRecheck`: if `AFTER DELETE` trigger re-inserts the conflicting key
    //    OR `BEFORE DELETE` executes `RAISE(IGNORE)`, `addrRecheck` aborts with `UNIQUE constraint failed`!
    assert_differential_error(
        sandbox.path(),
        "or_replace_recheck_after_delete_reinserts",
        "PRAGMA recursive_triggers = ON;
         CREATE TABLE r1(a INT PRIMARY KEY, b INT UNIQUE);
         INSERT INTO r1 VALUES(1, 10);
         CREATE TRIGGER r1_ad AFTER DELETE ON r1 BEGIN
             INSERT INTO r1 VALUES(99, 10);
         END;",
        "INSERT OR REPLACE INTO r1 VALUES(2, 10);",
    );
    assert_differential_error(
        sandbox.path(),
        "or_replace_recheck_before_delete_raise_ignore",
        "PRAGMA recursive_triggers = ON;
         CREATE TABLE r2(a INT PRIMARY KEY, b INT UNIQUE);
         INSERT INTO r2 VALUES(1, 10);
         CREATE TRIGGER r2_bd BEFORE DELETE ON r2 BEGIN
             SELECT RAISE(IGNORE);
         END;",
        "INSERT OR REPLACE INTO r2 VALUES(2, 10);",
    );

    // 3. Child `BEFORE DELETE` with `RAISE(IGNORE)` during `ON DELETE CASCADE` leaves child row
    //    orphaned and triggers immediate `FOREIGN KEY constraint failed`!
    assert_differential_error(
        sandbox.path(),
        "fk_cascade_child_raise_ignore_caught",
        "PRAGMA foreign_keys = ON;
         CREATE TABLE p(id INT PRIMARY KEY);
         CREATE TABLE c(id INT PRIMARY KEY, pid INT REFERENCES p(id) ON DELETE CASCADE);
         INSERT INTO p VALUES(1);
         INSERT INTO c VALUES(10, 1);
         CREATE TRIGGER c_bd BEFORE DELETE ON c BEGIN SELECT RAISE(IGNORE); END;",
        "DELETE FROM p WHERE id = 1;",
    );
}

#[test]
fn step4b_c_sqlite_tcl_suites_upsert_fkey3_and_triggers() {
    for (stem, min_subtests) in [
        ("fkey3", 31usize),
        ("upsert1", 35usize),
        ("upsert2", 14usize),
        ("upsert4", 128usize),
        ("trigger2", 111usize),
        ("trigger3", 24usize),
        ("trigger4", 24usize),
        ("trigger5", 2usize),
        ("trigger8", 1usize),
        ("trigger9", 28usize),
        ("triggerA", 17usize),
        ("triggerB", 202usize),
        ("triggerC", 121usize),
        ("triggerD", 12usize),
        ("triggerF", 12usize),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("sqlite/test")
            .join(format!("{stem}.test"));
        let raw = fs::read_to_string(&path).expect("read reference .test file");
        let stripped = format!(
            "set ::sqlite_options(trigger) 1\n{}",
            strip_test_comments(&raw)
        );
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
