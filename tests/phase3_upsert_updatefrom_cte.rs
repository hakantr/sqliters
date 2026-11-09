//! Phase 3 — Step 3-B Integration & C SQLite 3.54.1 Differential Suite:
//! Chained `UPSERT`, Shared `MutationContext` Core, `UPDATE ... FROM ...`,
//! Row-Value `SET (c1, c2) = (...)`, and Full `WITH [RECURSIVE] ... [[NOT] MATERIALIZED]`.
//!
//! Covers:
//! 1. Single and multi-clause chained `UPSERT` (`DO NOTHING` + `DO UPDATE`) across rowid PK
//!    (`INTEGER PRIMARY KEY` and implicit `rowid`), `WITHOUT ROWID` composite PK, inline/explicit
//!    `UNIQUE` indexes, expression indexes, partial `WHERE` indexes, collation-sensitive conflict
//!    targets, `INSERT INTO tbl AS alias`, `CREATE TABLE excluded` shadowing, `excluded.<col>` and
//!    generated columns, `DO UPDATE ... WHERE`, `INSERT OR REPLACE` + `ON CONFLICT`, `OE_Abort`
//!    conflict escalation, FK enforcement, `AUTOINCREMENT`, `changes()`, `PRAGMA count_changes`,
//!    `last_insert_rowid()`, and `RETURNING`.
//! 2. Shared `MutationContext` / `MutationOrigin` pipeline tracing across `TopLevelInsert`,
//!    `TopLevelUpdate`, `TopLevelDelete`, `UpsertDoUpdate`, `ReplaceConflictDelete`,
//!    `FkCascadeDelete`, and `FkActionUpdate`.
//! 3. `UPDATE ... FROM ...` (joins, views, CTEs, `JOIN ON` scoping, self-joins, multi-match target
//!    rows on both rowid and `WITHOUT ROWID` tables, PK/rowid updates, attached multi-DB joins,
//!    conflict clauses, `RETURNING`) and row-value `SET (c1, c2) = (...)` in both `UPDATE` and
//!    `UPSERT DO UPDATE`.
//! 4. Full `WITH [RECURSIVE]` (`UNION` vs `UNION ALL`, collations, `NULL` and numeric dedup,
//!    multi-arm recursive CTEs, graph cycle traversal, priority-queue `ORDER BY` BFS/DFS +
//!    `LIMIT`/`OFFSET`, `MATERIALIZED` vs `NOT MATERIALIZED` evaluation semantics, soft keyword
//!    `materialized`, forbidden recursive construct errors, outer `LIMIT` short-circuiting,
//!    nested CTE depth guard, and `InterruptHandle` cancellation of unbounded recursion).
//! 5. Bidirectional `.db` file exchange + `PRAGMA integrity_check` with C SQLite 3.54.1 CLI.
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use sqliters::{
    ConflictAction, Connection, ErrorCode, FunctionFlags, MutationOrigin, RuntimeValue, Value,
    parse_one,
};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct Step3BSandbox {
    dir: PathBuf,
}

impl Step3BSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_step3b_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create step3b temp sandbox");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Step3BSandbox {
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
            let tmp_o = ref_root.join(format!("sqlite3_step3b.{}.o.tmp", std::process::id()));
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

        let tmp_bin = ref_root.join(format!("sqlite3_step3b.{}.bin.tmp", std::process::id()));
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

