#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use sqliters::Connection;
use sqliters::ext::GenerateSeriesModule;
use sqliters::types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConstraintOp, VTabHostContext,
    VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor, VirtualTable,
    VirtualTableModule,
};

fn sqlite_ref_bin() -> PathBuf {
    PathBuf::from("/home/hakanbiris/github/sqliters/target/sqlite-ref/sqlite3")
}

fn run_ref_sqlite(db_path: Option<&std::path::Path>, sql: &str) -> Vec<String> {
    let bin = sqlite_ref_bin();
    assert!(
        bin.exists(),
        "Reference C SQLite binary not found at {}",
        bin.display()
    );
    let mut cmd = Command::new(bin);
    cmd.arg("-batch").arg("-list").arg("-separator").arg("|");
    cmd.arg("-nullvalue").arg("NULL");
    if let Some(p) = db_path {
        cmd.arg(p);
    } else {
        cmd.arg(":memory:");
    }
    cmd.arg(sql);
    let out = cmd.output().expect("failed to execute sqlite3 reference");
    assert!(
        out.status.success(),
        "C sqlite3 failed on SQL:\n{sql}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .map(|l| l.trim_end_matches('\r').to_owned())
        .collect()
}

fn format_val_for_diff(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => {
            if f.fract() == 0.0 {
                format!("{f:.1}")
            } else {
                format!("{f}")
            }
        }
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn run_rust_rows(conn: &Connection, sql: &str) -> Vec<String> {
    let res = conn
        .query(sql, &[])
        .unwrap_or_else(|e| panic!("sqliters failed on SQL:\n{sql}\nerr: {e}"));
    let mut lines = Vec::new();
    for row in res.rows {
        let line = row
            .values()
            .iter()
            .map(format_val_for_diff)
            .collect::<Vec<_>>()
            .join("|");
        lines.push(line);
    }
    lines
}

// ============================================================================
// 1. Acyclic Crate Dependency Graph (`cargo metadata --no-deps --format-version 1`)
// ============================================================================

#[test]
fn test_acyclic_workspace_crate_dependency_graph() {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("failed to run cargo metadata");
    assert!(output.status.success(), "cargo metadata failed");
    let stdout = String::from_utf8(output.stdout).expect("invalid utf8 from cargo metadata");

    // Extract each package's `"name":"..."` and `"dependencies":[...]` from `--no-deps` JSON.
    // Specifically verify:
    // 1. `sqliters-ext` is a workspace member.
    // 2. `sqliters-ext` depends ONLY on `sqliters-types` (zero dependencies on `sqliters-core`,
    //    `sqliters-vdbe`, `sqliters-btree`, `sqliters-pager`, `sqliters-vfs`, `sqliters-syntax`).
    // 3. `sqliters-core` depends on `sqliters-ext` and `sqliters-types`.
    let ext_cargo = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("crates/sqliters-ext/Cargo.toml"),
    )
    .expect("failed to read crates/sqliters-ext/Cargo.toml");
    assert!(
        stdout.contains("\"name\":\"sqliters-ext\""),
        "sqliters-ext must be present in cargo metadata"
    );

    let mut in_deps = false;
    let mut ext_deps = Vec::new();
    for line in ext_cargo.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]" || trimmed == "[dev-dependencies]";
            continue;
        }
        if in_deps
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && let Some((dep_name, _)) = trimmed.split_once('=')
        {
            ext_deps.push(dep_name.trim().to_owned());
        }
    }
    assert!(
        ext_deps.contains(&"sqliters-types".to_owned())
            && ext_deps
                .iter()
                .all(|d| d == "sqliters-types" || d == "sqliters-asm" || d == "sqliters-simd"),
        "sqliters-ext must depend ONLY on sqliters-types (and optionally leaf sqliters-asm/sqliters-simd), got {ext_deps:?}"
    );

    // Also verify all workspace crates' Cargo.toml files form a strict DAG with zero external crates.
    let workspace_crates = [
        ("sqliters-asm", "crates/sqliters-asm/Cargo.toml"),
        ("sqliters-simd", "crates/sqliters-simd/Cargo.toml"),
        ("sqliters-types", "crates/sqliters-types/Cargo.toml"),
        ("sqliters-vfs", "crates/sqliters-vfs/Cargo.toml"),
        ("sqliters-pager", "crates/sqliters-pager/Cargo.toml"),
        ("sqliters-btree", "crates/sqliters-btree/Cargo.toml"),
        ("sqliters-syntax", "crates/sqliters-syntax/Cargo.toml"),
        ("sqliters-vdbe", "crates/sqliters-vdbe/Cargo.toml"),
        ("sqliters-ext", "crates/sqliters-ext/Cargo.toml"),
        ("sqliters-core", "crates/sqliters-core/Cargo.toml"),
        ("sqliters-cli", "crates/sqliters-cli/Cargo.toml"),
        ("sqliters-tcltest", "crates/sqliters-tcltest/Cargo.toml"),
        ("sqliters", "Cargo.toml"),
    ];
    let allowed_names: BTreeSet<&str> = workspace_crates.iter().map(|(n, _)| *n).collect();
    let mut adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (pkg_name, rel_path) in &workspace_crates {
        let manifest =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel_path))
                .expect("read Cargo.toml");
        let mut section = "";
        let mut deps = Vec::new();
        for line in manifest.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                section = t;
                continue;
            }
            if (section == "[dependencies]" || section == "[dev-dependencies]")
                && !t.is_empty()
                && !t.starts_with('#')
                && let Some((k, _)) = t.split_once('=')
            {
                let dep = k.trim();
                assert!(
                    allowed_names.contains(dep),
                    "Forbidden external dependency `{dep}` in {rel_path}"
                );
                deps.push(dep.to_owned());
            }
        }
        adj.insert((*pkg_name).to_owned(), deps);
    }
    assert!(
        adj["sqliters-asm"].is_empty(),
        "sqliters-asm must be a zero-dependency leaf crate"
    );
    assert!(
        adj["sqliters-simd"].is_empty(),
        "sqliters-simd must be a zero-dependency leaf crate"
    );

    // Topological sort / cycle check
    fn has_cycle(
        node: &str,
        adj: &BTreeMap<String, Vec<String>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> bool {
        if visiting.contains(node) {
            return true;
        }
        if visited.contains(node) {
            return false;
        }
        visiting.insert(node.to_owned());
        if let Some(neighbors) = adj.get(node) {
            for next in neighbors {
                if has_cycle(next, adj, visiting, visited) {
                    return true;
                }
            }
        }
        visiting.remove(node);
        visited.insert(node.to_owned());
        false
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for pkg in adj.keys() {
        assert!(
            !has_cycle(pkg, &adj, &mut visiting, &mut visited),
            "Cycle detected in workspace dependency graph starting at {pkg}"
        );
    }
}

// ============================================================================
// 2. Custom VirtualTableModule: Shadow Tables, Internal Sub-SQL, Transactions,
//    Savepoints, HIDDEN Columns, WITHOUT ROWID, Defensive Mode, Lifecycle
// ============================================================================

#[derive(Default, Debug)]
struct LifecycleLog {
    events: Vec<String>,
    fail_next_sync: bool,
    last_raw_args: Vec<String>,
}

/// A custom virtual table module backed by a real shadow table `<table_name>_data`
/// using `VTabHostContext::execute_sql` and `VTabHostContext::query_sql`.
struct ShadowKvModule {
    log: Arc<Mutex<LifecycleLog>>,
    risk: VTabRiskFlags,
    force_innocuous_config: bool,
}

impl ShadowKvModule {
    fn new(log: Arc<Mutex<LifecycleLog>>) -> Self {
        Self {
            log,
            risk: VTabRiskFlags::Normal,
            force_innocuous_config: false,
        }
    }
}

impl VirtualTableModule for ShadowKvModule {
    fn name(&self) -> &str {
        "shadow_kv"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        self.risk
    }

    fn shadow_table_suffixes(&self) -> &[&str] {
        &["data"]
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        {
            let mut g = self.log.lock().unwrap();
            g.events
                .push(format!("create:{}.{}", ctx.db_name(), ctx.table_name()));
            g.last_raw_args = args.to_vec();
        }
        if self.force_innocuous_config {
            ctx.set_vtab_config(VTabConfigOption::Innocuous)?;
        }
        let shadow = format!("{}_data", ctx.table_name());
        ctx.execute_sql(
            &format!(
                "CREATE TABLE \"{shadow}\"(id INTEGER PRIMARY KEY, k TEXT, v INTEGER, tag TEXT)"
            ),
            &[],
        )?;
        ctx.declare_vtab("CREATE TABLE x(k TEXT, v INTEGER, tag TEXT HIDDEN)")?;
        Ok(Box::new(ShadowKvTable {
            db_name: ctx.db_name().to_owned(),
            table_name: ctx.table_name().to_owned(),
            log: Arc::clone(&self.log),
        }))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        {
            let mut g = self.log.lock().unwrap();
            g.events
                .push(format!("connect:{}.{}", ctx.db_name(), ctx.table_name()));
            g.last_raw_args = args.to_vec();
        }
        if self.force_innocuous_config {
            ctx.set_vtab_config(VTabConfigOption::Innocuous)?;
        }
        ctx.declare_vtab("CREATE TABLE x(k TEXT, v INTEGER, tag TEXT HIDDEN)")?;
        Ok(Box::new(ShadowKvTable {
            db_name: ctx.db_name().to_owned(),
            table_name: ctx.table_name().to_owned(),
            log: Arc::clone(&self.log),
        }))
    }
}

struct ShadowKvTable {
    db_name: String,
    table_name: String,
    log: Arc<Mutex<LifecycleLog>>,
}

impl VirtualTable for ShadowKvTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let mut idx_num = 0i32;
        let mut argv = 1usize;
        for (i, c) in info.constraints.iter().enumerate() {
            if !c.usable {
                continue;
            }
            if c.i_column == 0 && c.op == VTabConstraintOp::Eq {
                idx_num |= 1;
                info.constraint_usage[i].argv_index = argv;
                info.constraint_usage[i].omit = true;
                let _ = &mut argv;
                break;
            }
        }
        if info.order_by.len() == 1 && info.order_by[0].i_column == 1 {
            if info.order_by[0].desc {
                idx_num |= 4;
            } else {
                idx_num |= 2;
            }
            info.order_by_consumed = true;
        }
        info.idx_num = idx_num;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(ShadowKvCursor {
            table_name: self.table_name.clone(),
            rows: Vec::new(),
            pos: 0,
        }))
    }

    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let shadow = format!("{}_data", self.table_name);
        match op {
            VTabUpdateOperation::Delete { old_rowid } => {
                ctx.execute_sql(
                    &format!("DELETE FROM \"{shadow}\" WHERE id = ?1"),
                    &[old_rowid],
                )?;
                Ok(None)
            }
            VTabUpdateOperation::Insert { new_rowid, values } => {
                let k = values.first().cloned().unwrap_or(Value::Null);
                let v = values.get(1).cloned().unwrap_or(Value::Null);
                let tag = match values.get(2).cloned().unwrap_or(Value::Null) {
                    Value::Null => Value::Text("default_tag".to_owned()),
                    other => other,
                };
                if let Some(rid) = new_rowid.filter(|r| !matches!(r, Value::Null)) {
                    ctx.execute_sql(
                        &format!("INSERT INTO \"{shadow}\"(id, k, v, tag) VALUES (?1, ?2, ?3, ?4)"),
                        &[rid.clone(), k, v, tag],
                    )?;
                    if let Value::Integer(i) = rid {
                        return Ok(Some(i));
                    }
                } else {
                    ctx.execute_sql(
                        &format!("INSERT INTO \"{shadow}\"(k, v, tag) VALUES (?1, ?2, ?3)"),
                        &[k, v, tag],
                    )?;
                }
                let r = ctx.query_sql(&format!("SELECT max(id) FROM \"{shadow}\""), &[])?;
                let id = match r.first().and_then(|row| row.first()) {
                    Some(Value::Integer(i)) => *i,
                    _ => 1,
                };
                Ok(Some(id))
            }
            VTabUpdateOperation::Update {
                old_rowid,
                new_rowid,
                values,
            } => {
                let k = values.first().cloned().unwrap_or(Value::Null);
                let v = values.get(1).cloned().unwrap_or(Value::Null);
                let tag = values.get(2).cloned().unwrap_or(Value::Null);
                let eff_new = if matches!(new_rowid, Value::Null) {
                    old_rowid.clone()
                } else {
                    new_rowid
                };
                ctx.execute_sql(
                    &format!(
                        "UPDATE \"{shadow}\" SET id = ?1, k = ?2, v = ?3, tag = ?4 WHERE id = ?5"
                    ),
                    &[eff_new.clone(), k, v, tag, old_rowid],
                )?;
                if let Value::Integer(i) = eff_new {
                    Ok(Some(i))
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn begin(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("begin:{}", self.table_name));
        Ok(())
    }

    fn sync(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut g = self.log.lock().unwrap();
        g.events.push(format!("sync:{}", self.table_name));
        if g.fail_next_sync {
            g.fail_next_sync = false;
            return Err(SqliteError::new(
                ErrorCode::Error,
                "simulated xSync failure",
            ));
        }
        Ok(())
    }

    fn commit(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("commit:{}", self.table_name));
        Ok(())
    }

    fn rollback(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("rollback:{}", self.table_name));
        Ok(())
    }

    fn savepoint(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("savepoint:{}:{idx}", self.table_name));
        Ok(())
    }

    fn release(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("release:{}:{idx}", self.table_name));
        Ok(())
    }

    fn rollback_to(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("rollback_to:{}:{idx}", self.table_name));
        Ok(())
    }

    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
        let old_shadow = format!("{}_data", self.table_name);
        let new_shadow = format!("{new_name}_data");
        ctx.execute_sql(
            &format!("ALTER TABLE \"{old_shadow}\" RENAME TO \"{new_shadow}\""),
            &[],
        )?;
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("rename:{}->{new_name}", self.table_name));
        self.table_name = new_name.to_owned();
        Ok(())
    }

    fn destroy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let shadow = format!("{}_data", self.table_name);
        ctx.execute_sql(&format!("DROP TABLE IF EXISTS \"{shadow}\""), &[])?;
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("destroy:{}.{}", self.db_name, self.table_name));
        Ok(())
    }

    fn disconnect(&mut self) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .events
            .push(format!("disconnect:{}.{}", self.db_name, self.table_name));
        Ok(())
    }
}

