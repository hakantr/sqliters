//! Phase 3 — Step 3-A Integration & C SQLite 3.54.1 Differential Suite:
//! `VIEW`, `SchemaFixer`, Atomic `ALTER TABLE RENAME` View Rewriting, and
//! Atomic `ALTER TABLE DROP COLUMN`.
//!
//! Covers:
//! 1. `CREATE [TEMP | TEMPORARY] VIEW [IF NOT EXISTS]`, `catalog_sql` normalization,
//!    forward-referenced tables, name collisions across tables/views/indexes, `DROP VIEW` vs
//!    `DROP TABLE` cross-kind rejections (even with `IF EXISTS`), and `REINDEX` on views.
//! 2. `SchemaFixer`: bind parameter rejection, permanent-view cross-DB rejection (at creation and
//!    after re-`ATTACH` under a different alias), permanent-view immunity to `TEMP` table/view
//!    shadowing (created before or after the view), local CTE scoping, and `TEMP` view cross-DB
//!    and `temp -> main -> attached` resolution.
//! 3. View compilation delegation (`Ok(None)` -> relational interpreter), prepare validation &
//!    column metadata (`column_count`, `column_names`), `PrepareMode::V2` auto-reprepare vs
//!    `PrepareMode::V1` `SQLITE_SCHEMA`, view expansion across joins, subqueries, CTEs, `CTAS`,
//!    `INSERT ... SELECT`, `EXPLAIN QUERY PLAN`, `IN view_name` (scalar & row-value), circularity
//!    detection, explicit column arity validation, `:1`/`:2` duplicate column disambiguation,
//!    affinity/collation propagation (including compound `UNION ALL`), read-only DML & `CREATE INDEX`
//!    rejection, `PRAGMA [schema.]table_info(view)` & `table_xinfo(view)`, and `VACUUM` /
//!    `VACUUM INTO` view preservation.
//! 4. `PRAGMA legacy_alter_table` and atomic `ALTER TABLE RENAME TO` / `RENAME COLUMN` view
//!    rewriting via structural AST + token spans across `main`, `attached`, and `temp` views,
//!    preserving comments/whitespace/quoting/literals/CTEs/aliases/`USING`, full
//!    `legacy_alter_table` x `foreign_keys` matrix, and multi-DB atomic rollback on post-rename
//!    validation errors.
//! 5. Atomic `ALTER TABLE DROP [COLUMN]` across multi-page rowid tables, `INTEGER PRIMARY KEY`,
//!    short `ADD COLUMN` records with defaults, `VIRTUAL` & `STORED` generated columns, `STRICT`
//!    tables, and `WITHOUT ROWID` tables; all dependency/constraint rejections and atomic rollback.
//! 6. Differential query/error/catalog checks and bidirectional `.db` file exchange +
//!    `PRAGMA integrity_check` with the out-of-tree C SQLite 3.54.1 CLI (`target/sqlite-ref/sqlite3`).
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters::{Connection, ErrorCode, PrepareMode, StepResult, Value};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct Phase3Sandbox {
    dir: PathBuf,
}

impl Phase3Sandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_phase3_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create phase3 temp sandbox");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Phase3Sandbox {
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
                .arg("-C")
                .arg(&amalg_dir)
                .arg("-f")
                .arg(sqlite_src_dir.join("Makefile.linux-generic"))
                .arg(format!("TOP={}", sqlite_src_dir.display()))
                .args(["sqlite3.c", "shell.c"])
                .output()
                .expect("invoke make for out-of-tree sqlite3.c and shell.c");
            assert!(
                make_out.status.success(),
                "failed building out-of-tree amalgamation:\n{}",
                String::from_utf8_lossy(&make_out.stderr)
            );
        }

        let sqlite3_o = ref_root.join("sqlite3.o");
        if !sqlite3_o.exists() {
            let tmp_o = ref_root.join(format!("sqlite3_phase3.{}.o.tmp", std::process::id()));
            let cc_o = Command::new("cc")
                .args(["-O2", "-pthread", "-c"])
                .arg(&sqlite3_c)
                .arg("-I")
                .arg(&amalg_dir)
                .arg("-o")
                .arg(&tmp_o)
                .output()
                .expect("compile sqlite3.o");
            assert!(
                cc_o.status.success(),
                "failed compiling sqlite3.o:\n{}",
                String::from_utf8_lossy(&cc_o.stderr)
            );
            fs::rename(&tmp_o, &sqlite3_o).expect("install sqlite3.o");
        }

        let tmp_bin = ref_root.join(format!("sqlite3_phase3.{}.bin.tmp", std::process::id()));
        let cc_bin = Command::new("cc")
            .args(["-O2", "-pthread"])
            .arg(&shell_c)
            .arg(&sqlite3_o)
            .arg("-I")
            .arg(&amalg_dir)
            .args(["-ldl", "-lm", "-o"])
            .arg(&tmp_bin)
            .output()
            .expect("link sqlite3 CLI");
        assert!(
            cc_bin.status.success(),
            "failed linking sqlite3 CLI:\n{}",
            String::from_utf8_lossy(&cc_bin.stderr)
        );
        fs::rename(&tmp_bin, &cli_bin).expect("install sqlite3 CLI");
        cli_bin
    })
}

fn run_c_sqlite(db_path: &str, sql: &str) -> (bool, String, String) {
    let out = Command::new(c_sqlite3_cli())
        .arg("-batch")
        .arg("-noheader")
        .arg("-list")
        .arg("-separator")
        .arg("|")
        .arg("-nullvalue")
        .arg("<NULL>")
        .arg(db_path)
        .arg(sql)
        .output()
        .expect("invoke C sqlite3 CLI");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout)
            .replace("\r\n", "\n")
            .trim_end_matches('\n')
            .to_owned(),
        String::from_utf8_lossy(&out.stderr)
            .replace("\r\n", "\n")
            .trim()
            .to_owned(),
    )
}