fn query_pipe_lines(conn: &Connection, sql: &str) -> Vec<String> {
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

fn assert_err_msg(conn: &Connection, sql: &str, expected_substr: &str) {
    let err = conn
        .execute_batch(sql)
        .expect_err(&format!("expected error for `{sql}`, but succeeded"));
    assert!(
        err.message.contains(expected_substr),
        "for `{sql}` expected error containing {expected_substr:?}, got: {:?}",
        err.message
    );
}

fn assert_differential_in_memory(script: &str) {
    let conn = Connection::open_in_memory().unwrap();
    let mut rust_lines = Vec::new();
    let mut rem = script;
    while !rem.trim().is_empty() {
        let (stmt_opt, rest) = parse_one(rem).expect("split SQL script");
        let chunk = &rem[..rem.len() - rest.len()];
        rem = rest;
        if stmt_opt.is_none() {
            continue;
        }
        let res = conn
            .query(chunk, &[])
            .unwrap_or_else(|e| panic!("rust failed on `{chunk}`: {e}"));
        for row in res.rows {
            rust_lines.push(
                row.values()
                    .iter()
                    .map(format_val_pipe)
                    .collect::<Vec<_>>()
                    .join("|"),
            );
        }
    }

    let (c_ok, c_out, c_err) = run_c_sqlite(":memory:", script);
    assert!(c_ok, "C sqlite3 failed on script:\n{c_err}\nSQL:\n{script}");
    assert_eq!(
        rust_lines.join("\n"),
        c_out,
        "differential mismatch!\nSQL:\n{script}"
    );
}

#[test]
fn upsert_grammar_target_validation_and_returning_restrictions() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            sku TEXT COLLATE NOCASE UNIQUE,
            code INT,
            qty INT DEFAULT 0,
            tag TEXT
        );
        CREATE UNIQUE INDEX idx_items_code_partial ON items(code) WHERE code > 100;
        CREATE UNIQUE INDEX idx_items_expr ON items(lower(tag), (qty + 1));",
    )
    .unwrap();

    // 1. DEFAULT VALUES does not accept ON CONFLICT
    assert_err_msg(
        &conn,
        "INSERT INTO items DEFAULT VALUES ON CONFLICT DO NOTHING;",
        "syntax error",
    );

    // 2. Non-final ON CONFLICT clause MUST have a conflict target
    assert_err_msg(
        &conn,
        "INSERT INTO items(id) VALUES(1) ON CONFLICT DO NOTHING ON CONFLICT(id) DO NOTHING;",
        "syntax error",
    );

    // 3. `INSERT INTO items SELECT * FROM src ON CONFLICT ...` without WHERE parses ON as JOIN ON
    conn.execute_batch("CREATE TABLE src(a INT);").unwrap();
    assert_err_msg(
        &conn,
        "INSERT INTO items(id) SELECT a FROM src ON CONFLICT(id) DO NOTHING;",
        "near \"DO\": syntax error",
    );
    // With `WHERE true`, `ON CONFLICT` parses as UPSERT cleanly!
    conn.execute_batch(
        "INSERT INTO items(id) SELECT a FROM src WHERE true ON CONFLICT(id) DO NOTHING;",
    )
    .unwrap();

    // 4. Target validation: non-unique column, duplicate target column, collation mismatch, partial index WHERE mismatch
    assert_err_msg(
        &conn,
        "INSERT INTO items(id, qty) VALUES(1, 5) ON CONFLICT(qty) DO NOTHING;",
        "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint",
    );
    assert_err_msg(
        &conn,
        "INSERT INTO items(id, sku) VALUES(1, 'A') ON CONFLICT(sku COLLATE BINARY) DO NOTHING;",
        "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint",
    );
    // Matching NOCASE explicitly or implicitly succeeds:
    conn.execute_batch(
        "INSERT INTO items(id, sku, code, qty, tag) VALUES(1, 'AbC', 200, 10, 'Hello')
         ON CONFLICT(sku COLLATE NOCASE) DO UPDATE SET qty = qty + 1;",
    )
    .unwrap();

    // Partial index requires matching WHERE clause
    assert_err_msg(
        &conn,
        "INSERT INTO items(id, code) VALUES(2, 200) ON CONFLICT(code) DO NOTHING;",
        "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint",
    );
    assert_err_msg(
        &conn,
        "INSERT INTO items(id, code) VALUES(2, 200) ON CONFLICT(code) WHERE code > 50 DO NOTHING;",
        "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint",
    );
    conn.execute_batch(
        "INSERT INTO items(id, sku, code, qty, tag) VALUES(2, 'Xyz', 200, 5, 'World')
         ON CONFLICT(code) WHERE code > 100 DO UPDATE SET qty = items.qty + excluded.qty;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&conn, "SELECT id, sku, code, qty FROM items;"),
        vec!["1|AbC|200|15"]
    );

    // Expression index target matching (order-independent!)
    conn.execute_batch(
        "INSERT INTO items(id, sku, code, qty, tag) VALUES(3, 'New', 50, 15, 'HELLO')
         ON CONFLICT((qty + 1), lower(tag)) DO UPDATE SET tag = 'matched_expr';",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&conn, "SELECT id, tag FROM items;"),
        vec!["1|matched_expr"]
    );

    // 5. Scope rules: `INSERT INTO items AS it` hides `items.qty`, allows `excluded.rowid` on rowid tables,
    //    and rejects `excluded.rowid` on WITHOUT ROWID tables!
    assert_err_msg(
        &conn,
        "INSERT INTO items AS it(id, qty) VALUES(1, 9) ON CONFLICT(id) DO UPDATE SET qty = items.qty + 1;",
        "no such column: items.qty",
    );
    conn.execute_batch(
        "INSERT INTO items(id, qty) VALUES(1, 9) ON CONFLICT(id) DO UPDATE SET qty = excluded.rowid;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&conn, "SELECT id, qty FROM items;"),
        vec!["1|1"]
    );
    conn.execute_batch("CREATE TABLE wor_only(k INT PRIMARY KEY, qty INT) WITHOUT ROWID;")
        .unwrap();
    assert_err_msg(
        &conn,
        "INSERT INTO wor_only VALUES(1, 9) ON CONFLICT(k) DO UPDATE SET qty = excluded.rowid;",
        "no such column: excluded.rowid",
    );

    // 6. RETURNING restrictions on UPSERT: `RETURNING *` ok; `tbl.*`, `alias.*`, `excluded.*`, `excluded.col` rejected
    assert_err_msg(
        &conn,
        "INSERT INTO items(id, qty) VALUES(1, 9) ON CONFLICT(id) DO NOTHING RETURNING items.*;",
        "RETURNING may not use \"TABLE.*\" wildcards",
    );
    assert_err_msg(
        &conn,
        "INSERT INTO items AS it(id, qty) VALUES(1, 9) ON CONFLICT(id) DO NOTHING RETURNING it.*;",
        "RETURNING may not use \"TABLE.*\" wildcards",
    );
    assert_err_msg(
        &conn,
        "INSERT INTO items(id, qty) VALUES(1, 9) ON CONFLICT(id) DO NOTHING RETURNING excluded.*;",
        "RETURNING may not use \"TABLE.*\" wildcards",
    );
    assert_err_msg(
        &conn,
        "INSERT INTO items(id, qty) VALUES(1, 9) ON CONFLICT(id) DO UPDATE SET qty = 1 RETURNING excluded.qty;",
        "no such column: excluded.qty",
    );
}