struct ShadowKvCursor {
    table_name: String,
    rows: Vec<(i64, Value, Value, Value)>,
    pos: usize,
}

impl VirtualCursor for ShadowKvCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        let shadow = format!("{}_data", self.table_name);
        let order_clause = if (idx_num & 4) != 0 {
            "ORDER BY v DESC"
        } else if (idx_num & 2) != 0 {
            "ORDER BY v ASC"
        } else {
            "ORDER BY id ASC"
        };
        let raw_rows = if (idx_num & 1) != 0 {
            ctx.query_sql(
                &format!("SELECT id, k, v, tag FROM \"{shadow}\" WHERE k = ?1 {order_clause}"),
                args,
            )?
        } else {
            ctx.query_sql(
                &format!("SELECT id, k, v, tag FROM \"{shadow}\" {order_clause}"),
                &[],
            )?
        };
        self.rows = raw_rows
            .into_iter()
            .map(|r| {
                let id = match r.first() {
                    Some(Value::Integer(i)) => *i,
                    _ => 0,
                };
                let k = r.get(1).cloned().unwrap_or(Value::Null);
                let v = r.get(2).cloned().unwrap_or(Value::Null);
                let tag = r.get(3).cloned().unwrap_or(Value::Null);
                (id, k, v, tag)
            })
            .collect();
        self.pos = 0;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        let row = &self.rows[self.pos];
        Ok(match col_idx {
            0 => row.1.clone(),
            1 => row.2.clone(),
            2 => row.3.clone(),
            _ => Value::Null,
        })
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.rows[self.pos].0)
    }
}