fn format_val_pipe(v: &Value) -> String {
    match v {
        Value::Null => "<NULL>".to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => {
            if f.is_finite() && f.fract() == 0.0 {
                format!("{f:.1}")
            } else {
                format!("{f}")
            }
        }
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn query_pipe_lines(conn: &mut Connection, sql: &str) -> Vec<String> {
    conn.query(sql, &[])
        .unwrap_or_else(|e| panic!("query failed `{sql}`: {e}"))
        .rows
        .into_iter()
        .map(|r| {
            r.values()
                .iter()
                .map(format_val_pipe)
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

fn assert_err_msg(conn: &mut Connection, sql: &str, expected_substr: &str) {
    let err = conn
        .execute_batch(sql)
        .expect_err(&format!("expected error for `{sql}`, but succeeded"));
    assert!(
        err.message.contains(expected_substr),
        "for `{sql}` expected error containing {expected_substr:?}, got: {:?}",
        err.message
    );
}

#[test]
fn view_ddl_catalog_sql_normalization_collisions_and_drop_rules() {
    let sb = Phase3Sandbox::new("view_ddl");
    let aux_path = sb.path().join("aux.db");

    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!("ATTACH DATABASE '{}' AS aux;", aux_path.display()))
        .unwrap();

    // Forward-referenced table in CREATE VIEW is permitted; resolution happens at query time.
    conn.execute_batch(
        "CREATE VIEW IF NOT EXISTS main.v_forward(c1, c2) AS /* keep comment */ SELECT x + 1, y FROM future_tbl;",
    )
    .unwrap();

    // Verify normalized catalog SQL strips `IF NOT EXISTS` and `main.` while preserving body tokens & comments.
    let cat_rows = query_pipe_lines(
        &mut conn,
        "SELECT type, name, tbl_name, rootpage, sql FROM sqlite_schema WHERE name = 'v_forward';",
    );
    assert_eq!(
        cat_rows,
        vec![
            "view|v_forward|v_forward|0|CREATE VIEW v_forward(c1, c2) AS /* keep comment */ SELECT x + 1, y FROM future_tbl"
        ]
    );

    // Compare catalog SQL byte-for-byte with C SQLite 3.54.1.
    let (c_ok, c_out, c_err) = run_c_sqlite(
        ":memory:",
        "CREATE VIEW IF NOT EXISTS main.v_forward(c1, c2) AS /* keep comment */ SELECT x + 1, y FROM future_tbl;
         SELECT type, name, tbl_name, rootpage, sql FROM sqlite_schema WHERE name = 'v_forward';",
    );
    assert!(c_ok, "C sqlite failed: {c_err}");
    assert_eq!(cat_rows.join("\n"), c_out);

    // Querying before `future_tbl` exists fails cleanly; creating `future_tbl` makes it succeed!
    assert_err_msg(
        &mut conn,
        "SELECT * FROM v_forward;",
        "no such table: main.future_tbl",
    );
    conn.execute_batch(
        "CREATE TABLE future_tbl(x INT, y TEXT); INSERT INTO future_tbl VALUES (41, 'ok');",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT c1, c2 FROM v_forward;"),
        vec!["42|ok"]
    );

    // TEMP / TEMPORARY view normalization in sqlite_temp_schema
    conn.execute_batch("CREATE TEMPORARY VIEW IF NOT EXISTS tv_norm AS SELECT 7 AS z;")
        .unwrap();
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT type, name, rootpage, sql FROM sqlite_temp_schema WHERE name = 'tv_norm';"
        ),
        vec!["view|tv_norm|0|CREATE VIEW tv_norm AS SELECT 7 AS z"]
    );
    conn.execute_batch("CREATE TEMP VIEW temp.tv_qual AS SELECT 9 AS q;")
        .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT q FROM tv_qual;"),
        vec!["9"]
    );

    // Attached DB view normalization
    conn.execute_batch("CREATE TABLE aux.atbl(k INT); INSERT INTO aux.atbl VALUES (55);")
        .unwrap();
    conn.execute_batch("CREATE VIEW IF NOT EXISTS aux.av AS SELECT k * 2 AS k2 FROM atbl;")
        .unwrap();
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT sql FROM aux.sqlite_schema WHERE name = 'av';"
        ),
        vec!["CREATE VIEW av AS SELECT k * 2 AS k2 FROM atbl"]
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT k2 FROM aux.av;"),
        vec!["110"]
    );

    // Invalid TEMP qualification & reserved names
    assert_err_msg(
        &mut conn,
        "CREATE TEMP VIEW main.bad_tv AS SELECT 1;",
        "temporary table name must be unqualified",
    );
    assert_err_msg(
        &mut conn,
        "CREATE VIEW sqlite_reserved_v AS SELECT 1;",
        "object name reserved for internal use: sqlite_reserved_v",
    );

    // Collisions across tables, views, and indexes
    conn.execute_batch(
        "CREATE TABLE existing_tbl(a INT); CREATE INDEX existing_idx ON existing_tbl(a);",
    )
    .unwrap();
    assert_err_msg(
        &mut conn,
        "CREATE VIEW existing_tbl AS SELECT 1;",
        "table existing_tbl already exists",
    );
    conn.execute_batch("CREATE VIEW IF NOT EXISTS existing_tbl AS SELECT 1;")
        .unwrap();
    assert_err_msg(
        &mut conn,
        "CREATE VIEW v_forward AS SELECT 1;",
        "view v_forward already exists",
    );
    conn.execute_batch("CREATE VIEW IF NOT EXISTS v_forward AS SELECT 1;")
        .unwrap();
    // Index collision fails even with IF NOT EXISTS (matching C SQLite `sqlite3CreateView`)
    assert_err_msg(
        &mut conn,
        "CREATE VIEW IF NOT EXISTS existing_idx AS SELECT 1;",
        "there is already an index named existing_idx",
    );
    assert_err_msg(
        &mut conn,
        "CREATE TABLE v_forward(a INT);",
        "view v_forward already exists",
    );
    conn.execute_batch("CREATE TABLE IF NOT EXISTS v_forward(a INT);")
        .unwrap();
    assert_err_msg(
        &mut conn,
        "CREATE INDEX v_forward ON existing_tbl(a);",
        "there is already a table named v_forward",
    );

    // Cross-kind DROP VIEW vs DROP TABLE errors (even with IF EXISTS!)
    assert_err_msg(
        &mut conn,
        "DROP VIEW existing_tbl;",
        "use DROP TABLE to delete table existing_tbl",
    );
    assert_err_msg(
        &mut conn,
        "DROP VIEW IF EXISTS existing_tbl;",
        "use DROP TABLE to delete table existing_tbl",
    );
    assert_err_msg(
        &mut conn,
        "DROP VIEW IF EXISTS sqlite_schema;",
        "table sqlite_master may not be dropped",
    );
    assert_err_msg(
        &mut conn,
        "DROP TABLE v_forward;",
        "use DROP VIEW to delete view v_forward",
    );
    assert_err_msg(
        &mut conn,
        "DROP TABLE IF EXISTS v_forward;",
        "use DROP VIEW to delete view v_forward",
    );

    // REINDEX on a view is a no-op; DROP VIEW removes it cleanly.
    conn.execute_batch("REINDEX v_forward;").unwrap();
    assert_err_msg(&mut conn, "DROP VIEW no_such_v;", "no such view: no_such_v");
    conn.execute_batch("DROP VIEW IF EXISTS no_such_v;")
        .unwrap();
    conn.execute_batch("DROP VIEW v_forward;").unwrap();
    assert_err_msg(
        &mut conn,
        "SELECT * FROM v_forward;",
        "no such table: v_forward",
    );
}