#[test]
fn upsert_chained_multi_clause_without_rowid_generated_and_differential() {
    let script = "
        PRAGMA foreign_keys = ON;
        CREATE TABLE parents(pid INT PRIMARY KEY);
        INSERT INTO parents VALUES (10), (20);

        CREATE TABLE inv (
            k1 INT,
            k2 TEXT COLLATE NOCASE,
            u1 INT UNIQUE,
            u2 INT,
            pid INT REFERENCES parents(pid),
            base INT NOT NULL DEFAULT 3,
            doubled INT GENERATED ALWAYS AS (base * 2) STORED,
            tripled INT GENERATED ALWAYS AS (base * 3) VIRTUAL,
            note TEXT DEFAULT 'init',
            PRIMARY KEY(k1, k2)
        ) WITHOUT ROWID;
        CREATE UNIQUE INDEX idx_inv_u2 ON inv(u2) WHERE u2 IS NOT NULL;

        INSERT INTO inv(k1, k2, u1, u2, pid, base, note) VALUES
            (1, 'Alpha', 100, 500, 10, 4, 'seed1'),
            (2, 'Beta',  200, 600, 20, 5, 'seed2');

        -- Chained multi-clause UPSERT:
        -- Clause 1 matches PK(k1, k2) with DO UPDATE WHERE guard
        -- Clause 2 matches partial index idx_inv_u2
        -- Clause 3 catch-all DO NOTHING handles u1 conflict
        INSERT INTO inv AS t(k1, k2, u1, u2, pid, base, note) VALUES
            (1, 'ALPHA', 999, 999, 10, 7, 'pk_upd'),
            (2, 'BETA',  888, 888, 20, 2, 'pk_skip_where'),
            (9, 'Gamma', 777, 600, 10, 9, 'u2_partial_upd'),
            (8, 'Delta', 100, 700, 10, 6, 'u1_catchall_nothing'),
            (3, 'Eps',   300, NULL, 20, 8, 'fresh_insert')
        ON CONFLICT(k2, k1) DO UPDATE
            SET (base, note) = (excluded.doubled + t.base, t.note || '->' || excluded.note || ':' || excluded.tripled)
            WHERE excluded.base > t.base
        ON CONFLICT(u2) WHERE u2 IS NOT NULL DO UPDATE
            SET note = t.note || '+u2(' || excluded.base || ')'
        ON CONFLICT DO NOTHING
        RETURNING k1, k2, base, doubled, tripled, note;

        SELECT 'changes', changes(), 'total', total_changes();
        SELECT k1, k2, u1, u2, base, doubled, tripled, note FROM inv ORDER BY k1;
    ";
    assert_differential_in_memory(script);

    // Verify NOT NULL, CHECK, STRICT, and FK errors are NOT swallowed by ON CONFLICT DO NOTHING,
    // and DO UPDATE enforces OE_Abort even when outer INSERT specifies OR IGNORE / OR REPLACE!
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE p(id INT PRIMARY KEY);
         INSERT INTO p VALUES (1);
         CREATE TABLE c(
             id INT PRIMARY KEY,
             u INT UNIQUE,
             nn INT NOT NULL CHECK(nn > 0),
             pid INT REFERENCES p(id)
         ) STRICT;
         INSERT INTO c VALUES (1, 10, 5, 1), (2, 20, 6, 1);",
    )
    .unwrap();

    assert_err_msg(
        &conn,
        "INSERT INTO c VALUES (3, 30, NULL, 1) ON CONFLICT DO NOTHING;",
        "NOT NULL constraint failed: c.nn",
    );
    assert_err_msg(
        &conn,
        "INSERT INTO c VALUES (3, 30, -1, 1) ON CONFLICT DO NOTHING;",
        "CHECK constraint failed",
    );
    assert_err_msg(
        &conn,
        "INSERT INTO c VALUES (3, 30, 'not_an_int', 1) ON CONFLICT DO NOTHING;",
        "cannot store TEXT value in INT column c.nn",
    );
    assert_err_msg(
        &conn,
        "INSERT INTO c VALUES (3, 30, 5, 999) ON CONFLICT DO NOTHING;",
        "FOREIGN KEY constraint failed",
    );
    // Secondary UNIQUE collision inside DO UPDATE escalates to ABORT even under INSERT OR IGNORE:
    assert_err_msg(
        &conn,
        "INSERT OR IGNORE INTO c VALUES (1, 99, 5, 1) ON CONFLICT(id) DO UPDATE SET u = 20;",
        "UNIQUE constraint failed: c.u",
    );

    // Table literally named `excluded` shadows `excluded.b` to target row unless aliased (`AS tgt`):
    conn.execute_batch(
        "CREATE TABLE excluded(a INT PRIMARY KEY, b INT); INSERT INTO excluded VALUES (1, 10);",
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO excluded VALUES (1, 20) ON CONFLICT(a) DO UPDATE SET b = excluded.b + 1;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&conn, "SELECT a, b FROM excluded;"),
        vec!["1|11"]
    );
    conn.execute_batch(
        "INSERT INTO excluded AS tgt VALUES (1, 20) ON CONFLICT(a) DO UPDATE SET b = tgt.b + excluded.b;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(&conn, "SELECT a, b FROM excluded;"),
        vec!["1|31"]
    );

    // Verify PRAGMA count_changes=ON counts only newly inserted rows while changes() counts all changed rows:
    conn.execute_batch(
        "CREATE TABLE cc(a TEXT UNIQUE, b INT DEFAULT 1);
         INSERT INTO cc(a) VALUES ('one'), ('two'), ('three');
         PRAGMA count_changes = ON;",
    )
    .unwrap();
    assert_eq!(
        query_pipe_lines(
            &conn,
            "INSERT INTO cc(a) VALUES ('one'), ('one'), ('three'), ('four')
             ON CONFLICT(a) DO UPDATE SET b = b + 1;"
        ),
        vec!["1"]
    );
    conn.execute_batch("PRAGMA count_changes = OFF;").unwrap();
    assert_eq!(query_pipe_lines(&conn, "SELECT changes();"), vec!["4"]);
    assert_eq!(
        query_pipe_lines(&conn, "SELECT a, b FROM cc ORDER BY a;"),
        vec!["four|1", "one|3", "three|2", "two|1"]
    );
}