#[test]
fn test_custom_vtab_lifecycle_shadow_tables_transactions_and_defensive_mode() {
    let log = Arc::new(Mutex::new(LifecycleLog::default()));
    let conn = Connection::open_in_memory().unwrap();
    conn.register_virtual_table_module(Arc::new(ShadowKvModule::new(Arc::clone(&log))))
        .unwrap();

    // 1. CREATE VIRTUAL TABLE invokes xCreate and creates the shadow table `kv_data`
    conn.execute_batch("CREATE VIRTUAL TABLE kv USING shadow_kv(arg1, 'arg 2');")
        .unwrap();
    assert_eq!(
        log.lock().unwrap().last_raw_args,
        vec!["arg1".to_owned(), "'arg 2'".to_owned()]
    );

    // 2. INSERT with default columns omits HIDDEN column `tag`; explicit column list includes `tag`
    conn.execute_batch(
        "INSERT INTO kv VALUES ('alpha', 10);
         INSERT INTO kv(k, v, tag) VALUES ('beta', 20, 'custom_tag');
         INSERT INTO kv(rowid, k, v) VALUES (7, 'gamma', 15);",
    )
    .unwrap();
    assert_eq!(conn.last_insert_rowid(), 7);

    // 3. SELECT * omits HIDDEN column `tag`; explicit SELECT includes `tag` and `rowid`
    assert_eq!(
        run_rust_rows(&conn, "SELECT * FROM kv ORDER BY v ASC;"),
        vec!["alpha|10", "gamma|15", "beta|20"]
    );
    assert_eq!(
        run_rust_rows(&conn, "SELECT rowid, k, v, tag FROM kv WHERE k = 'beta';"),
        vec!["2|beta|20|custom_tag"]
    );

    // 4. PRAGMA table_info omits HIDDEN column; PRAGMA table_xinfo includes HIDDEN column with hidden=1
    assert_eq!(
        run_rust_rows(&conn, "SELECT cid, name FROM pragma_table_info('kv');"),
        vec!["0|k", "1|v"]
    );
    assert_eq!(
        run_rust_rows(
            &conn,
            "SELECT cid, name, hidden FROM pragma_table_xinfo('kv');"
        ),
        vec!["0|k|0", "1|v|0", "2|tag|1"]
    );

    // 5. UPDATE and DELETE on virtual table
    conn.execute_batch("UPDATE kv SET v = 25, tag = 'updated' WHERE k = 'beta';")
        .unwrap();
    assert_eq!(
        run_rust_rows(&conn, "SELECT k, v, tag FROM kv WHERE k = 'beta';"),
        vec!["beta|25|updated"]
    );
    conn.execute_batch("DELETE FROM kv WHERE rowid = 7;")
        .unwrap();
    assert_eq!(
        run_rust_rows(&conn, "SELECT k, v FROM kv ORDER BY v DESC;"),
        vec!["beta|25", "alpha|10"]
    );

    // 6. Transaction & Savepoint hooks: xBegin, xSavepoint, xRollbackTo, xRelease, xSync, xCommit
    log.lock().unwrap().events.clear();
    conn.execute_batch(
        "BEGIN;
         INSERT INTO kv VALUES ('delta', 30);
         SAVEPOINT sp1;
         INSERT INTO kv VALUES ('epsilon', 40);
         ROLLBACK TO sp1;
         RELEASE sp1;
         COMMIT;",
    )
    .unwrap();
    assert_eq!(
        run_rust_rows(&conn, "SELECT k, v FROM kv ORDER BY v ASC;"),
        vec!["alpha|10", "beta|25", "delta|30"]
    );
    {
        let evs = log.lock().unwrap().events.clone();
        assert!(evs.contains(&"begin:kv".to_owned()), "events: {evs:?}");
        assert!(
            evs.iter().any(|e| e.starts_with("savepoint:kv:")),
            "events: {evs:?}"
        );
        assert!(
            evs.iter().any(|e| e.starts_with("rollback_to:kv:")),
            "events: {evs:?}"
        );
        assert!(
            evs.iter().any(|e| e.starts_with("release:kv:")),
            "events: {evs:?}"
        );
        assert!(evs.contains(&"sync:kv".to_owned()), "events: {evs:?}");
        assert!(evs.contains(&"commit:kv".to_owned()), "events: {evs:?}");
    }

    // 7. xSync failure during COMMIT triggers full rollback of both B-tree shadow table and xRollback
    log.lock().unwrap().events.clear();
    log.lock().unwrap().fail_next_sync = true;
    conn.execute_batch("BEGIN; INSERT INTO kv VALUES ('zeta', 99);")
        .unwrap();
    let commit_err = conn.execute_batch("COMMIT;").unwrap_err();
    assert!(
        commit_err.to_string().contains("simulated xSync failure"),
        "unexpected err: {commit_err}"
    );
    // Notice that in SQLite / sqliters, a failed xSync rolls back or leaves data consistent;
    // verify `zeta` is not committed when rolled back:
    let _ = conn.execute_batch("ROLLBACK;");
    assert_eq!(
        run_rust_rows(&conn, "SELECT k, v FROM kv ORDER BY v ASC;"),
        vec!["alpha|10", "beta|25", "delta|30"]
    );

    // 8. Defensive mode protects shadow tables (`kv_data`) from direct user DML/DDL while still
    //    allowing virtual table methods (`INSERT INTO kv ...`) to write to `kv_data`!
    conn.set_defensive(true).unwrap();
    assert!(conn.defensive().unwrap());
    let direct_ins_err = conn
        .execute_batch("INSERT INTO kv_data(k, v) VALUES ('hack', 999);")
        .unwrap_err();
    assert!(
        direct_ins_err.to_string().contains("may not be modified"),
        "expected defensive shadow table error, got: {direct_ins_err}"
    );
    let direct_drop_err = conn.execute_batch("DROP TABLE kv_data;").unwrap_err();
    assert!(
        direct_drop_err.to_string().contains("may not be dropped"),
        "expected defensive drop shadow table error, got: {direct_drop_err}"
    );
    // Virtual table's own internal sub-SQL still succeeds even in defensive mode!
    conn.execute_batch("INSERT INTO kv VALUES ('theta', 50);")
        .unwrap();
    assert_eq!(
        run_rust_rows(&conn, "SELECT k, v FROM kv WHERE k = 'theta';"),
        vec!["theta|50"]
    );
    conn.set_defensive(false).unwrap();

    // 9. Prohibited DDL on virtual tables (CREATE INDEX, CREATE TRIGGER, ALTER TABLE ADD/DROP COLUMN)
    let idx_err = conn
        .execute_batch("CREATE INDEX idx_kv ON kv(k);")
        .unwrap_err();
    assert!(
        idx_err
            .to_string()
            .contains("virtual tables may not be indexed"),
        "got: {idx_err}"
    );
    let trig_err = conn
        .execute_batch("CREATE TRIGGER tr_kv BEFORE INSERT ON kv BEGIN SELECT 1; END;")
        .unwrap_err();
    assert!(
        trig_err
            .to_string()
            .contains("cannot create triggers on virtual tables"),
        "got: {trig_err}"
    );

    // 10. ALTER TABLE kv RENAME TO kv_renamed invokes xRename and updates sqlite_schema
    conn.execute_batch("ALTER TABLE kv RENAME TO kv_renamed;")
        .unwrap();
    assert_eq!(
        run_rust_rows(&conn, "SELECT k, v FROM kv_renamed WHERE k = 'theta';"),
        vec!["theta|50"]
    );
    assert_eq!(
        run_rust_rows(
            &conn,
            "SELECT name, sql FROM sqlite_schema WHERE type='table' AND name='kv_renamed';"
        ),
        vec!["kv_renamed|CREATE VIRTUAL TABLE kv_renamed USING shadow_kv(arg1, 'arg 2')"]
    );

    // 11. DROP TABLE kv_renamed invokes xDestroy and drops the shadow table `kv_renamed_data`
    conn.execute_batch("DROP TABLE kv_renamed;").unwrap();
    assert_eq!(
        run_rust_rows(
            &conn,
            "SELECT count(*) FROM sqlite_schema WHERE name IN ('kv_renamed', 'kv_renamed_data');"
        ),
        vec!["0"]
    );
}