#[test]
fn schema_fixer_parameters_cross_db_rejection_and_temp_shadowing_immunity() {
    let sb = Phase3Sandbox::new("schema_fixer");
    let main_path = sb.path().join("main.db");
    let aux_path = sb.path().join("aux.db");

    let mut conn = Connection::open(&main_path).unwrap();
    conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS aux;
         CREATE TABLE main.items(v TEXT);
         INSERT INTO main.items VALUES ('from_main');
         CREATE TABLE aux.items(v TEXT);
         INSERT INTO aux.items VALUES ('from_aux');",
        aux_path.display()
    ))
    .unwrap();

    // 1. Bind parameters of every lexical form are rejected in views (`prepare` & `execute`).
    for bad_sql in [
        "CREATE VIEW vp1 AS SELECT ?;",
        "CREATE VIEW vp2 AS SELECT ?2;",
        "CREATE VIEW vp3 AS SELECT :named;",
        "CREATE VIEW vp4 AS SELECT @at_param;",
        "CREATE VIEW vp5 AS SELECT $dollar;",
        "CREATE VIEW vp6 AS SELECT v FROM items WHERE v = ?;",
    ] {
        assert_err_msg(&mut conn, bad_sql, "parameters are not allowed in views");
        let prep_err = conn.prepare(bad_sql).unwrap_err();
        assert!(
            prep_err
                .message
                .contains("parameters are not allowed in views")
        );
    }

    // 2. Permanent views cannot reference objects in another database.
    assert_err_msg(
        &mut conn,
        "CREATE VIEW main.v_cross AS SELECT * FROM aux.items;",
        "view v_cross cannot reference objects in database aux",
    );
    assert_err_msg(
        &mut conn,
        "CREATE VIEW aux.v_cross AS SELECT * FROM main.items;",
        "view v_cross cannot reference objects in database main",
    );
    assert_err_msg(
        &mut conn,
        "CREATE VIEW main.v_cross_in AS SELECT 1 WHERE 'x' IN aux.items;",
        "view v_cross_in cannot reference objects in database aux",
    );

    // 3. Unqualified attached view remains portable when re-attached under a different alias `aux_renamed`,
    //    whereas explicit same-DB schema qualification (`aux.items`) works under `aux` and is rejected
    //    with `SQLITE_CORRUPT` (`malformed database schema ...`) if re-attached under `aux_renamed`!
    conn.execute_batch("CREATE VIEW aux.v_unqual_aux AS SELECT v FROM items;")
        .unwrap();
    conn.execute_batch(&format!(
        "DETACH DATABASE aux;
         ATTACH DATABASE '{}' AS aux_renamed;",
        aux_path.display()
    ))
    .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM aux_renamed.v_unqual_aux;"),
        vec!["from_aux"]
    );
    let (c_ok_unqual, c_out_unqual, _) = run_c_sqlite(
        main_path.to_str().unwrap(),
        &format!(
            "ATTACH DATABASE '{}' AS aux_renamed; SELECT * FROM aux_renamed.v_unqual_aux;",
            aux_path.display()
        ),
    );
    assert!(c_ok_unqual);
    assert_eq!(c_out_unqual, "from_aux");

    // Re-attach as `aux`, add explicit `aux.items` view, verify it works under `aux` and fails on `ATTACH ... AS aux_renamed`
    // in BOTH Rust and C SQLite 3.54.1!
    conn.execute_batch(&format!(
        "DETACH DATABASE aux_renamed;
         ATTACH DATABASE '{}' AS aux;
         CREATE VIEW aux.v_explicit_aux AS SELECT aux.items.v FROM aux.items;",
        aux_path.display()
    ))
    .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM aux.v_explicit_aux;"),
        vec!["from_aux"]
    );
    conn.execute_batch("DETACH DATABASE aux;").unwrap();
    let attach_bad = conn
        .execute_batch(&format!(
            "ATTACH DATABASE '{}' AS aux_renamed;",
            aux_path.display()
        ))
        .unwrap_err();
    assert_eq!(attach_bad.code, ErrorCode::Corrupt);
    assert!(
        attach_bad
            .message
            .contains("malformed database schema (v_explicit_aux) - view v_explicit_aux cannot reference objects in database aux"),
        "unexpected Rust ATTACH error: {attach_bad:?}"
    );
    let (c_ok_exp, _, c_err_exp) = run_c_sqlite(
        main_path.to_str().unwrap(),
        &format!("ATTACH DATABASE '{}' AS aux_renamed;", aux_path.display()),
    );
    assert!(!c_ok_exp);
    assert!(
        c_err_exp.contains(
            "malformed database schema (v_explicit_aux) - view v_explicit_aux cannot reference objects in database aux"
        ),
        "unexpected C error: {c_err_exp}"
    );

    // Drop `v_explicit_aux` under `aux` and re-attach cleanly as `aux_renamed` for remaining checks.
    conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS aux;
         DROP VIEW aux.v_explicit_aux;
         DETACH DATABASE aux;
         ATTACH DATABASE '{}' AS aux_renamed;",
        aux_path.display(),
        aux_path.display()
    ))
    .unwrap();

    // 4. Permanent-view immunity to TEMP shadowing (both BEFORE and AFTER CREATE VIEW)!
    conn.execute_batch(
        "CREATE TEMP TABLE items(v TEXT);
         INSERT INTO temp.items VALUES ('from_temp_shadow');",
    )
    .unwrap();
    // Direct unqualified query sees TEMP (`from_temp_shadow`).
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT v FROM items;"),
        vec!["from_temp_shadow"]
    );
    // Permanent view created AFTER `temp.items` already exists MUST STILL pin `items` to `main`!
    conn.execute_batch("CREATE VIEW main.v_after_temp AS SELECT v FROM items;")
        .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT v FROM main.v_after_temp;"),
        vec!["from_main"]
    );

    // Permanent view created BEFORE a TEMP table/view is created MUST ALSO remain pinned to `main`!
    conn.execute_batch(
        "CREATE TABLE main.base2(n INT);
         INSERT INTO main.base2 VALUES (100);
         CREATE VIEW main.inner_v AS SELECT n + 1 AS n1 FROM base2;
         CREATE VIEW main.outer_v AS SELECT n1 * 2 AS res FROM inner_v;
         CREATE TEMP TABLE base2(n INT);
         INSERT INTO temp.base2 VALUES (9999);
         CREATE TEMP VIEW inner_v AS SELECT 7777 AS n1;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT res FROM main.outer_v;"),
        vec!["202"]
    );

    // Local CTE inside a permanent view MUST NOT be rewritten to `main.items` — it shadows `main.items`!
    conn.execute_batch(
        "CREATE VIEW main.v_local_cte AS WITH items(v) AS (SELECT 'from_cte') SELECT v FROM items;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT v FROM main.v_local_cte;"),
        vec!["from_cte"]
    );

    // 5. TEMP view CAN cross-reference `main`, `aux_renamed`, and `temp`, and resolves unqualified via `temp -> main -> attached`.
    conn.execute_batch(
        "CREATE TEMP VIEW tv_multi AS
         SELECT items.v AS tv, m.v AS mv, a.v AS av
         FROM items, main.items AS m, aux_renamed.items AS a;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT tv, mv, av FROM tv_multi;"),
        vec!["from_temp_shadow|from_main|from_aux"]
    );
}