#[test]
fn mutation_context_pipeline_traces_all_dml_upsert_replace_and_fk_origins() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE dept (
             id INTEGER PRIMARY KEY,
             code TEXT UNIQUE,
             title TEXT
         );
         CREATE TABLE emp (
             id INTEGER PRIMARY KEY,
             dept_id INT REFERENCES dept(id) ON DELETE CASCADE ON UPDATE CASCADE,
             mgr_dept INT REFERENCES dept(id) ON DELETE SET NULL,
             name TEXT
         );
         CREATE TABLE kv_wor (
             k TEXT PRIMARY KEY,
             v INT
         ) WITHOUT ROWID;",
    )
    .unwrap();

    conn.set_mutation_trace_enabled(true).unwrap();

    // 1. TopLevelInsert (rowid + WITHOUT ROWID)
    conn.execute_batch(
        "INSERT INTO dept VALUES (1, 'ENG', 'Engineering'), (2, 'OPS', 'Operations');
         INSERT INTO emp VALUES (100, 1, 2, 'Ada');
         INSERT INTO kv_wor VALUES ('k1', 10);",
    )
    .unwrap();
    let evs = conn.take_mutation_trace().unwrap();
    assert_eq!(evs.len(), 4);
    assert_eq!(evs[0].origin, MutationOrigin::TopLevelInsert);
    assert_eq!(evs[0].table_name, "dept");
    assert!(evs[0].old_row.is_none());
    assert_eq!(evs[0].new_row.as_ref().unwrap().rowid, 1);
    assert!(evs[0].emit_update_hook);
    assert!(!evs[0].invalidate_blobs);
    // WITHOUT ROWID does not emit SQLite rowid update hook:
    assert_eq!(evs[3].table_name, "kv_wor");
    assert_eq!(evs[3].origin, MutationOrigin::TopLevelInsert);
    assert!(!evs[3].emit_update_hook);

    // 2. UpsertDoUpdate carries old_row, new_row, excluded_row, and ConflictAction::Abort
    conn.execute_batch(
        "INSERT OR IGNORE INTO dept VALUES (1, 'ENG', 'Core Engineering')
         ON CONFLICT(id) DO UPDATE SET title = excluded.title;",
    )
    .unwrap();
    let evs = conn.take_mutation_trace().unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].origin, MutationOrigin::UpsertDoUpdate);
    assert_eq!(evs[0].effective_conflict, ConflictAction::Abort);
    assert_eq!(
        evs[0].old_row.as_ref().unwrap().values[2],
        Value::Text("Engineering".to_owned())
    );
    assert_eq!(
        evs[0].excluded_row.as_ref().unwrap().values[2],
        Value::Text("Core Engineering".to_owned())
    );
    assert_eq!(
        evs[0].new_row.as_ref().unwrap().values[2],
        Value::Text("Core Engineering".to_owned())
    );
    assert!(evs[0].invalidate_blobs);

    // 3. ReplaceConflictDelete on both rowid and WITHOUT ROWID tables
    conn.execute_batch(
        "INSERT OR REPLACE INTO kv_wor VALUES ('k1', 99);
         INSERT OR REPLACE INTO dept VALUES (3, 'OPS', 'Ops Replaced');",
    )
    .unwrap();
    let evs = conn.take_mutation_trace().unwrap();
    // kv_wor: ReplaceConflictDelete + TopLevelInsert
    // dept(2,'OPS') deleted via ReplaceConflictDelete -> triggers FkActionUpdate (SET NULL on emp.mgr_dept) -> TopLevelInsert dept(3)
    let origins: Vec<(&str, MutationOrigin)> = evs
        .iter()
        .map(|e| (e.table_name.as_str(), e.origin))
        .collect();
    assert_eq!(
        origins,
        vec![
            ("kv_wor", MutationOrigin::ReplaceConflictDelete),
            ("kv_wor", MutationOrigin::TopLevelInsert),
            ("dept", MutationOrigin::ReplaceConflictDelete),
            ("emp", MutationOrigin::FkActionUpdate),
            ("dept", MutationOrigin::TopLevelInsert),
        ]
    );

    // 4. TopLevelUpdate changing PK cascades via FkActionUpdate, then TopLevelDelete cascades via FkCascadeDelete
    conn.execute_batch("UPDATE dept SET id = 10 WHERE id = 1;")
        .unwrap();
    let evs = conn.take_mutation_trace().unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0].origin, MutationOrigin::TopLevelUpdate);
    assert!(evs[0].invalidate_blobs); // rowid changed 1 -> 10!
    assert_eq!(evs[1].origin, MutationOrigin::FkActionUpdate);
    assert_eq!(evs[1].table_name, "emp");

    conn.execute_batch("DELETE FROM dept WHERE id = 10;")
        .unwrap();
    let evs = conn.take_mutation_trace().unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0].origin, MutationOrigin::TopLevelDelete);
    assert_eq!(evs[0].table_name, "dept");
    assert!(evs[0].invalidate_blobs);
    assert_eq!(evs[1].origin, MutationOrigin::FkCascadeDelete);
    assert_eq!(evs[1].table_name, "emp");
    assert_eq!(
        query_pipe_lines(&conn, "SELECT count(*) FROM emp;"),
        vec!["0"]
    );
}