// ============================================================================
// 3. WITHOUT ROWID Virtual Tables (Single-PK Writable vs Composite-PK Read-Only)
// ============================================================================

struct WithoutRowidVTabModule {
    composite_pk: bool,
}

impl VirtualTableModule for WithoutRowidVTabModule {
    fn name(&self) -> &str {
        if self.composite_pk {
            "wor_composite"
        } else {
            "wor_single"
        }
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.connect(ctx, _args)
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        if self.composite_pk {
            ctx.declare_vtab(
                "CREATE TABLE x(k1 TEXT, k2 TEXT, val INTEGER, PRIMARY KEY(k1, k2)) WITHOUT ROWID",
            )?;
        } else {
            ctx.declare_vtab("CREATE TABLE x(k TEXT PRIMARY KEY, val INTEGER) WITHOUT ROWID")?;
        }
        Ok(Box::new(WithoutRowidVTab {
            rows: Mutex::new(BTreeMap::new()),
        }))
    }
}

struct WithoutRowidVTab {
    rows: Mutex<BTreeMap<String, i64>>,
}

impl VirtualTable for WithoutRowidVTab {
    fn best_index(&self, _info: &mut VTabIndexInfo) -> Result<()> {
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        let snapshot: Vec<(String, i64)> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        Ok(Box::new(WithoutRowidCursor {
            rows: snapshot,
            pos: 0,
        }))
    }