#[test]
fn view_queries_metadata_reprepare_pragma_table_info_and_vacuum() {
    let sb = Phase3Sandbox::new("view_queries");
    let db_path = sb.path().join("vq.db");
    let vac_into_path = sb.path().join("vq_into.db");

    let mut conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE dept(id INTEGER PRIMARY KEY, dname VARCHAR(20) COLLATE NOCASE);
         CREATE TABLE emp(id INTEGER PRIMARY KEY, dept_id INT, ename TEXT, salary NUMERIC);
         INSERT INTO dept VALUES (1, 'Eng'), (2, 'Sales');
         INSERT INTO emp VALUES (10, 1, 'Alice', 120), (11, 1, 'Bob', 95), (12, 2, 'Cara', 110);

         CREATE VIEW v_emp_dept AS
           SELECT e.id AS emp_id, d.dname, e.ename, e.salary
           FROM emp e JOIN dept d ON e.dept_id = d.id;

         CREATE VIEW v_named(c_id, c_dept, c_total) AS
           SELECT d.id, d.dname, SUM(e.salary)
           FROM dept d LEFT JOIN emp e ON e.dept_id = d.id
           GROUP BY d.id, d.dname;",
    )
    .unwrap();

    // 1. Prepare metadata (`column_count`, `column_names`) without side effects
    {
        let stmt = conn
            .prepare("SELECT * FROM v_emp_dept ORDER BY emp_id;")
            .unwrap();
        assert_eq!(stmt.column_count(), 4);
        assert_eq!(stmt.column_names(), &["emp_id", "dname", "ename", "salary"]);
    }
    {
        let stmt = conn
            .prepare("SELECT * FROM v_named ORDER BY c_id;")
            .unwrap();
        assert_eq!(stmt.column_count(), 3);
        assert_eq!(stmt.column_names(), &["c_id", "c_dept", "c_total"]);
    }

    // 2. Joins, subqueries, CTE isolation (outer WITH must NOT leak into view!), CTAS, INSERT ... SELECT, EQP, IN view
    conn.execute_batch("CREATE VIEW v_dept_ids AS SELECT id FROM dept; CREATE VIEW v_pairs AS SELECT dept_id, ename FROM emp;")
        .unwrap();
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "WITH dept(id, dname) AS (SELECT 1, 'FAKE_OUTER_CTE')
             SELECT emp_id, dname FROM v_emp_dept WHERE dname = 'eng' ORDER BY emp_id;"
        ),
        vec!["10|Eng", "11|Eng"] // NOCASE preserved from `dept.dname` + outer CTE ignored inside view!
    );
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT 1 IN v_dept_ids, 99 IN v_dept_ids, (1, 'Alice') IN v_pairs, (2, 'Alice') IN v_pairs;"
        ),
        vec!["1|0|1|0"]
    );
    assert_err_msg(
        &mut conn,
        "SELECT 1 IN v_pairs;",
        "sub-select returns 2 columns - expected 1",
    );
    assert_err_msg(
        &mut conn,
        "SELECT (1, 2, 3) IN v_pairs;",
        "sub-select returns 2 columns - expected 3",
    );

    // Comparison affinity through view (`emp_id` has INTEGER affinity -> `'10'` coerced to `10`)
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT ename, typeof(emp_id) FROM v_emp_dept WHERE emp_id = '10';"
        ),
        vec!["Alice|integer"]
    );

    // CTAS & INSERT INTO ... SELECT & EXPLAIN QUERY PLAN
    conn.execute_batch(
        "CREATE TABLE summary_copy AS SELECT * FROM v_named ORDER BY c_id;
         CREATE TABLE summary_ins(id INT, dn TEXT, tot NUMERIC);
         INSERT INTO summary_ins SELECT * FROM v_named ORDER BY c_id;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM summary_copy ORDER BY c_id;"),
        vec!["1|Eng|215", "2|Sales|110"]
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM summary_ins ORDER BY id;"),
        vec!["1|Eng|215", "2|Sales|110"]
    );
    let eqp_rows = query_pipe_lines(&mut conn, "EXPLAIN QUERY PLAN SELECT * FROM v_dept_ids;");
    assert!(!eqp_rows.is_empty());

    // 3. Circularity detection & explicit column arity mismatch & `:1` duplicate column disambiguation
    conn.execute_batch(
        "CREATE VIEW cyc_self AS SELECT * FROM cyc_self;
         CREATE VIEW cyc_a AS SELECT * FROM cyc_b;
         CREATE VIEW cyc_b AS SELECT * FROM cyc_a;
         CREATE VIEW cyc_cte AS WITH w AS (SELECT * FROM cyc_cte) SELECT * FROM w;
         CREATE VIEW v_arity_bad(a, b, c) AS SELECT 1, 2;
         CREATE VIEW v_dedup AS SELECT 1 AS x, 2 AS x, 3 AS \"x:1\", 4 AS x;
         CREATE VIEW v_dedup_explicit(a, a) AS SELECT 10, 20;",
    )
    .unwrap();
    assert_err_msg(
        &mut conn,
        "SELECT * FROM cyc_self;",
        "view cyc_self is circularly defined",
    );
    assert_err_msg(&mut conn, "SELECT * FROM cyc_a;", "is circularly defined");
    assert_err_msg(
        &mut conn,
        "SELECT * FROM cyc_cte;",
        "view cyc_cte is circularly defined",
    );
    assert_err_msg(
        &mut conn,
        "SELECT * FROM v_arity_bad;",
        "expected 3 columns for 'v_arity_bad' but got 2",
    );
    {
        let s1 = conn.prepare("SELECT * FROM v_dedup;").unwrap();
        assert_eq!(s1.column_names(), &["x", "x:1", "x:2", "x:3"]);
        let s2 = conn.prepare("SELECT * FROM v_dedup_explicit;").unwrap();
        assert_eq!(s2.column_names(), &["a", "a:1"]);
    }

    // 4. Read-only DML & CREATE INDEX / INDEXED BY rules on views
    assert_err_msg(
        &mut conn,
        "INSERT INTO v_dept_ids VALUES (5);",
        "cannot modify v_dept_ids because it is a view",
    );
    assert_err_msg(
        &mut conn,
        "UPDATE v_dept_ids SET id = 9;",
        "cannot modify v_dept_ids because it is a view",
    );
    assert_err_msg(
        &mut conn,
        "DELETE FROM v_dept_ids;",
        "cannot modify v_dept_ids because it is a view",
    );
    assert_err_msg(
        &mut conn,
        "CREATE INDEX idx_on_v ON v_dept_ids(id);",
        "views may not be indexed",
    );
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT id FROM v_dept_ids NOT INDEXED ORDER BY id;"
        ),
        vec!["1", "2"]
    );
    assert_err_msg(
        &mut conn,
        "SELECT id FROM v_dept_ids INDEXED BY nope;",
        "no such index: nope",
    );

    // 5. PRAGMA table_info(view) & table_xinfo(view) differential parity (including compound UNION ALL views!)
    conn.execute_batch(
        "CREATE VIEW v_union_types AS
           SELECT dname, id, id, salary FROM dept JOIN emp USING(id) WHERE 0
           UNION ALL
           SELECT 'txt', 42, 'demote_to_blob', 3.5;",
    )
    .unwrap();
    for pragma_sql in [
        "PRAGMA table_info(v_emp_dept);",
        "PRAGMA table_xinfo(v_emp_dept);",
        "PRAGMA table_info(v_named);",
        "PRAGMA table_info(v_arity_bad);",
        "PRAGMA table_info(v_dedup);",
        "PRAGMA table_xinfo(v_union_types);",
    ] {
        let rust_out = query_pipe_lines(&mut conn, pragma_sql).join("\n");
        let (c_ok, c_out, c_err) = run_c_sqlite(db_path.to_str().unwrap(), pragma_sql);
        assert!(c_ok, "C sqlite failed `{pragma_sql}`: {c_err}");
        assert_eq!(
            rust_out, c_out,
            "PRAGMA mismatch on `{pragma_sql}`:\nRust:\n{rust_out}\nC:\n{c_out}"
        );
    }

    // Drop circular/broken test views before VACUUM / reprepare test
    conn.execute_batch(
        "DROP VIEW cyc_self; DROP VIEW cyc_a; DROP VIEW cyc_b; DROP VIEW cyc_cte; DROP VIEW v_arity_bad;",
    )
    .unwrap();

    // 6. PrepareMode::V2 auto-reprepare vs PrepareMode::V1 SQLITE_SCHEMA when view definition changes
    conn.execute_batch("CREATE VIEW v_evolving AS SELECT 10 AS a;")
        .unwrap();
    let mut stmt_v2 = conn
        .prepare_with_mode("SELECT * FROM v_evolving;", PrepareMode::V2)
        .unwrap();
    let mut stmt_v1 = conn
        .prepare_with_mode("SELECT * FROM v_evolving;", PrepareMode::V1)
        .unwrap();
    conn.execute_batch("DROP VIEW v_evolving; CREATE VIEW v_evolving AS SELECT 20 AS a, 30 AS b;")
        .unwrap();

    let StepResult::Row(row) = stmt_v2.step().unwrap() else {
        panic!("expected StepResult::Row after V2 reprepare");
    };
    assert_eq!(stmt_v2.column_count(), 2);
    assert_eq!(stmt_v2.column_names(), &["a", "b"]);
    assert_eq!(row.get_i64(0), Some(20));
    assert_eq!(row.get_i64(1), Some(30));
    assert!(matches!(stmt_v2.step().unwrap(), StepResult::Done));
    drop(stmt_v2);

    let v1_err = stmt_v1.step().unwrap_err();
    assert_eq!(v1_err.code, ErrorCode::Schema);
    drop(stmt_v1);

    // 7. VACUUM and VACUUM INTO preserve views intact
    conn.execute_batch(&format!(
        "VACUUM;
         VACUUM INTO '{}';",
        vac_into_path.display()
    ))
    .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "PRAGMA integrity_check;"),
        vec!["ok"]
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM v_evolving;"),
        vec!["20|30"]
    );
    let (c_ok_vac, c_out_vac, c_err_vac) = run_c_sqlite(
        vac_into_path.to_str().unwrap(),
        "PRAGMA integrity_check; SELECT emp_id, dname FROM v_emp_dept ORDER BY emp_id;",
    );
    assert!(c_ok_vac, "C sqlite failed on VACUUM INTO db: {c_err_vac}");
    assert_eq!(c_out_vac, "ok\n10|Eng\n11|Eng\n12|Sales");
}