#[test]
fn update_from_and_row_value_set_semantics_scoping_multidb_and_differential() {
    let script = "
        CREATE TABLE accounts (
            id INTEGER PRIMARY KEY,
            owner TEXT,
            bal INT,
            tier TEXT
        );
        INSERT INTO accounts VALUES
            (1, 'Ada',   100, 'bronze'),
            (2, 'Grace', 250, 'silver'),
            (3, 'Linus',  50, 'bronze');

        -- Simultaneous row-value swap + 1-column parenthesized SET + 0-row subquery yielding NULLs
        UPDATE accounts SET (bal, owner) = (bal + 50, upper(owner)), (tier) = ('gold') WHERE id = 2;
        UPDATE accounts SET (owner, tier) = (SELECT 'nobody', 'none' WHERE 0) WHERE id = 3;
        SELECT id, owner, bal, tier FROM accounts ORDER BY id;

        -- UPDATE ... FROM with CTE, JOIN USING, self-join, and multi-match target rows on rowid & WITHOUT ROWID
        CREATE TABLE adjustments (acc_id INT, delta INT, badge TEXT);
        INSERT INTO adjustments VALUES
            (1, 40, 'silver'),
            (2, 50, 'platinum');

        WITH bonus(acc_id, extra) AS MATERIALIZED (
            VALUES (1, 10), (2, 25)
        )
        UPDATE accounts AS a
           SET (bal, tier) = (a.bal + adj.delta + b.extra, adj.badge)
          FROM adjustments AS adj
          JOIN bonus AS b USING (acc_id)
         WHERE a.id = acc_id
        RETURNING accounts.id, owner, bal, tier;

        -- WITHOUT ROWID PK update via UPDATE ... FROM + self-join
        CREATE TABLE wor_items (
            grp TEXT,
            seq INT,
            label TEXT,
            PRIMARY KEY(grp, seq)
        ) WITHOUT ROWID;
        INSERT INTO wor_items VALUES ('A', 1, 'first'), ('A', 2, 'second'), ('B', 1, 'solo');

        UPDATE wor_items AS tgt
           SET (seq, label) = (tgt.seq + 10, tgt.label || '#' || src.label)
          FROM wor_items AS src
         WHERE tgt.grp = src.grp AND src.seq = 1 AND tgt.grp = 'A'
        RETURNING grp, seq, label;

        SELECT grp, seq, label FROM wor_items ORDER BY grp, seq;
    ";
    assert_differential_in_memory(script);

    // Verify single subquery evaluation per updated row in SET (c1, c2, c3) = (SELECT ...)
    let conn = Connection::open_in_memory().unwrap();
    let eval_counter = Arc::new(AtomicU64::new(0));
    let counter_cb = Arc::clone(&eval_counter);
    conn.register_scalar_function("bump_eval", 1, FunctionFlags::default(), move |args| {
        let n = counter_cb.fetch_add(1, Ordering::SeqCst) + 1;
        let base = args[0].value.as_i64().unwrap_or(0);
        Ok(RuntimeValue::plain(Value::Integer(base + n as i64)))
    })
    .unwrap();

    conn.execute_batch(
        "CREATE TABLE t_once(id INTEGER PRIMARY KEY, x INT, y INT, z INT);
         INSERT INTO t_once VALUES (1, 0, 0, 0), (2, 0, 0, 0);",
    )
    .unwrap();
    conn.execute_batch(
        "UPDATE t_once SET (x, y, z) = (SELECT v, v * 10, v * 100 FROM (SELECT bump_eval(id * 100) AS v));",
    )
    .unwrap();
    assert_eq!(
        eval_counter.load(Ordering::SeqCst),
        2,
        "3-column row-value subquery must execute exactly once per updated row"
    );
    assert_eq!(
        query_pipe_lines(&conn, "SELECT id, x, y, z FROM t_once ORDER BY id;"),
        vec!["1|101|1010|10100", "2|202|2020|20200"]
    );

    // Verify cross-database ATTACH `UPDATE main.t SET ... FROM aux.s`
    let sb = Step3BSandbox::new("update_from_attach");
    let aux_db = sb.path().join("aux.db");
    conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS aux;
         CREATE TABLE aux.rates(id INT PRIMARY KEY, mult INT);
         INSERT INTO aux.rates VALUES (1, 3), (2, 5);
         UPDATE main.t_once SET x = x * r.mult FROM aux.rates AS r WHERE t_once.id = r.id;",
        aux_db.display()
    ))
    .unwrap();
    assert_eq!(
        query_pipe_lines(&conn, "SELECT id, x FROM t_once ORDER BY id;"),
        vec!["1|303", "2|1010"]
    );

    // Verify exact arity, JOIN ON scoping, and RETURNING scoping errors
    assert_err_msg(
        &conn,
        "UPDATE t_once SET (x, y) = (1, 2, 3);",
        "2 columns assigned 3 values",
    );
    assert_err_msg(
        &conn,
        "UPDATE t_once SET (x, y) = (SELECT 1, 2, 3);",
        "2 columns assigned 3 values",
    );
    assert_err_msg(&conn, "UPDATE t_once SET x = (1, 2);", "row value misused");
    assert_err_msg(
        &conn,
        "UPDATE t_once SET x = (SELECT 1, 2);",
        "sub-select returns 2 columns - expected 1",
    );
    // Target table is NOT in scope inside `FROM ... JOIN ... ON` (`SF_NestedFrom`):
    assert_err_msg(
        &conn,
        "UPDATE t_once SET x = 1 FROM aux.rates AS r1 JOIN aux.rates AS r2 ON r1.id = t_once.id;",
        "no such column: t_once.id",
    );
    // `FROM` table columns AND DML target alias (`AS t_al`) are NOT in scope inside `RETURNING`:
    assert_err_msg(
        &conn,
        "UPDATE t_once SET x = 1 FROM aux.rates AS r WHERE t_once.id = r.id RETURNING r.mult;",
        "no such column: r.mult",
    );
    assert_err_msg(
        &conn,
        "UPDATE t_once AS t_al SET x = 1 WHERE t_al.id = 1 RETURNING t_al.x;",
        "no such column: t_al.x",
    );
}