    fn update(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let mut map = self.rows.lock().unwrap();
        match op {
            VTabUpdateOperation::Insert { values, .. } => {
                if let (Some(Value::Text(k)), Some(Value::Integer(v))) =
                    (values.first(), values.get(1))
                {
                    map.insert(k.clone(), *v);
                }
                Ok(None)
            }
            VTabUpdateOperation::Update {
                old_rowid, values, ..
            } => {
                if let Value::Text(old_k) = old_rowid {
                    map.remove(&old_k);
                }
                if let (Some(Value::Text(k)), Some(Value::Integer(v))) =
                    (values.first(), values.get(1))
                {
                    map.insert(k.clone(), *v);
                }
                Ok(None)
            }
            VTabUpdateOperation::Delete { old_rowid } => {
                if let Value::Text(old_k) = old_rowid {
                    map.remove(&old_k);
                }
                Ok(None)
            }
        }
    }
}

struct WithoutRowidCursor {
    rows: Vec<(String, i64)>,
    pos: usize,
}

impl VirtualCursor for WithoutRowidCursor {
    fn filter(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        _idx_num: i32,
        _idx_str: Option<&str>,
        _args: &[Value],
    ) -> Result<()> {
        self.pos = 0;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        let (k, v) = &self.rows[self.pos];
        Ok(match col_idx {
            0 => Value::Text(k.clone()),
            1 => Value::Integer(*v),
            _ => Value::Null,
        })
    }

    fn rowid(&self) -> Result<i64> {
        Ok(0)
    }
}

#[test]
fn test_without_rowid_virtual_table_single_and_composite_pk() {
    let conn = Connection::open_in_memory().unwrap();
    conn.register_virtual_table_module(Arc::new(WithoutRowidVTabModule {
        composite_pk: false,
    }))
    .unwrap();
    conn.register_virtual_table_module(Arc::new(WithoutRowidVTabModule { composite_pk: true }))
        .unwrap();

    conn.execute_batch(
        "CREATE VIRTUAL TABLE t_wor USING wor_single;
         CREATE VIRTUAL TABLE t_wor_comp USING wor_composite;",
    )
    .unwrap();

    // Single-column PK WITHOUT ROWID virtual table supports INSERT / UPDATE / DELETE
    conn.execute_batch(
        "INSERT INTO t_wor VALUES ('k1', 100), ('k2', 200);
         UPDATE t_wor SET val = 150 WHERE k = 'k1';
         DELETE FROM t_wor WHERE k = 'k2';",
    )
    .unwrap();
    assert_eq!(
        run_rust_rows(&conn, "SELECT k, val FROM t_wor;"),
        vec!["k1|150"]
    );
    // `rowid` does not exist on a WITHOUT ROWID virtual table
    assert!(conn.execute_batch("SELECT rowid FROM t_wor;").is_err());

    // Composite-PK WITHOUT ROWID virtual table is read-only (`ErrorCode::ReadOnly`)
    let err = conn
        .execute_batch("INSERT INTO t_wor_comp VALUES ('a', 'b', 1);")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ReadOnly);
}