#[test]
fn atomic_alter_rename_table_and_column_view_rewriting_legacy_matrix_and_rollback() {
    let sb = Phase3Sandbox::new("alter_rename");
    let main_path = sb.path().join("main.db");
    let aux_path = sb.path().join("aux.db");

    let mut conn = Connection::open(&main_path).unwrap();
    // PRAGMA legacy_alter_table getter/setter
    assert_eq!(
        query_pipe_lines(&mut conn, "PRAGMA legacy_alter_table;"),
        vec!["0"]
    );
    conn.execute_batch("PRAGMA legacy_alter_table = ON;")
        .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "PRAGMA legacy_alter_table;"),
        vec!["1"]
    );
    conn.execute_batch("PRAGMA legacy_alter_table = false;")
        .unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn, "PRAGMA legacy_alter_table;"),
        vec!["0"]
    );

    conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS aux;
         CREATE TABLE src(id INTEGER PRIMARY KEY, old_col INT, keep_col TEXT);
         INSERT INTO src VALUES (1, 10, 'src');
         CREATE TABLE other(id INT, old_col INT);
         INSERT INTO other VALUES (1, 99);
         CREATE TABLE aux.asrc(id INT, old_col INT);
         INSERT INTO aux.asrc VALUES (1, 77);

         -- Structural view SQL with comments, string literals, CTEs, aliases, ORDER BY alias vs col
         CREATE VIEW v_complex AS
           /* header src old_col */
           WITH shadow_cte(old_col) AS (SELECT 500)
           SELECT
             src.old_col AS preserved_alias,
             al.old_col AS from_aliased,
             other.old_col AS untouched_other,
             shadow_cte.old_col AS untouched_cte,
             'src.old_col literal' AS lit
           FROM src
           JOIN src AS al ON src.id = al.id
           JOIN other ON other.id = src.id
           JOIN shadow_cte ON 1=1
           WHERE src.old_col > 0 AND 'old_col' = 'old_col'
           ORDER BY preserved_alias, src.old_col;

         -- TEMP view referencing both main.src and aux.asrc
         CREATE TEMP VIEW tv_cross AS
           SELECT main.src.old_col + aux.asrc.old_col AS sum_val
           FROM main.src JOIN aux.asrc ON main.src.id = aux.asrc.id;",
        aux_path.display()
    ))
    .unwrap();

    // 1. RENAME TO rewrites both `main.v_complex` and `temp.tv_cross`, preserving comments, literals, aliases, and CTEs!
    conn.execute_batch("ALTER TABLE src RENAME TO dst;")
        .unwrap();
    let v_sql_after_tbl = query_pipe_lines(
        &mut conn,
        "SELECT sql FROM sqlite_schema WHERE name = 'v_complex';",
    )[0]
    .clone();
    assert!(
        v_sql_after_tbl.contains("/* header src old_col */"),
        "comment was altered: {v_sql_after_tbl}"
    );
    assert!(
        v_sql_after_tbl.contains("'src.old_col literal'"),
        "string literal was altered: {v_sql_after_tbl}"
    );
    assert!(
        v_sql_after_tbl.contains("al.old_col AS from_aliased"),
        "table alias qualifier `al.` should NOT be rewritten: {v_sql_after_tbl}"
    );
    assert!(
        v_sql_after_tbl.contains("FROM \"dst\"") && v_sql_after_tbl.contains("JOIN \"dst\" AS al"),
        "table references not rewritten: {v_sql_after_tbl}"
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM v_complex;"),
        vec!["10|10|99|500|src.old_col literal"]
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM tv_cross;"),
        vec!["87"]
    );

    // 2. RENAME COLUMN rewrites ONLY columns resolved to `dst.old_col` across `main.v_complex` and `temp.tv_cross`!
    conn.execute_batch("ALTER TABLE dst RENAME COLUMN old_col TO new_col;")
        .unwrap();
    let v_sql_after_col = query_pipe_lines(
        &mut conn,
        "SELECT sql FROM sqlite_schema WHERE name = 'v_complex';",
    )[0]
    .clone();
    assert!(
        v_sql_after_col.contains("\"dst\".new_col AS preserved_alias")
            && v_sql_after_col.contains("al.new_col AS from_aliased")
            && v_sql_after_col.contains("other.old_col AS untouched_other")
            && v_sql_after_col.contains("shadow_cte.old_col AS untouched_cte")
            && v_sql_after_col.contains("ORDER BY preserved_alias, \"dst\".new_col"),
        "unexpected column rewrite result:\n{v_sql_after_col}"
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM v_complex;"),
        vec!["10|10|99|500|src.old_col literal"]
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM tv_cross;"),
        vec!["87"]
    );

    // Rename column in attached `aux.asrc` ALSO updates `temp.tv_cross`!
    conn.execute_batch("ALTER TABLE aux.asrc RENAME COLUMN old_col TO aux_new_col;")
        .unwrap();
    let tv_sql = query_pipe_lines(
        &mut conn,
        "SELECT sql FROM sqlite_temp_schema WHERE name = 'tv_cross';",
    )[0]
    .clone();
    assert!(
        tv_sql.contains("aux.asrc.aux_new_col"),
        "TEMP view referencing attached table was not updated: {tv_sql}"
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM tv_cross;"),
        vec!["87"]
    );

    // 3. Multi-DB atomic rollback when a TEMP view fails post-rename validation!
    // Create `u1(a, b)` and `u2(a, c)` in `main`, a `main` view `mv_u`, AND a `temp` view `tv_using` with `JOIN USING(a)`.
    // Renaming `u1.a` to `a_renamed` updates `mv_u`, leaves `USING(a)` untouched (`alter.c`), and then fails validation on `tv_using`!
    // BOTH `main` (table `u1` AND `mv_u`) AND `temp` (`tv_using`) must roll back atomically!
    conn.execute_batch(
        "CREATE TABLE u1(a INT, b INT);
         CREATE TABLE u2(a INT, c INT);
         INSERT INTO u1 VALUES (1, 2);
         INSERT INTO u2 VALUES (1, 3);
         CREATE VIEW mv_u AS SELECT a, b FROM u1;
         CREATE TEMP VIEW tv_using AS SELECT u1.b, c FROM u1 JOIN u2 USING(a);",
    )
    .unwrap();
    let pre_main_u1_sql = query_pipe_lines(
        &mut conn,
        "SELECT sql FROM sqlite_schema WHERE name = 'u1';",
    );
    let pre_main_mv_sql = query_pipe_lines(
        &mut conn,
        "SELECT sql FROM sqlite_schema WHERE name = 'mv_u';",
    );
    let pre_temp_tv_sql = query_pipe_lines(
        &mut conn,
        "SELECT sql FROM sqlite_temp_schema WHERE name = 'tv_using';",
    );

    // Test rollback inside an explicit BEGIN transaction too (preserving earlier inserts in the tx!).
    conn.execute_batch("BEGIN; INSERT INTO u1 VALUES (4, 5);")
        .unwrap();
    assert_err_msg(
        &mut conn,
        "ALTER TABLE u1 RENAME COLUMN a TO a_renamed;",
        "error in view tv_using after rename",
    );
    conn.execute_batch("COMMIT;").unwrap();

    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT sql FROM sqlite_schema WHERE name = 'u1';"
        ),
        pre_main_u1_sql
    );
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT sql FROM sqlite_schema WHERE name = 'mv_u';"
        ),
        pre_main_mv_sql
    );
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT sql FROM sqlite_temp_schema WHERE name = 'tv_using';"
        ),
        pre_temp_tv_sql
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT a, b FROM mv_u ORDER BY a;"),
        vec!["1|2", "4|5"]
    );
    assert_eq!(
        query_pipe_lines(&mut conn, "SELECT * FROM tv_using;"),
        vec!["2|3"]
    );

    // 4. Full `legacy_alter_table` (0/1) x `foreign_keys` (0/1) differential 2x2 matrix vs C SQLite 3.54.1!
    for legacy in ["OFF", "ON"] {
        for fk in ["OFF", "ON"] {
            let matrix_sql = format!(
                "PRAGMA legacy_alter_table = {legacy};
                 PRAGMA foreign_keys = {fk};
                 CREATE TABLE p(id INTEGER PRIMARY KEY, x INT CHECK(x > 0));
                 CREATE TABLE c(id INT, pid INT REFERENCES p(id));
                 CREATE VIEW vp AS SELECT id, x FROM p;
                 ALTER TABLE p RENAME TO p_new;
                 SELECT name, sql FROM sqlite_schema WHERE type IN ('table', 'view') ORDER BY name;"
            );
            let mut mconn = Connection::open_in_memory().unwrap();
            mconn.execute_batch(&matrix_sql).unwrap();
            let r_out = query_pipe_lines(
                &mut mconn,
                "SELECT name, sql FROM sqlite_schema WHERE type IN ('table', 'view') ORDER BY name;",
            )
            .join("\n");
            let (c_ok, c_out, c_err) = run_c_sqlite(":memory:", &matrix_sql);
            assert!(
                c_ok,
                "C sqlite failed for legacy={legacy}, fk={fk}: {c_err}"
            );
            assert_eq!(
                r_out, c_out,
                "matrix mismatch for legacy={legacy}, fk={fk}:\nRust:\n{r_out}\nC:\n{c_out}"
            );
        }
    }

    // Also verify self-table qualifier `CHECK(p.x > 0)`: rewritten under `legacy_alter_table = OFF`,
    // and rejected by post-rename table validation under `legacy_alter_table = ON` in BOTH Rust & C SQLite!
    {
        let ok_sql = "PRAGMA legacy_alter_table = OFF;
                      CREATE TABLE p(id INTEGER PRIMARY KEY, x INT CHECK(p.x > 0));
                      ALTER TABLE p RENAME TO p_new;
                      SELECT sql FROM sqlite_schema WHERE name = 'p_new';";
        let mut c_off = Connection::open_in_memory().unwrap();
        c_off.execute_batch(ok_sql).unwrap();
        let r_sql = query_pipe_lines(
            &mut c_off,
            "SELECT sql FROM sqlite_schema WHERE name = 'p_new';",
        )
        .join("\n");
        let (c_ok, c_out, _) = run_c_sqlite(":memory:", ok_sql);
        assert!(c_ok);
        assert_eq!(r_sql, c_out);

        let mut c_on = Connection::open_in_memory().unwrap();
        c_on.execute_batch(
            "PRAGMA legacy_alter_table = ON;
             CREATE TABLE p(id INTEGER PRIMARY KEY, x INT CHECK(p.x > 0));",
        )
        .unwrap();
        assert_err_msg(
            &mut c_on,
            "ALTER TABLE p RENAME TO p_new;",
            "error in table p_new after rename: no such column: p.x",
        );
    }
}