#[test]
fn with_recursive_materialized_soft_keyword_priority_queue_and_interrupt() {
    let script = "
        -- 1. Soft keyword `materialized` usable as table, column, and CTE identifier
        CREATE TABLE materialized (materialized INT PRIMARY KEY, label TEXT);
        INSERT INTO materialized VALUES (1, 'alpha'), (2, 'beta');
        WITH m AS MATERIALIZED (SELECT materialized, label FROM materialized),
             nm AS NOT MATERIALIZED (SELECT materialized + 10 AS k, label FROM m)
        SELECT k, label FROM nm ORDER BY k;

        -- 2. Recursive UNION dedup: NULLs, 1 vs 1.0, COLLATE NOCASE, and graph cycles
        WITH RECURSIVE dedup_test(v) AS (
            VALUES (NULL), (1), (1.0), ('abc')
            UNION
            SELECT CASE WHEN v IS NULL THEN NULL WHEN v = 1 THEN 1.0 ELSE 'ABC' END FROM dedup_test
        )
        SELECT v FROM dedup_test;

        CREATE TABLE edges(u INT, v INT);
        INSERT INTO edges VALUES (1, 2), (2, 3), (3, 1), (2, 4), (4, 5);
        WITH RECURSIVE reach(n) AS (
            SELECT 1
            UNION
            SELECT e.v FROM edges e JOIN reach r ON e.u = r.n
        )
        SELECT n FROM reach ORDER BY n;

        -- 3. Multi-arm recursive CTE + ORDER BY priority queue (BFS vs DFS) + LIMIT/OFFSET
        CREATE TABLE tree(id INT PRIMARY KEY, parent INT, name TEXT);
        INSERT INTO tree VALUES
            (1, NULL, 'root'),
            (2, 1, 'left'),
            (3, 1, 'right'),
            (4, 2, 'll'),
            (5, 2, 'lr'),
            (6, 3, 'rl');

        -- Depth-first (`ORDER BY depth DESC`) vs Breadth-first (`ORDER BY depth ASC`)
        WITH RECURSIVE dfs(id, depth, path) AS (
            SELECT id, 0, name FROM tree WHERE parent IS NULL
            UNION ALL
            SELECT t.id, d.depth + 1, d.path || '/' || t.name
              FROM tree t JOIN dfs d ON t.parent = d.id
            ORDER BY 2 DESC, 1 ASC
        )
        SELECT path FROM dfs;

        WITH RECURSIVE bfs(id, depth, path) AS (
            SELECT id, 0, name FROM tree WHERE parent IS NULL
            UNION ALL
            SELECT t.id, b.depth + 1, b.path || '/' || t.name
              FROM tree t JOIN bfs b ON t.parent = b.id
            ORDER BY 2 ASC, 1 ASC
            LIMIT 4 OFFSET 1
        )
        SELECT path FROM bfs;

        -- 4. Non-MATERIALIZED unbounded recursive CTE with outer LIMIT short-circuits (`withM-1.3`)
        WITH RECURSIVE nat(x) AS (
            SELECT 1
            UNION ALL
            SELECT x + 1 FROM nat
        )
        SELECT x FROM nat WHERE x >= 10 LIMIT 5;
    ";
    assert_differential_in_memory(script);

    // 5. Verify MATERIALIZED vs NOT MATERIALIZED actual evaluation count via stateful scalar callback
    let conn = Connection::open_in_memory().unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let calls_cb = Arc::clone(&calls);
    conn.register_scalar_function("count_cte_eval", 1, FunctionFlags::default(), move |args| {
        calls_cb.fetch_add(1, Ordering::SeqCst);
        Ok(args[0].clone())
    })
    .unwrap();

    calls.store(0, Ordering::SeqCst);
    let mat_res = query_pipe_lines(
        &conn,
        "WITH c AS MATERIALIZED (SELECT count_cte_eval(1) AS x UNION ALL SELECT count_cte_eval(2))
         SELECT a.x, b.x FROM c AS a JOIN c AS b ON a.x = b.x ORDER BY a.x;",
    );
    assert_eq!(mat_res, vec!["1|1", "2|2"]);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "AS MATERIALIZED must evaluate the 2-row CTE body once even when joined twice"
    );

    calls.store(0, Ordering::SeqCst);
    let not_mat_res = query_pipe_lines(
        &conn,
        "WITH c AS NOT MATERIALIZED (SELECT count_cte_eval(1) AS x UNION ALL SELECT count_cte_eval(2))
         SELECT a.x, b.x FROM c AS a JOIN c AS b ON a.x = b.x ORDER BY a.x;",
    );
    assert_eq!(not_mat_res, vec!["1|1", "2|2"]);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "AS NOT MATERIALIZED must re-evaluate the 2-row CTE body per reference"
    );

    // 6. Forbidden recursive CTE constructs & nested CTE expansion depth limit
    assert_err_msg(
        &conn,
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT sum(x) FROM c) SELECT * FROM c;",
        "recursive aggregate queries not supported",
    );
    assert_err_msg(
        &conn,
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT a.x + b.x FROM c a, c b) SELECT * FROM c;",
        "multiple references to recursive table: c",
    );
    assert_err_msg(
        &conn,
        "WITH RECURSIVE c(x) AS (SELECT 1 EXCEPT SELECT x + 1 FROM c) SELECT * FROM c;",
        "circular reference: c",
    );
    assert_err_msg(
        &conn,
        "WITH RECURSIVE c AS NOT MATERIALIZED (
            WITH RECURSIVE c AS NOT MATERIALIZED (
                WITH RECURSIVE c AS NOT MATERIALIZED (
                    WITH RECURSIVE c AS NOT MATERIALIZED (
                        WITH c AS (VALUES(0)) SELECT 1 FROM c
                    ) SELECT 1 FROM c,c,c,c,c,c,c,c,c
                ) SELECT 2 FROM c,c,c,c,c,c,c,c,c
            ) SELECT 3 FROM c,c,c,c,c,c,c,c,c
         ) SELECT 4 FROM c,c,c,c,c,c,c,c,c;",
        "VIEWs and/or subqueries nested too deep",
    );

    // 7. Unbounded recursive CTE cancelled via `InterruptHandle` returns `ErrorCode::Interrupt` promptly
    let handle = conn.interrupt_handle();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(25));
        handle.interrupt();
    });
    let err = conn
        .query(
            "WITH RECURSIVE inf(x) AS MATERIALIZED (SELECT 1 UNION ALL SELECT x + 1 FROM inf)
             SELECT count(*) FROM inf;",
            &[],
        )
        .expect_err("unbounded MATERIALIZED recursive CTE should be interrupted");
    canceller.join().unwrap();
    assert_eq!(err.code, ErrorCode::Interrupt);
}