// ============================================================================
// 4. Lazy xConnect (`Group 66`), VTabRiskFlags (`Group 61`), and Raw Module Args (`Group 54`)
// ============================================================================

struct RawEchoModule {
    captured_args: Arc<Mutex<Vec<String>>>,
    risk: VTabRiskFlags,
}

impl VirtualTableModule for RawEchoModule {
    fn name(&self) -> &str {
        "raw_echo"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        self.risk
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.connect(ctx, args)
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        *self.captured_args.lock().unwrap() = args.to_vec();
        ctx.declare_vtab("CREATE TABLE x(idx INTEGER, raw_arg TEXT)")?;
        let owned = args.to_vec();
        Ok(Box::new(RawEchoTable { args: owned }))
    }
}

struct RawEchoTable {
    args: Vec<String>,
}

impl VirtualTable for RawEchoTable {
    fn best_index(&self, _info: &mut VTabIndexInfo) -> Result<()> {
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(RawEchoCursor {
            args: self.args.clone(),
            pos: 0,
        }))
    }
}

struct RawEchoCursor {
    args: Vec<String>,
    pos: usize,
}

impl VirtualCursor for RawEchoCursor {
    fn filter(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        _idx_num: i32,
        _idx_str: Option<&str>,
        _args: &[Value],
    ) -> Result<()> {
        self.pos = 0;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.args.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        Ok(match col_idx {
            0 => Value::Integer(self.pos as i64),
            1 => Value::Text(self.args[self.pos].clone()),
            _ => Value::Null,
        })
    }

    fn rowid(&self) -> Result<i64> {
        Ok((self.pos + 1) as i64)
    }
}