#[test]
fn atomic_alter_table_drop_column_physical_variants_rejections_and_interoperability() {
    let sb = Phase3Sandbox::new("drop_col");
    let rust_db = sb.path().join("rust_drop.db");
    let c_db = sb.path().join("c_drop.db");

    // 1. Differential physical variants: rowid table, INTEGER PRIMARY KEY, ADD COLUMN short records,
    //    VIRTUAL & STORED generated columns, STRICT table, WITHOUT ROWID table, and multi-page B-tree!
    let workload_sql = "
        -- (A) Rowid table with INTEGER PRIMARY KEY, ADD COLUMN default, VIRTUAL and STORED columns
        CREATE TABLE t_rich(
            drop_first TEXT,
            id INTEGER PRIMARY KEY,
            base INT NOT NULL,
            v_gen INT GENERATED ALWAYS AS (base * 3) VIRTUAL,
            s_gen INT GENERATED ALWAYS AS (base + 100) STORED,
            drop_chk INT CHECK(drop_chk > 0),
            keep_chk INT,
            CHECK(keep_chk >= 0)
        );
        INSERT INTO t_rich(drop_first, id, base, drop_chk, keep_chk) VALUES
            ('a', 1, 10, 5, 1),
            ('b', 2, 20, 6, 2),
            ('c', 5, 30, 7, 3);
        ALTER TABLE t_rich ADD COLUMN added_def TEXT DEFAULT 'filled_default';
        INSERT INTO t_rich(drop_first, id, base, drop_chk, keep_chk, added_def) VALUES
            ('d', 9, 40, 8, 4, 'explicit_val');

        -- Drop both `DROP col` (without COLUMN keyword) and `DROP COLUMN col`
        ALTER TABLE t_rich DROP drop_first;
        ALTER TABLE t_rich DROP COLUMN v_gen;
        ALTER TABLE t_rich DROP COLUMN s_gen;
        ALTER TABLE t_rich DROP COLUMN drop_chk;

        -- (B) STRICT table + view with `SELECT *` adapting after DROP COLUMN
        CREATE TABLE t_strict(k INT PRIMARY KEY, mid TEXT, score REAL) STRICT;
        INSERT INTO t_strict VALUES (1, 'drop_me', 2), (2, 'drop_me_too', 4.5);
        CREATE VIEW v_star AS SELECT * FROM t_strict;
        ALTER TABLE t_strict DROP COLUMN mid;

        -- (C) WITHOUT ROWID composite PK + prior ADD COLUMN default + DROP COLUMN
        CREATE TABLE t_wor(
            k1 TEXT,
            drop_mid BLOB,
            k2 INT,
            keep_val TEXT,
            PRIMARY KEY(k1, k2)
        ) WITHOUT ROWID;
        INSERT INTO t_wor VALUES ('alpha', X'CAFE', 1, 'v1'), ('beta', X'BEEF', 2, 'v2');
        ALTER TABLE t_wor ADD COLUMN tail_def INT DEFAULT 77;
        INSERT INTO t_wor VALUES ('gamma', X'00', 3, 'v3', 88);
        ALTER TABLE t_wor DROP COLUMN drop_mid;

        -- (D) Inline REFERENCES on dropped column succeeds cleanly
        CREATE TABLE parent_fk(id INT PRIMARY KEY);
        INSERT INTO parent_fk VALUES (1);
        CREATE TABLE child_fk(id INT PRIMARY KEY, pid INT REFERENCES parent_fk(id), note TEXT);
        INSERT INTO child_fk VALUES (10, 1, 'kept');
        PRAGMA foreign_keys = ON;
        ALTER TABLE child_fk DROP COLUMN pid;
    ";

    let mut conn = Connection::open(&rust_db).unwrap();
    conn.execute_batch(workload_sql).unwrap();

    // Multi-page + overflow payload test on `rust_db` (250 rows with 600-byte strings)
    conn.execute_batch(
        "CREATE TABLE t_big(id INTEGER PRIMARY KEY, big_drop TEXT, keep_tag TEXT); BEGIN;",
    )
    .unwrap();
    for i in 1..=250 {
        let blob_txt = format!("row_{i:04}_{}", "x".repeat(600));
        conn.execute_batch(&format!(
            "INSERT INTO t_big VALUES ({i}, '{blob_txt}', 'tag_{i}');"
        ))
        .unwrap();
    }
    conn.execute_batch("COMMIT; ALTER TABLE t_big DROP COLUMN big_drop;")
        .unwrap();

    let (c_ok_init, _, c_err_init) = run_c_sqlite(c_db.to_str().unwrap(), workload_sql);
    assert!(c_ok_init, "C sqlite workload failed: {c_err_init}");

    // Verify `PRAGMA integrity_check` and exact differential query results between Rust and C SQLite!
    assert_eq!(
        query_pipe_lines(&mut conn, "PRAGMA integrity_check;"),
        vec!["ok"]
    );
    assert_eq!(
        query_pipe_lines(
            &mut conn,
            "SELECT COUNT(*), MIN(keep_tag), MAX(keep_tag) FROM t_big;"
        ),
        vec!["250|tag_1|tag_99"]
    );

    let verify_sql = "
        SELECT id, base, keep_chk, added_def FROM t_rich ORDER BY id;
        SELECT k, score, typeof(score) FROM v_star ORDER BY k;
        SELECT k1, k2, keep_val, tail_def FROM t_wor ORDER BY k1, k2;
        SELECT id, note FROM child_fk ORDER BY id;
    ";
    let rust_verify = [
        query_pipe_lines(
            &mut conn,
            "SELECT id, base, keep_chk, added_def FROM t_rich ORDER BY id;",
        ),
        query_pipe_lines(
            &mut conn,
            "SELECT k, score, typeof(score) FROM v_star ORDER BY k;",
        ),
        query_pipe_lines(
            &mut conn,
            "SELECT k1, k2, keep_val, tail_def FROM t_wor ORDER BY k1, k2;",
        ),
        query_pipe_lines(&mut conn, "SELECT id, note FROM child_fk ORDER BY id;"),
    ]
    .concat()
    .join("\n");

    let (c_ok_ver, c_verify, c_err_ver) = run_c_sqlite(c_db.to_str().unwrap(), verify_sql);
    assert!(c_ok_ver, "C verify failed: {c_err_ver}");
    assert_eq!(rust_verify, c_verify);

    drop(conn);

    // 2. Bidirectional `.db` interoperability:
    //    (a) C SQLite opens `rust_db` (including multi-page rewritten `t_big`), runs `PRAGMA integrity_check` and queries!
    let (c_ok_on_rust, c_on_rust_out, c_on_rust_err) = run_c_sqlite(
        rust_db.to_str().unwrap(),
        &format!("PRAGMA integrity_check; SELECT COUNT(*) FROM t_big; {verify_sql}"),
    );
    assert!(
        c_ok_on_rust,
        "C sqlite rejected Rust-produced DB: {c_on_rust_err}"
    );
    assert_eq!(c_on_rust_out, format!("ok\n250\n{c_verify}"));

    //    (b) Rust `sqliters` opens `c_db` produced by C SQLite, runs `PRAGMA integrity_check`, queries, and performs further ALTER!
    let mut conn_from_c = Connection::open(&c_db).unwrap();
    assert_eq!(
        query_pipe_lines(&mut conn_from_c, "PRAGMA integrity_check;"),
        vec!["ok"]
    );
    assert_eq!(
        query_pipe_lines(
            &mut conn_from_c,
            "SELECT k1, k2, keep_val, tail_def FROM t_wor ORDER BY k1, k2;"
        ),
        vec!["alpha|1|v1|77", "beta|2|v2|77", "gamma|3|v3|88"]
    );
    conn_from_c
        .execute_batch("ALTER TABLE t_wor DROP COLUMN keep_val;")
        .unwrap();
    drop(conn_from_c);
    let (c_recheck_ok, c_recheck_out, _) = run_c_sqlite(
        c_db.to_str().unwrap(),
        "PRAGMA integrity_check; SELECT k1, k2, tail_def FROM t_wor ORDER BY k1, k2;",
    );
    assert!(c_recheck_ok);
    assert_eq!(c_recheck_out, "ok\nalpha|1|77\nbeta|2|77\ngamma|3|88");

    // 3. All `ALTER TABLE DROP COLUMN` rejection cases + atomic data/schema rollback verification!
    let mut rconn = Connection::open_in_memory().unwrap();
    rconn
        .execute_batch(
            "CREATE TABLE rej_pk(a INT PRIMARY KEY, b INT);
             CREATE TABLE rej_tpk(a INT, b INT, PRIMARY KEY(a, b));
             CREATE TABLE rej_uq(a INT UNIQUE, b INT);
             CREATE TABLE rej_tuq(a INT, b INT, UNIQUE(a, b));
             CREATE TABLE rej_idx(a INT, b INT);
             CREATE INDEX idx_rej_a ON rej_idx(a);
             CREATE TABLE rej_eidx(a INT, b INT);
             CREATE INDEX idx_rej_expr ON rej_eidx(a + 1);
             CREATE TABLE rej_pidx(a INT, b INT);
             CREATE INDEX idx_rej_part ON rej_pidx(b) WHERE a > 0;
             CREATE TABLE rej_chk_other(a INT, b INT CHECK(b > a));
             CREATE TABLE rej_chk_tbl(a INT, b INT, CHECK(a + b > 0));
             CREATE TABLE rej_gen_dep(a INT, c INT, g INT GENERATED ALWAYS AS (a * 2) VIRTUAL);
             CREATE TABLE rej_all_gen(a INT, g INT GENERATED ALWAYS AS (10) VIRTUAL);
             CREATE TABLE rej_tfk(x INT, pid INT, FOREIGN KEY(pid) REFERENCES rej_pk(a));
             CREATE TABLE rej_one(only_col INT);
             CREATE TABLE rej_rowid_col(rowid INT, b INT);
             CREATE VIEW rej_view AS SELECT a, b FROM rej_idx;

             CREATE TABLE rollback_target(a INT, b INT, c TEXT);
             INSERT INTO rollback_target VALUES (10, 20, 'survives'), (30, 40, 'intact');
             CREATE TEMP VIEW tv_dep AS SELECT a + b AS ab FROM rollback_target;",
        )
        .unwrap();

    for (bad_sql, expected_err) in [
        (
            "ALTER TABLE rej_pk DROP COLUMN a;",
            "cannot drop PRIMARY KEY column: \"a\"",
        ),
        (
            "ALTER TABLE rej_tpk DROP COLUMN a;",
            "cannot drop PRIMARY KEY column: \"a\"",
        ),
        (
            "ALTER TABLE rej_uq DROP COLUMN a;",
            "cannot drop UNIQUE column: \"a\"",
        ),
        (
            "ALTER TABLE rej_tuq DROP COLUMN a;",
            "error in table rej_tuq after drop column",
        ),
        (
            "ALTER TABLE rej_idx DROP COLUMN a;",
            "error in index idx_rej_a after drop column",
        ),
        (
            "ALTER TABLE rej_eidx DROP COLUMN a;",
            "error in index idx_rej_expr after drop column",
        ),
        (
            "ALTER TABLE rej_pidx DROP COLUMN a;",
            "error in index idx_rej_part after drop column",
        ),
        (
            "ALTER TABLE rej_chk_other DROP COLUMN a;",
            "error in table rej_chk_other after drop column",
        ),
        (
            "ALTER TABLE rej_chk_tbl DROP COLUMN a;",
            "error in table rej_chk_tbl after drop column",
        ),
        (
            "ALTER TABLE rej_gen_dep DROP COLUMN a;",
            "error in table rej_gen_dep after drop column",
        ),
        (
            "ALTER TABLE rej_all_gen DROP COLUMN a;",
            "error in table rej_all_gen after drop column: must have at least one non-generated column",
        ),
        (
            "ALTER TABLE rej_tfk DROP COLUMN pid;",
            "error in table rej_tfk after drop column: unknown column \"pid\" in foreign key definition",
        ),
        (
            "ALTER TABLE rej_one DROP COLUMN only_col;",
            "cannot drop column \"only_col\": no other columns exist",
        ),
        (
            "ALTER TABLE rej_one DROP COLUMN missing_col;",
            "no such column: \"missing_col\"",
        ),
        (
            "ALTER TABLE rej_pk DROP COLUMN rowid;",
            "no such column: \"rowid\"",
        ),
        (
            "ALTER TABLE rej_rowid_col DROP COLUMN rowid;",
            "cannot drop rowid alias: rowid",
        ),
        (
            "ALTER TABLE rej_view DROP COLUMN a;",
            "cannot drop column from view \"rej_view\"",
        ),
        (
            "ALTER TABLE rollback_target DROP COLUMN a;",
            "error in view tv_dep after drop column",
        ),
    ] {
        assert_err_msg(&mut rconn, bad_sql, expected_err);
    }

    // Verify `rollback_target` data, schema, and `tv_dep` are 100% untouched after failed DROP COLUMN!
    assert_eq!(
        query_pipe_lines(
            &mut rconn,
            "SELECT a, b, c FROM rollback_target ORDER BY a;"
        ),
        vec!["10|20|survives", "30|40|intact"]
    );
    assert_eq!(
        query_pipe_lines(&mut rconn, "SELECT ab FROM tv_dep ORDER BY ab;"),
        vec!["30", "70"]
    );
    assert_eq!(
        query_pipe_lines(&mut rconn, "PRAGMA integrity_check;"),
        vec!["ok"]
    );

    // With `PRAGMA legacy_alter_table = ON`, dropping `a` from `rollback_target` skips view validation and succeeds!
    rconn
        .execute_batch("PRAGMA legacy_alter_table = ON; ALTER TABLE rollback_target DROP COLUMN a;")
        .unwrap();
    assert_eq!(
        query_pipe_lines(&mut rconn, "SELECT b, c FROM rollback_target ORDER BY b;"),
        vec!["20|survives", "40|intact"]
    );
}