#[test]
fn bidirectional_db_file_exchange_and_integrity_check_with_c_sqlite() {
    let sb = Step3BSandbox::new("bidir_step3b");
    let rust_db = sb.path().join("from_rust.db");
    let c_db = sb.path().join("from_c.db");

    let setup_and_mutate_sql = "
        CREATE TABLE catalog_items (
            id INTEGER PRIMARY KEY,
            sku TEXT COLLATE NOCASE UNIQUE,
            cat TEXT,
            price INT NOT NULL,
            stock INT NOT NULL DEFAULT 0
        );
        CREATE UNIQUE INDEX idx_cat_sku_high ON catalog_items(cat, sku) WHERE price >= 100;

        CREATE TABLE wor_ledger (
            acct TEXT,
            seq INT,
            amount INT NOT NULL,
            memo TEXT,
            PRIMARY KEY(acct, seq)
        ) WITHOUT ROWID;

        CREATE VIEW v_materialized_summary AS
            WITH totals AS MATERIALIZED (
                SELECT cat, count(*) AS n, sum(stock) AS total_stock
                  FROM catalog_items
                 GROUP BY cat
            )
            SELECT cat, n, total_stock FROM totals;

        INSERT INTO catalog_items(id, sku, cat, price, stock) VALUES
            (1, 'SKU-A', 'books', 120, 10),
            (2, 'SKU-B', 'tools',  80,  5),
            (3, 'SKU-C', 'books', 150,  2);

        INSERT INTO catalog_items AS ci(id, sku, cat, price, stock) VALUES
            (10, 'sku-a', 'books', 130, 7),
            (11, 'SKU-D', 'books', 200, 4)
        ON CONFLICT(cat, sku) WHERE price >= 100 DO UPDATE
            SET (price, stock) = (excluded.price, ci.stock + excluded.stock)
        ON CONFLICT(sku COLLATE NOCASE) DO UPDATE
            SET stock = ci.stock + excluded.stock;

        INSERT INTO wor_ledger VALUES ('main', 1, 500, 'open'), ('main', 2, -120, 'fee');
        UPDATE wor_ledger AS wl
           SET (amount, memo) = (wl.amount + s.total_stock * 10, wl.memo || ':' || s.cat)
          FROM v_materialized_summary AS s
         WHERE s.cat = 'books' AND wl.seq = 1;
    ";

    let verify_sql = "
        PRAGMA integrity_check;
        SELECT id, sku, cat, price, stock FROM catalog_items ORDER BY id;
        SELECT acct, seq, amount, memo FROM wor_ledger ORDER BY acct, seq;
        SELECT cat, n, total_stock FROM v_materialized_summary ORDER BY cat;
    ";

    // Direction A: Write in `sqliters`, verify in C SQLite 3.54.1
    {
        let conn = Connection::open(&rust_db).unwrap();
        conn.execute_batch(setup_and_mutate_sql).unwrap();
        assert_eq!(
            query_pipe_lines(&conn, "PRAGMA integrity_check;"),
            vec!["ok"]
        );
    }
    let (c_ok_a, c_out_a, c_err_a) = run_c_sqlite(rust_db.to_str().unwrap(), verify_sql);
    assert!(c_ok_a, "C sqlite failed reading rust_db: {c_err_a}");
    assert!(
        c_out_a.starts_with("ok\n"),
        "expected PRAGMA integrity_check = ok, got:\n{c_out_a}"
    );

    // Direction B: Write in C SQLite 3.54.1, verify & further mutate in `sqliters`, re-verify in C SQLite
    let (c_ok_b, _, c_err_b) = run_c_sqlite(c_db.to_str().unwrap(), setup_and_mutate_sql);
    assert!(c_ok_b, "C sqlite failed creating c_db: {c_err_b}");

    {
        let conn = Connection::open(&c_db).unwrap();
        let mut rust_verify = Vec::new();
        for q in [
            "PRAGMA integrity_check;",
            "SELECT id, sku, cat, price, stock FROM catalog_items ORDER BY id;",
            "SELECT acct, seq, amount, memo FROM wor_ledger ORDER BY acct, seq;",
            "SELECT cat, n, total_stock FROM v_materialized_summary ORDER BY cat;",
        ] {
            rust_verify.extend(query_pipe_lines(&conn, q));
        }
        assert_eq!(rust_verify.join("\n"), c_out_a);

        // Mutate C-created DB using recursive CTE + UPSERT + ALTER TABLE RENAME view rewrite!
        conn.execute_batch(
            "WITH RECURSIVE seq(n) AS (SELECT 3 UNION ALL SELECT n + 1 FROM seq WHERE n < 6)
             INSERT INTO wor_ledger(acct, seq, amount, memo)
             SELECT 'main', n, n * 10, 'rec' FROM seq WHERE true
             ON CONFLICT(acct, seq) DO UPDATE SET amount = excluded.amount;
             ALTER TABLE catalog_items RENAME COLUMN stock TO on_hand;",
        )
        .unwrap();
        assert_eq!(
            query_pipe_lines(&conn, "PRAGMA integrity_check;"),
            vec!["ok"]
        );
    }

    let (c_ok_post, c_out_post, c_err_post) = run_c_sqlite(
        c_db.to_str().unwrap(),
        "PRAGMA integrity_check;
         SELECT acct, seq, amount, memo FROM wor_ledger ORDER BY seq;
         SELECT cat, n, total_stock FROM v_materialized_summary ORDER BY cat;",
    );
    assert!(
        c_ok_post,
        "C sqlite failed after sqliters mutation: {c_err_post}"
    );
    assert_eq!(
        c_out_post,
        "ok\nmain|1|730|open:books\nmain|2|-120|fee\nmain|3|30|rec\nmain|4|40|rec\nmain|5|50|rec\nmain|6|60|rec\nbooks|3|23\ntools|1|5"
    );
}