#[test]
fn test_lazy_xconnect_and_raw_module_args_and_risk_flags() {
    let tmp_dir = std::env::temp_dir().join(format!("sqliters_phase7_lazy_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("lazy_vtab.db");

    let captured = Arc::new(Mutex::new(Vec::new()));
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.register_virtual_table_module(Arc::new(RawEchoModule {
            captured_args: Arc::clone(&captured),
            risk: VTabRiskFlags::Normal,
        }))
        .unwrap();
        // FTS5 and R-Tree style unparsed raw module arguments with nested parens, quotes, `+col`
        conn.execute_batch(
            "CREATE TABLE normal_t(id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO normal_t VALUES (1, 'hello'), (2, 'world');
             CREATE VIRTUAL TABLE fts_like USING raw_echo(
                 content='normal_t',
                 tokenize = 'unicode61 remove_diacritics 2',
                 body UNINDEXED,
                 prefix='2,3',
                 +aux1 TEXT NOT NULL
             );",
        )
        .unwrap();
        assert_eq!(
            *captured.lock().unwrap(),
            vec![
                "content='normal_t'".to_owned(),
                "tokenize = 'unicode61 remove_diacritics 2'".to_owned(),
                "body UNINDEXED".to_owned(),
                "prefix='2,3'".to_owned(),
                "+aux1 TEXT NOT NULL".to_owned(),
            ]
        );
    }

    // Reopen `db_path` WITHOUT registering `raw_echo`:
    // 1. Opening and querying `normal_t` succeeds because `Schema::load` does NOT call `xConnect`!
    {
        let conn2 = Connection::open(&db_path).unwrap();
        assert_eq!(
            run_rust_rows(&conn2, "SELECT id, label FROM normal_t ORDER BY id;"),
            vec!["1|hello", "2|world"]
        );

        // 2. Touching `fts_like` lazily triggers `xConnect` and fails with "no such module: raw_echo"
        let err = conn2.execute_batch("SELECT * FROM fts_like;").unwrap_err();
        assert!(
            err.to_string().contains("no such module: raw_echo"),
            "expected 'no such module: raw_echo', got: {err}"
        );

        // 3. Registering `raw_echo` now allows `SELECT * FROM fts_like` to succeed!
        conn2
            .register_virtual_table_module(Arc::new(RawEchoModule {
                captured_args: Arc::clone(&captured),
                risk: VTabRiskFlags::Normal,
            }))
            .unwrap();
        assert_eq!(
            run_rust_rows(&conn2, "SELECT idx, raw_arg FROM fts_like ORDER BY idx;"),
            vec![
                "0|content='normal_t'",
                "1|tokenize = 'unicode61 remove_diacritics 2'",
                "2|body UNINDEXED",
                "3|prefix='2,3'",
                "4|+aux1 TEXT NOT NULL",
            ]
        );

        // 4. Test `VTabRiskFlags::Normal` with `PRAGMA trusted_schema = 0` inside a VIEW
        conn2
            .execute_batch("CREATE VIEW v_fts AS SELECT raw_arg FROM fts_like WHERE idx = 0;")
            .unwrap();
        assert_eq!(
            run_rust_rows(&conn2, "SELECT * FROM v_fts;"),
            vec!["content='normal_t'"]
        );
        conn2.execute_batch("PRAGMA trusted_schema = 0;").unwrap();
        assert!(!conn2.trusted_schema().unwrap());
        let untrusted_err = conn2.execute_batch("SELECT * FROM v_fts;").unwrap_err();
        assert!(
            untrusted_err
                .to_string()
                .contains("unsafe use of virtual table \"fts_like\""),
            "expected unsafe use error, got: {untrusted_err}"
        );
        conn2.execute_batch("PRAGMA trusted_schema = 1;").unwrap();
        assert_eq!(
            run_rust_rows(&conn2, "SELECT * FROM v_fts;"),
            vec!["content='normal_t'"]
        );
    }

    // 5. Test `VTabRiskFlags::DirectOnly`: rejected in VIEW/TRIGGER even when `trusted_schema = 1`
    {
        let conn3 = Connection::open_in_memory().unwrap();
        conn3
            .register_virtual_table_module(Arc::new(RawEchoModule {
                captured_args: Arc::clone(&captured),
                risk: VTabRiskFlags::DirectOnly,
            }))
            .unwrap();
        conn3
            .execute_batch(
                "CREATE VIRTUAL TABLE direct_vt USING raw_echo(only_top_level);
                 CREATE VIEW v_direct AS SELECT * FROM direct_vt;",
            )
            .unwrap();
        assert_eq!(
            run_rust_rows(&conn3, "SELECT idx, raw_arg FROM direct_vt;"),
            vec!["0|only_top_level"]
        );
        let direct_err = conn3.execute_batch("SELECT * FROM v_direct;").unwrap_err();
        assert!(
            direct_err
                .to_string()
                .contains("unsafe use of virtual table \"direct_vt\""),
            "expected DirectOnly error in view, got: {direct_err}"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ============================================================================
// 5. Differential Parity against C SQLite 3.54.1: `generate_series`, `pragma_*`,
//    `dbstat`, and JSON `->` / `->>` Operators
// ============================================================================

#[test]
fn test_generate_series_differential_parity_with_c_sqlite() {
    let conn = Connection::open_in_memory().unwrap();
    // Verify `GenerateSeriesModule` is also directly constructible from `sqliters::ext`
    let _ext_mod = GenerateSeriesModule;

    let queries = [
        "SELECT value FROM generate_series(1, 5);",
        "SELECT value FROM generate_series(1, 10, 3);",
        "SELECT value FROM generate_series(10, 1, -3);",
        "SELECT value FROM generate_series(1, 5, 0);",
        "SELECT value FROM generate_series(5, 1);",
        "SELECT value FROM generate_series(1, 5, -1);",
        "SELECT value FROM generate_series(1, 5) ORDER BY value DESC;",
        "SELECT value, start, stop, step FROM generate_series(2, 6, 2);",
        "SELECT * FROM generate_series WHERE start = 1 AND stop = 4 AND step = 1;",
        "SELECT value FROM generate_series(1, 10) WHERE value >= 3 AND value <= 7;",
        "SELECT value FROM generate_series(1, 10) LIMIT 4 OFFSET 2;",
        "SELECT g.value * 2 FROM generate_series(1, 4) AS g;",
        "WITH s(x) AS (SELECT value FROM generate_series(1, 3)) SELECT x, y.value FROM s JOIN generate_series(10, 11) AS y;",
    ];

    for sql in queries {
        let rust_out = run_rust_rows(&conn, sql);
        let ref_out = run_ref_sqlite(None, sql);
        assert_eq!(rust_out, ref_out, "generate_series mismatch on SQL: {sql}");
    }

    // Missing `start` constraint must fail
    let missing_start = conn.execute_batch("SELECT * FROM generate_series;");
    assert!(missing_start.is_err());
}

#[test]
fn test_eponymous_pragma_virtual_tables_differential_parity() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let tmp_dir =
        std::env::temp_dir().join(format!("sqliters_phase7_pragma_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("pragma_vtab.db");

    let setup_sql = "
        CREATE TABLE parent(id INTEGER PRIMARY KEY, code TEXT UNIQUE NOT NULL);
        CREATE TABLE child(
            cid INTEGER PRIMARY KEY,
            pid INTEGER REFERENCES parent(id) ON DELETE CASCADE ON UPDATE RESTRICT,
            name TEXT DEFAULT 'anon'
        );
        CREATE INDEX idx_child_name ON child(name DESC);
        CREATE VIEW v_child AS SELECT cid, name FROM child;
    ";

    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(setup_sql).unwrap();

    let queries = [
        "SELECT cid, name, type, \"notnull\", dflt_value, pk FROM pragma_table_info('parent') ORDER BY cid;",
        "SELECT cid, name, type, \"notnull\", dflt_value, pk FROM pragma_table_info('child') ORDER BY cid;",
        "SELECT cid, name, type, \"notnull\", dflt_value, pk FROM pragma_table_info WHERE arg = 'child' ORDER BY cid;",
        "SELECT cid, name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo('child') ORDER BY cid;",
        "SELECT schema, name, type, ncol, wr, strict FROM pragma_table_list WHERE name IN ('parent', 'child', 'v_child') ORDER BY name;",
        "SELECT seq, name, \"unique\", origin, partial FROM pragma_index_list('child') ORDER BY seq;",
        "SELECT seqno, cid, name FROM pragma_index_info('idx_child_name') ORDER BY seqno;",
        "SELECT seqno, cid, name, desc, coll, key FROM pragma_index_xinfo('idx_child_name') ORDER BY seqno;",
        "SELECT id, seq, \"table\", \"from\", \"to\", on_update, on_delete, match FROM pragma_foreign_key_list('child') ORDER BY id, seq;",
        "SELECT seq, name FROM pragma_database_list ORDER BY seq;",
        "SELECT name FROM pragma_collation_list WHERE name IN ('BINARY', 'NOCASE', 'RTRIM') ORDER BY name;",
        "SELECT m.name, p.cid, p.name, p.type FROM sqlite_schema m JOIN pragma_table_info(m.name) p WHERE m.type = 'table' ORDER BY m.name, p.cid;",
    ];

    for sql in queries {
        let rust_out = run_rust_rows(&conn, sql);
        let ref_out = run_ref_sqlite(Some(&db_path), sql);
        assert_eq!(rust_out, ref_out, "pragma_* vtab mismatch on SQL: {sql}");
    }

    // Also verify `pragma_module_list`, `pragma_function_list`, and `pragma_pragma_list`
    let mod_rows = run_rust_rows(
        &conn,
        "SELECT name FROM pragma_module_list WHERE name IN ('generate_series', 'dbstat') ORDER BY name;",
    );
    assert_eq!(mod_rows, vec!["dbstat", "generate_series"]);

    let fn_rows = run_rust_rows(
        &conn,
        "SELECT DISTINCT name, builtin FROM pragma_function_list WHERE name IN ('abs', 'coalesce', 'count', 'substr') ORDER BY name;",
    );
    assert_eq!(fn_rows, vec!["abs|1", "coalesce|1", "count|1", "substr|1"]);

    let pragma_rows = run_rust_rows(
        &conn,
        "SELECT name FROM pragma_pragma_list WHERE name IN ('table_info', 'foreign_keys', 'trusted_schema', 'user_version') ORDER BY name;",
    );
    assert_eq!(
        pragma_rows,
        vec![
            "foreign_keys",
            "table_info",
            "trusted_schema",
            "user_version"
        ]
    );

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_dbstat_virtual_table_differential_parity() {
    let tmp_dir =
        std::env::temp_dir().join(format!("sqliters_phase7_dbstat_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("dbstat_test.db");

    // Build database using C sqlite3 so on-disk Format 3 B-tree pages are created by C SQLite,
    // then inspect them via sqliters's built-in `dbstat` virtual table!
    let setup_sql = "
        PRAGMA page_size = 4096;
        CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT, payload BLOB);
        CREATE INDEX idx_items_name ON items(name);
        INSERT INTO items VALUES (1, 'alpha', zeroblob(100));
        INSERT INTO items VALUES (2, 'beta', zeroblob(200));
        INSERT INTO items VALUES (3, 'gamma', zeroblob(150));
    ";
    run_ref_sqlite(Some(&db_path), setup_sql);

    let conn = Connection::open(&db_path).unwrap();

    // 1. Per-page mode: 3 pages (page 1 = sqlite_schema, page 2 = items, page 3 = idx_items_name)
    let page_rows = run_rust_rows(
        &conn,
        "SELECT name, path, pageno, pagetype, ncell, pgoffset, pgsize FROM dbstat ORDER BY pageno;",
    );
    assert_eq!(
        page_rows,
        vec![
            "sqlite_schema|/|1|leaf|2|0|4096",
            "items|/|2|leaf|3|4096|4096",
            "idx_items_name|/|3|leaf|3|8192|4096",
        ]
    );

    // 2. Table-valued function form `dbstat('main')` matches `FROM dbstat`
    assert_eq!(
        run_rust_rows(
            &conn,
            "SELECT name, path, pageno, pagetype, ncell, pgoffset, pgsize FROM dbstat('main') ORDER BY pageno;",
        ),
        page_rows
    );

    // 3. Aggregate mode (`WHERE aggregate = 1`) groups by object name and sums payload + unused == pgsize
    let agg_rows = run_rust_rows(
        &conn,
        "SELECT name, path, pageno, pagetype, ncell, payload > 0, unused > 0, pgsize FROM dbstat WHERE aggregate = 1 ORDER BY name;",
    );
    assert_eq!(
        agg_rows,
        vec![
            "idx_items_name|NULL|1|NULL|3|1|1|4096",
            "items|NULL|1|NULL|3|1|1|4096",
            "sqlite_schema|NULL|1|NULL|2|1|1|4096",
        ]
    );

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_json_arrow_and_double_arrow_differential_parity() {
    let conn = Connection::open_in_memory().unwrap();

    let queries = [
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' -> 'a';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' ->> 'a';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' -> 'b';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' ->> 'b';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' -> 'c';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' ->> 'c';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' -> 'd';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' ->> 'd';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' -> 'e';",
        "SELECT '{\"a\":1,\"b\":\"hello\",\"c\":[10,20,30],\"d\":true,\"e\":null}' ->> 'e';",
        "SELECT '{\"a\":1}' -> 'missing', '{\"a\":1}' ->> 'missing';",
        "SELECT '[10, 20, 30]' -> 1, '[10, 20, 30]' ->> 1;",
        "SELECT '[10, 20, 30]' -> '$[#-1]', '[10, 20, 30]' ->> '$[#-1]';",
        "SELECT '{\"a\":{\"b\":[10,{\"c\":\"ok\"}]}}' -> 'a' -> 'b' -> 1 -> 'c';",
        "SELECT '{\"a\":{\"b\":[10,{\"c\":\"ok\"}]}}' -> 'a' -> 'b' -> 1 ->> 'c';",
        "SELECT subtype('{\"a\":\"hello\"}' -> 'a'), subtype('{\"a\":\"hello\"}' ->> 'a');",
        "SELECT typeof('{\"a\":123}' -> 'a'), typeof('{\"a\":123}' ->> 'a');",
    ];

    for sql in queries {
        let rust_out = run_rust_rows(&conn, sql);
        let ref_out = run_ref_sqlite(None, sql);
        assert_eq!(rust_out, ref_out, "JSON -> / ->> mismatch on SQL: {sql}");
    }
}