#[test]
fn audit_parity_catalog_names_writable_schema_and_legacy_pre_alter_views() {
    // 1. Canonical catalog table names (`sqlite_master` / `sqlite_temp_master`) in ALTER TABLE & DROP TABLE
    let conn = Connection::open_in_memory().unwrap();
    for (sql, expected) in [
        (
            "ALTER TABLE sqlite_schema DROP COLUMN sql;",
            "table sqlite_master may not be altered",
        ),
        (
            "ALTER TABLE SqLiTe_MaStEr RENAME TO x;",
            "table sqlite_master may not be altered",
        ),
        (
            "ALTER TABLE sqlite_temp_schema DROP COLUMN sql;",
            "table sqlite_temp_master may not be altered",
        ),
        (
            "ALTER TABLE temp.sqlite_schema DROP COLUMN sql;",
            "table sqlite_temp_master may not be altered",
        ),
        (
            "DROP TABLE sqlite_schema;",
            "table sqlite_master may not be dropped",
        ),
        (
            "DROP TABLE IF EXISTS sqlite_schema;",
            "table sqlite_master may not be dropped",
        ),
        (
            "DROP TABLE sqlite_temp_schema;",
            "table sqlite_temp_master may not be dropped",
        ),
        (
            "DROP TABLE IF EXISTS temp.sqlite_schema;",
            "table sqlite_temp_master may not be dropped",
        ),
        (
            "DROP TABLE main.sqlite_temp_schema;",
            "no such table: main.sqlite_temp_schema",
        ),
        (
            "DROP VIEW main.sqlite_temp_schema;",
            "no such view: main.sqlite_temp_schema",
        ),
    ] {
        let err = conn.execute_batch(sql).unwrap_err();
        assert_eq!(err.code, ErrorCode::Error, "unexpected code for {sql}");
        assert_eq!(err.message, expected, "unexpected message for {sql}");
    }
    conn.execute_batch("DROP TABLE IF EXISTS main.sqlite_temp_schema;")
        .unwrap();
    conn.execute_batch("DROP VIEW IF EXISTS main.sqlite_temp_schema;")
        .unwrap();

    // 2. `writable_schema` live `sqlite_schema` mutation & `dropColumnFunc` table-DDL check (`alterdropcol-6.0` & `6.1`)
    for replacement_sql in [
        "CREATE INDEX t1b ON t1(b)",
        "CREATE VIEW t2(x,y,z) AS SELECT b,a,c FROM t1",
    ] {
        let wconn = Connection::open_in_memory().unwrap();
        wconn
            .execute_batch(&format!(
                "CREATE TABLE t1(a,b,c);
                 CREATE TABLE t2(x,y,z);
                 PRAGMA writable_schema=ON;
                 UPDATE sqlite_schema SET sql='{replacement_sql}' WHERE name='t2';
                 PRAGMA writable_schema=OFF;"
            ))
            .unwrap();
        let err = wconn
            .execute_batch("ALTER TABLE t2 DROP COLUMN z;")
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Corrupt);
        assert_eq!(err.message, "database disk image is malformed");
    }

    // 3. Pre-alter view check under `PRAGMA legacy_alter_table = ON` vs `OFF` and `writable_schema = ON`
    let mut lconn = Connection::open_in_memory().unwrap();
    lconn
        .execute_batch(
            "CREATE TABLE t1(a, b, c);
             CREATE TABLE t2(x, y, z);
             CREATE VIEW v1 AS SELECT a, b FROM t1;
             PRAGMA legacy_alter_table = ON;
             -- First DROP COLUMN breaks `v1`, which is allowed post-alter under `legacy_alter_table = ON`:
             ALTER TABLE t1 DROP COLUMN b;",
        )
        .unwrap();

    // Second DROP COLUMN and RENAME COLUMN must fail pre-alter via `renameFixQuotes` with `SQL logic error`:
    let err_drop = lconn
        .execute_batch("ALTER TABLE t2 DROP COLUMN z;")
        .unwrap_err();
    assert_eq!(err_drop.code, ErrorCode::Error);
    assert_eq!(err_drop.message, "SQL logic error");

    let err_ren_col = lconn
        .execute_batch("ALTER TABLE t2 RENAME COLUMN z TO z2;")
        .unwrap_err();
    assert_eq!(err_ren_col.code, ErrorCode::Error);
    assert_eq!(err_ren_col.message, "SQL logic error");

    // `RENAME TO` under `legacy_alter_table = ON` does NOT resolve views and succeeds:
    lconn
        .execute_batch("ALTER TABLE t2 RENAME TO t2_renamed;")
        .unwrap();

    // Switching `legacy_alter_table = OFF` surfaces the detailed `"error in view v1: ..."` pre-alter diagnostic:
    lconn
        .execute_batch("PRAGMA legacy_alter_table = OFF;")
        .unwrap();
    for sql in [
        "ALTER TABLE t2_renamed DROP COLUMN z;",
        "ALTER TABLE t2_renamed RENAME COLUMN z TO z2;",
        "ALTER TABLE t2_renamed RENAME TO t2;",
    ] {
        let err = lconn.execute_batch(sql).unwrap_err();
        assert_eq!(err.code, ErrorCode::Error);
        assert_eq!(err.message, "error in view v1: no such column: b");
    }

    // Enabling `PRAGMA writable_schema = ON` suppresses pre-alter view errors during `DROP COLUMN`:
    lconn
        .execute_batch(
            "PRAGMA legacy_alter_table = ON;
             PRAGMA writable_schema = ON;
             ALTER TABLE t2_renamed DROP COLUMN z;",
        )
        .unwrap();
    assert_eq!(
        query_pipe_lines(
            &mut lconn,
            "SELECT sql FROM sqlite_schema WHERE name = 't2_renamed';"
        ),
        vec!["CREATE TABLE \"t2_renamed\"(x, y)"]
    );
}