#[test]
fn upsert_or_replace_autoincrement_last_rowid_and_prepare_metadata() {
    let script = "
        -- 1. INSERT OR REPLACE combined with targeted ON CONFLICT(a) DO UPDATE:
        --    Conflicts on `a` run DO UPDATE; conflicts on `b` (not targeted) run OR REPLACE!
        CREATE TABLE mix_rep (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            a TEXT UNIQUE,
            b TEXT UNIQUE,
            hits INT DEFAULT 1
        );
        INSERT INTO mix_rep(a, b, hits) VALUES ('a1', 'b1', 10), ('a2', 'b2', 20);
        SELECT 'rowid_before', last_insert_rowid();

        -- Conflict on `a1` -> DO UPDATE (last_insert_rowid unchanged!)
        INSERT OR REPLACE INTO mix_rep(a, b, hits) VALUES ('a1', 'b_unused', 5)
            ON CONFLICT(a) DO UPDATE SET hits = mix_rep.hits + excluded.hits;
        SELECT 'rowid_after_doupdate', last_insert_rowid(), 'changes', changes();

        -- Conflict on `a2` -> DO NOTHING (last_insert_rowid unchanged, changes() = 0!)
        INSERT INTO mix_rep(a, b, hits) VALUES ('a2', 'b_unused', 99)
            ON CONFLICT(a) DO NOTHING;
        SELECT 'rowid_after_donothing', last_insert_rowid(), 'changes', changes();

        -- Conflict on `b2` (not matched by ON CONFLICT(a)) -> falls back to OR REPLACE!
        INSERT OR REPLACE INTO mix_rep(a, b, hits) VALUES ('a_new', 'b2', 77)
            ON CONFLICT(a) DO UPDATE SET hits = 999;
        SELECT 'rowid_after_replace', last_insert_rowid(), 'changes', changes();
        SELECT id, a, b, hits FROM mix_rep ORDER BY id;
        SELECT name, seq FROM sqlite_sequence;
    ";
    assert_differential_in_memory(script);

    // 2. Verify prepare-time `Statement::column_count` and `Statement::column_names` on UPSERT & UPDATE FROM RETURNING
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE pmeta(id INTEGER PRIMARY KEY, k TEXT UNIQUE, v INT);
         CREATE TABLE psrc(k TEXT, bonus INT);",
    )
    .unwrap();
    let up_stmt = conn
        .prepare(
            "INSERT INTO pmeta(id, k, v) VALUES (1, 'x', 10)
             ON CONFLICT(k) DO UPDATE SET v = pmeta.v + excluded.v
             RETURNING id, k, v * 2 AS v2;",
        )
        .unwrap();
    assert_eq!(up_stmt.column_count(), 3);
    assert_eq!(up_stmt.column_names(), &["id", "k", "v2"]);

    let uf_stmt = conn
        .prepare(
            "UPDATE pmeta SET (v) = (pmeta.v + s.bonus)
               FROM psrc AS s
              WHERE pmeta.k = s.k
             RETURNING pmeta.id, v AS updated_v;",
        )
        .unwrap();
    assert_eq!(uf_stmt.column_count(), 2);
    assert_eq!(uf_stmt.column_names(), &["id", "updated_v"]);
}

#[test]
fn update_from_multi_match_conflict_modes_and_multi_arm_recursive_cte() {
    let script = "
        -- 1. Multiple FROM rows matching the SAME target row on both rowid and WITHOUT ROWID tables,
        --    plus UPDATE OR IGNORE / UPDATE OR REPLACE conflict handling
        CREATE TABLE tgt_rowid (
            id INTEGER PRIMARY KEY,
            code TEXT UNIQUE,
            acc INT
        );
        INSERT INTO tgt_rowid VALUES (1, 'c1', 10), (2, 'c2', 20);

        CREATE TABLE tgt_wor (
            k INT PRIMARY KEY,
            code TEXT UNIQUE,
            acc INT
        ) WITHOUT ROWID;
        INSERT INTO tgt_wor VALUES (1, 'c1', 10), (2, 'c2', 20);

        CREATE TABLE multi_src (target_id INT, add_val INT, next_code TEXT);
        INSERT INTO multi_src VALUES
            (1, 5, 'c1_a'),
            (1, 7, 'c1_b');

        UPDATE tgt_rowid
           SET (acc, code) = (acc + s.add_val, s.next_code)
          FROM multi_src AS s
         WHERE tgt_rowid.id = s.target_id;
        SELECT 'rowid_multi', id, code, acc, changes() FROM tgt_rowid ORDER BY id;

        UPDATE tgt_wor
           SET (acc, code) = (acc + s.add_val, s.next_code)
          FROM multi_src AS s
         WHERE tgt_wor.k = s.target_id;
        SELECT 'wor_multi', k, code, acc, changes() FROM tgt_wor ORDER BY k;

        -- UPDATE OR IGNORE skips colliding unique row while updating non-colliding row:
        CREATE TABLE conflict_src (id INT, desired_code TEXT);
        INSERT INTO conflict_src VALUES (1, 'c2'), (2, 'c2_updated');
        UPDATE OR IGNORE tgt_rowid
           SET code = cs.desired_code
          FROM conflict_src AS cs
         WHERE tgt_rowid.id = cs.id;
        SELECT 'after_ignore', id, code FROM tgt_rowid ORDER BY id;

        -- UPDATE OR REPLACE deletes colliding row when replacing unique key:
        INSERT INTO tgt_rowid VALUES (3, 'c3', 30);
        UPDATE OR REPLACE tgt_rowid
           SET code = 'c3'
          FROM (SELECT 1 AS target_id) AS s
         WHERE tgt_rowid.id = s.target_id;
        SELECT 'after_replace', id, code, acc FROM tgt_rowid ORDER BY id;

        -- 2. Multi-arm seed + multi-arm recursive CTE + recursive WITH without RECURSIVE keyword + NOCASE UNION
        WITH multi_rec(n, tag) AS (
            SELECT 1, 'seedA'
            UNION ALL
            SELECT 2, 'seedB'
            UNION ALL
            SELECT n + 10, tag || '+10' FROM multi_rec WHERE n < 15
            UNION ALL
            SELECT n + 100, tag || '+100' FROM multi_rec WHERE n < 5
        )
        SELECT n, tag FROM multi_rec ORDER BY n, tag;

        CREATE TABLE words(w TEXT COLLATE NOCASE);
        INSERT INTO words VALUES ('Alpha'), ('ALPHA'), ('beta'), ('BETA');
        WITH RECURSIVE wset(w) AS (
            SELECT w FROM words
            UNION
            SELECT lower(w) FROM wset
        )
        SELECT count(*), min(w), max(w) FROM wset;
    ";
    assert_differential_in_memory(script);
}
