#![forbid(unsafe_code)]
//! Deterministic, zero-dependency benchmark suite (`W01`..`W14`) for `sqliters`.
//!
//! Modeled on SQLite's `speedtest1.c` extended with window functions, `JSONB`, `FTS5` + `bm25()`,
//! 2D `R-Tree`, autocommit `WAL` transactions, and end-to-end CLI batch execution. Every workload
//! computes a 64-bit FNV-1a output digest so `tools/run_speedbench.py` can verify 100% functional
//! parity against C SQLite 3.54.1 (`-O3`).

use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use sqliters::{Connection, StepResult, Value};

const SEED_BASE: u64 = 0x5351_4C49_5445_3335;
const SEED_STRIDE: u64 = 0x9E37_79B9_7F4A_7C15;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    #[inline]
    fn for_workload(w_num: u64) -> Self {
        Self(SEED_BASE ^ w_num.wrapping_mul(SEED_STRIDE))
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    #[inline]
    fn next_range(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

#[derive(Clone, Copy)]
struct Digest(u64);

impl Digest {
    #[inline]
    fn new() -> Self {
        Self(FNV_OFFSET)
    }

    #[inline]
    fn u8(&mut self, b: u8) {
        self.0 ^= u64::from(b);
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }

    #[inline]
    fn i64(&mut self, v: i64) {
        for b in (v as u64).to_le_bytes() {
            self.u8(b);
        }
    }

    #[inline]
    fn opt_i64(&mut self, v: Option<i64>) {
        match v {
            None => self.u8(0),
            Some(x) => {
                self.u8(1);
                self.i64(x);
            }
        }
    }

    #[inline]
    fn bytes(&mut self, data: &[u8]) {
        self.i64(data.len() as i64);
        for &b in data {
            self.u8(b);
        }
    }

    #[inline]
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    #[inline]
    fn f64_scaled(&mut self, v: f64) {
        let scaled = if v >= 0.0 {
            (v * 10_000.0 + 0.5) as i64
        } else {
            (v * 10_000.0 - 0.5) as i64
        };
        self.i64(scaled);
    }

    #[inline]
    fn hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

#[derive(Debug, Clone)]
struct WorkloadMetric {
    name: &'static str,
    storage: &'static str,
    ops: u64,
    op_unit: &'static str,
    elapsed_ms: f64,
    ops_per_sec: f64,
    digest_hex: String,
}

impl WorkloadMetric {
    fn finish(
        name: &'static str,
        storage: &'static str,
        ops: u64,
        op_unit: &'static str,
        elapsed_secs: f64,
        digest: Digest,
    ) -> Self {
        let elapsed_ms = elapsed_secs * 1_000.0;
        let ops_per_sec = if elapsed_secs > 0.0 {
            (ops as f64) / elapsed_secs
        } else {
            0.0
        };
        Self {
            name,
            storage,
            ops,
            op_unit,
            elapsed_ms,
            ops_per_sec,
            digest_hex: digest.hex(),
        }
    }
}

fn remove_db_family(db_path: &Path) {
    let base = db_path.to_string_lossy();
    let _ = fs::remove_file(db_path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(format!("{base}{suffix}"));
    }
}

fn build_w14_sql_script() -> String {
    let mut rng = Rng::for_workload(14);
    let mut sql = String::with_capacity(220 * 1024);
    sql.push_str(
        "PRAGMA page_size = 4096;\n\
         PRAGMA journal_mode = WAL;\n\
         PRAGMA synchronous = NORMAL;\n\
         CREATE TABLE orders(\n\
           id INTEGER PRIMARY KEY,\n\
           cust INTEGER NOT NULL,\n\
           region TEXT NOT NULL,\n\
           amount INTEGER NOT NULL,\n\
           meta BLOB NOT NULL\n\
         );\n\
         CREATE INDEX idx_orders_cust ON orders(cust, amount);\n\
         BEGIN;\n",
    );
    let regions = ["north", "south", "east", "west", "central"];
    for id in 1..=2_000_i64 {
        let cust = (rng.next_range(120) + 1) as i64;
        let reg = regions[rng.next_range(regions.len() as u64) as usize];
        let amount = (rng.next_range(5_000) + 10) as i64;
        let tier = if amount >= 3_500 {
            "vip"
        } else if amount >= 1_500 {
            "plus"
        } else {
            "std"
        };
        sql.push_str(&format!(
            "INSERT INTO orders VALUES({id}, {cust}, '{reg}', {amount}, jsonb('{{\"tier\":\"{tier}\",\"seq\":{id}}}'));\n"
        ));
    }
    sql.push_str("COMMIT;\n");

    for q in 0..150_u64 {
        let cust = (rng.next_range(120) + 1) as i64;
        let min_amt = (rng.next_range(2_500) + 100) as i64;
        if q % 3 == 0 {
            sql.push_str(&format!(
                "SELECT count(*), coalesce(sum(amount), 0) FROM orders WHERE cust = {cust} AND amount >= {min_amt};\n"
            ));
        } else if q % 3 == 1 {
            sql.push_str(&format!(
                "SELECT id, meta ->> '$.tier' FROM orders WHERE cust = {cust} ORDER BY amount DESC, id ASC LIMIT 5;\n"
            ));
        } else {
            let reg = regions[(q as usize) % regions.len()];
            sql.push_str(&format!(
                "SELECT region, count(*), sum(amount) FROM orders WHERE region = '{reg}' AND amount >= {min_amt} GROUP BY region;\n"
            ));
        }
    }

    sql.push_str("BEGIN;\n");
    for u in 1..=100_i64 {
        let target_id = u * 15;
        let bonus = (rng.next_range(400) + 1) as i64;
        sql.push_str(&format!(
            "UPDATE orders SET amount = amount + {bonus} WHERE id = {target_id};\n"
        ));
    }
    sql.push_str("DELETE FROM orders WHERE id % 40 = 0;\nCOMMIT;\n");

    sql.push_str(
        "SELECT region, rnk, amount FROM (\n\
           SELECT region, id, amount, RANK() OVER (PARTITION BY region ORDER BY amount DESC, id ASC) AS rnk\n\
           FROM orders WHERE id <= 500\n\
         ) WHERE rnk <= 3 ORDER BY region ASC, rnk ASC, amount DESC;\n\
         SELECT count(*), sum(amount) FROM orders;\n\
         PRAGMA integrity_check;\n",
    );
    sql
}

fn default_cli_bin() -> Option<PathBuf> {
    if let Ok(exe) = env::current_exe() {
        // target/release/examples/speedbench -> target/release/sqliters
        if let Some(examples_dir) = exe.parent()
            && let Some(profile_dir) = examples_dir.parent()
        {
            let candidate = profile_dir.join("sqliters");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    let fallback = PathBuf::from("target/release/sqliters");
    if fallback.exists() {
        Some(fallback)
    } else {
        None
    }
}

fn run_suite(
    db_dir: &Path,
    cli_bin: Option<&Path>,
    filter: Option<&str>,
) -> Result<Vec<WorkloadMetric>, String> {
    fs::create_dir_all(db_dir).map_err(|e| format!("create db_dir {}: {e}", db_dir.display()))?;
    let wants = |name: &str| -> bool {
        match filter {
            None => true,
            Some(f) => name.contains(f),
        }
    };
    let need_core_chain = [
        "W01_bulk_insert_prepared",
        "W02_create_indexes",
        "W03_pk_point_select",
        "W04_secondary_idx_range",
        "W05_full_scan_filter_agg",
        "W06_join_group_order",
        "W07_window_functions",
        "W08_indexed_update_delete",
        "W09_like_text_scan",
    ]
    .iter()
    .any(|w| wants(w));

    let mut results = Vec::with_capacity(14);

    if need_core_chain {
        let core_db = db_dir.join("core_wal.db");
        remove_db_family(&core_db);
        let conn = Connection::open(&core_db).map_err(|e| format!("open core_wal.db: {e}"))?;
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE t1(
                 id INTEGER PRIMARY KEY,
                 grp INTEGER NOT NULL,
                 score INTEGER NOT NULL,
                 code TEXT NOT NULL,
                 bio TEXT NOT NULL
             );",
        )
        .map_err(|e| format!("setup core_wal.db: {e}"))?;

        // W01: 10,000 prepared INSERTs inside BEGIN..COMMIT on WAL disk
        {
            let mut rng = Rng::for_workload(1);
            let t0 = Instant::now();
            conn.execute_batch("BEGIN;")
                .map_err(|e| format!("W01 BEGIN: {e}"))?;
            let mut stmt = conn
                .prepare("INSERT INTO t1(id, grp, score, code, bio) VALUES (?1, ?2, ?3, ?4, ?5)")
                .map_err(|e| format!("W01 prepare: {e}"))?;
            for id in 1..=10_000_i64 {
                let grp = rng.next_range(50) as i64;
                let score = rng.next_range(10_000) as i64;
                let code_num = rng.next_range(1_000);
                let tok_num = rng.next_range(64);
                let code = format!("C-{code_num:04}");
                let bio = format!("entry-{id:05}-g{grp:02}-token{tok_num:02}-payload-{score:04}");
                stmt.bind(1, id).map_err(|e| format!("W01 bind1: {e}"))?;
                stmt.bind(2, grp).map_err(|e| format!("W01 bind2: {e}"))?;
                stmt.bind(3, score).map_err(|e| format!("W01 bind3: {e}"))?;
                stmt.bind(4, code).map_err(|e| format!("W01 bind4: {e}"))?;
                stmt.bind(5, bio).map_err(|e| format!("W01 bind5: {e}"))?;
                let _ = stmt.step().map_err(|e| format!("W01 step: {e}"))?;
                stmt.reset().map_err(|e| format!("W01 reset: {e}"))?;
            }
            stmt.finalize().map_err(|e| format!("W01 finalize: {e}"))?;
            conn.execute_batch("COMMIT;")
                .map_err(|e| format!("W01 COMMIT: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();

            let mut dg = Digest::new();
            let agg = conn
                .query_row("SELECT count(*), sum(grp), sum(score) FROM t1", &[])
                .map_err(|e| format!("W01 verify agg: {e}"))?
                .ok_or_else(|| "W01 verify missing row".to_owned())?;
            dg.i64(agg.get_i64(0).unwrap_or(-1));
            dg.i64(agg.get_i64(1).unwrap_or(-1));
            dg.i64(agg.get_i64(2).unwrap_or(-1));
            let sample = conn
                .query(
                    "SELECT code, bio FROM t1 WHERE id IN (1, 2500, 5000, 7500, 10000) ORDER BY id",
                    &[],
                )
                .map_err(|e| format!("W01 verify sample: {e}"))?;
            for r in &sample.rows {
                dg.str(r.get_str(0).unwrap_or(""));
                dg.str(r.get_str(1).unwrap_or(""));
            }
            if wants("W01_bulk_insert_prepared") {
                results.push(WorkloadMetric::finish(
                    "W01_bulk_insert_prepared",
                    "disk_wal",
                    10_000,
                    "inserts",
                    elapsed,
                    dg,
                ));
            }
        }

        // W02: CREATE INDEX x2 over 10,000 rows
        {
            let t0 = Instant::now();
            conn.execute_batch(
                "CREATE INDEX idx_t1_grp_score ON t1(grp, score);
                 CREATE INDEX idx_t1_code ON t1(code);",
            )
            .map_err(|e| format!("W02 create indexes: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();

            let mut dg = Digest::new();
            let idx_rows = conn
                .query(
                    "SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = 't1' ORDER BY name",
                    &[],
                )
                .map_err(|e| format!("W02 verify schema: {e}"))?;
            for r in &idx_rows.rows {
                dg.str(r.get_str(0).unwrap_or(""));
            }
            let probe = conn
                .query("SELECT id FROM t1 WHERE code = 'C-0042' ORDER BY id", &[])
                .map_err(|e| format!("W02 verify probe: {e}"))?;
            dg.i64(probe.rows.len() as i64);
            for r in &probe.rows {
                dg.i64(r.get_i64(0).unwrap_or(-1));
            }
            if wants("W02_create_indexes") {
                results.push(WorkloadMetric::finish(
                    "W02_create_indexes",
                    "disk_wal",
                    2,
                    "indexes",
                    elapsed,
                    dg,
                ));
            }
        }

        // W03: 10,000 random PK point selects
        if wants("W03_pk_point_select") {
            let mut rng = Rng::for_workload(3);
            let mut dg = Digest::new();
            let t0 = Instant::now();
            let mut stmt = conn
                .prepare("SELECT id, grp, score, code FROM t1 WHERE id = ?1")
                .map_err(|e| format!("W03 prepare: {e}"))?;
            for _ in 0..10_000_u64 {
                let id = (rng.next_range(10_000) + 1) as i64;
                stmt.bind(1, id).map_err(|e| format!("W03 bind: {e}"))?;
                while let StepResult::Row(row) =
                    stmt.step().map_err(|e| format!("W03 step: {e}"))?
                {
                    dg.i64(row.get_i64(0).unwrap_or(-1));
                    dg.i64(row.get_i64(1).unwrap_or(-1));
                    dg.i64(row.get_i64(2).unwrap_or(-1));
                    dg.str(row.get_str(3).unwrap_or(""));
                }
                stmt.reset().map_err(|e| format!("W03 reset: {e}"))?;
            }
            stmt.finalize().map_err(|e| format!("W03 finalize: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();
            results.push(WorkloadMetric::finish(
                "W03_pk_point_select",
                "disk_wal",
                10_000,
                "queries",
                elapsed,
                dg,
            ));
        }

        // W04: 2,000 secondary index range scans
        if wants("W04_secondary_idx_range") {
            let mut rng = Rng::for_workload(4);
            let mut dg = Digest::new();
            let t0 = Instant::now();
            let mut stmt = conn
                .prepare("SELECT count(*), sum(score) FROM t1 WHERE grp = ?1 AND score BETWEEN ?2 AND ?3")
                .map_err(|e| format!("W04 prepare: {e}"))?;
            for _ in 0..2_000_u64 {
                let grp = rng.next_range(50) as i64;
                let lo = rng.next_range(8_500) as i64;
                let hi = lo + (rng.next_range(1_500) as i64);
                stmt.bind(1, grp).map_err(|e| format!("W04 bind1: {e}"))?;
                stmt.bind(2, lo).map_err(|e| format!("W04 bind2: {e}"))?;
                stmt.bind(3, hi).map_err(|e| format!("W04 bind3: {e}"))?;
                while let StepResult::Row(row) =
                    stmt.step().map_err(|e| format!("W04 step: {e}"))?
                {
                    dg.i64(row.get_i64(0).unwrap_or(0));
                    dg.opt_i64(row.get_i64(1));
                }
                stmt.reset().map_err(|e| format!("W04 reset: {e}"))?;
            }
            stmt.finalize().map_err(|e| format!("W04 finalize: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();
            results.push(WorkloadMetric::finish(
                "W04_secondary_idx_range",
                "disk_wal",
                2_000,
                "queries",
                elapsed,
                dg,
            ));
        }

        // W05: 20 full-table scans with filter + GROUP BY + ORDER BY
        if wants("W05_full_scan_filter_agg") {
            let mut rng = Rng::for_workload(5);
            let mut dg = Digest::new();
            let t0 = Instant::now();
            let mut stmt = conn
                .prepare(
                    "SELECT grp, count(*), sum(score), avg(score) \
                     FROM t1 WHERE score > ?1 GROUP BY grp ORDER BY grp",
                )
                .map_err(|e| format!("W05 prepare: {e}"))?;
            for _ in 0..20_u64 {
                let threshold = (rng.next_range(7_000) + 500) as i64;
                stmt.bind(1, threshold)
                    .map_err(|e| format!("W05 bind: {e}"))?;
                while let StepResult::Row(row) =
                    stmt.step().map_err(|e| format!("W05 step: {e}"))?
                {
                    dg.i64(row.get_i64(0).unwrap_or(-1));
                    dg.i64(row.get_i64(1).unwrap_or(0));
                    dg.i64(row.get_i64(2).unwrap_or(0));
                    dg.f64_scaled(row.get_f64(3).unwrap_or(0.0));
                }
                stmt.reset().map_err(|e| format!("W05 reset: {e}"))?;
            }
            stmt.finalize().map_err(|e| format!("W05 finalize: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();
            results.push(WorkloadMetric::finish(
                "W05_full_scan_filter_agg",
                "disk_wal",
                20,
                "scans",
                elapsed,
                dg,
            ));
        }

        // W06: Join t1 (10,000 rows) with t2 (1,000 rows) + GROUP BY + ORDER BY (10 queries)
        if wants("W06_join_group_order") {
            conn.execute_batch(
                "CREATE TABLE t2(
                     id INTEGER PRIMARY KEY,
                     code TEXT NOT NULL,
                     category INTEGER NOT NULL,
                     weight INTEGER NOT NULL
                 );
                 CREATE INDEX idx_t2_code ON t2(code);
                 BEGIN;",
            )
            .map_err(|e| format!("W06 setup DDL: {e}"))?;
            {
                let mut ins = conn
                    .prepare("INSERT INTO t2(id, code, category, weight) VALUES (?1, ?2, ?3, ?4)")
                    .map_err(|e| format!("W06 setup prepare: {e}"))?;
                for id in 1..=1_000_i64 {
                    let code = format!("C-{:04}", id - 1);
                    let category = (id - 1) % 20;
                    let weight = ((id * 7) % 13) + 1;
                    ins.bind(1, id).map_err(|e| format!("W06 sbind1: {e}"))?;
                    ins.bind(2, code).map_err(|e| format!("W06 sbind2: {e}"))?;
                    ins.bind(3, category)
                        .map_err(|e| format!("W06 sbind3: {e}"))?;
                    ins.bind(4, weight)
                        .map_err(|e| format!("W06 sbind4: {e}"))?;
                    let _ = ins.step().map_err(|e| format!("W06 sstep: {e}"))?;
                    ins.reset().map_err(|e| format!("W06 sreset: {e}"))?;
                }
                ins.finalize().map_err(|e| format!("W06 sfin: {e}"))?;
            }
            conn.execute_batch("COMMIT;")
                .map_err(|e| format!("W06 setup COMMIT: {e}"))?;

            let mut rng = Rng::for_workload(6);
            let mut dg = Digest::new();
            let t0 = Instant::now();
            let mut stmt = conn
                .prepare(
                    "SELECT t2.category, count(*), sum(t1.score * t2.weight) AS wsum \
                     FROM t2 JOIN t1 ON t1.code = t2.code \
                     WHERE t2.id BETWEEN ?1 AND ?2 \
                     GROUP BY t2.category \
                     ORDER BY wsum DESC, t2.category ASC",
                )
                .map_err(|e| format!("W06 prepare: {e}"))?;
            for _ in 0..10_u64 {
                let lo_id = (rng.next_range(400) + 1) as i64;
                let hi_id = lo_id + 500;
                stmt.bind(1, lo_id).map_err(|e| format!("W06 bind1: {e}"))?;
                stmt.bind(2, hi_id).map_err(|e| format!("W06 bind2: {e}"))?;
                while let StepResult::Row(row) =
                    stmt.step().map_err(|e| format!("W06 step: {e}"))?
                {
                    dg.i64(row.get_i64(0).unwrap_or(-1));
                    dg.i64(row.get_i64(1).unwrap_or(0));
                    dg.i64(row.get_i64(2).unwrap_or(0));
                }
                stmt.reset().map_err(|e| format!("W06 reset: {e}"))?;
            }
            stmt.finalize().map_err(|e| format!("W06 finalize: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();
            results.push(WorkloadMetric::finish(
                "W06_join_group_order",
                "disk_wal",
                10,
                "queries",
                elapsed,
                dg,
            ));
        }

        // W07: Window functions (ROW_NUMBER, RANK, sliding SUM) over 5,000 rows (3 window passes)
        if wants("W07_window_functions") {
            let mut rng = Rng::for_workload(7);
            let mut dg = Digest::new();
            let t0 = Instant::now();
            let mut stmt = conn
                .prepare(
                    "SELECT \
                         id, \
                         ROW_NUMBER() OVER (PARTITION BY grp ORDER BY id), \
                         RANK() OVER (PARTITION BY grp ORDER BY score DESC), \
                         SUM(score) OVER (PARTITION BY grp ORDER BY id ROWS BETWEEN 3 PRECEDING AND CURRENT ROW) \
                     FROM t1 \
                     WHERE id BETWEEN ?1 AND ?2 \
                     ORDER BY id",
                )
                .map_err(|e| format!("W07 prepare: {e}"))?;
            for _ in 0..3_u64 {
                let start_id = (rng.next_range(5_000) + 1) as i64;
                let end_id = start_id + 4_999;
                stmt.bind(1, start_id)
                    .map_err(|e| format!("W07 bind1: {e}"))?;
                stmt.bind(2, end_id)
                    .map_err(|e| format!("W07 bind2: {e}"))?;
                while let StepResult::Row(row) =
                    stmt.step().map_err(|e| format!("W07 step: {e}"))?
                {
                    dg.i64(row.get_i64(0).unwrap_or(-1));
                    dg.i64(row.get_i64(1).unwrap_or(-1));
                    dg.i64(row.get_i64(2).unwrap_or(-1));
                    dg.i64(row.get_i64(3).unwrap_or(0));
                }
                stmt.reset().map_err(|e| format!("W07 reset: {e}"))?;
            }
            stmt.finalize().map_err(|e| format!("W07 finalize: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();
            results.push(WorkloadMetric::finish(
                "W07_window_functions",
                "disk_wal",
                3,
                "queries",
                elapsed,
                dg,
            ));
        }

        // W08: 2,000 prepared indexed UPDATEs + 2,000 prepared indexed DELETEs inside transactions
        if wants("W08_indexed_update_delete") || wants("W09_like_text_scan") {
            let mut rng = Rng::for_workload(8);
            let t0 = Instant::now();
            conn.execute_batch("BEGIN;")
                .map_err(|e| format!("W08 BEGIN upd: {e}"))?;
            let mut upd = conn
                .prepare("UPDATE t1 SET score = ?1, grp = ?2 WHERE id = ?3")
                .map_err(|e| format!("W08 prepare upd: {e}"))?;
            for _ in 0..2_000_u64 {
                let new_score = rng.next_range(10_000) as i64;
                let new_grp = rng.next_range(50) as i64;
                let target_id = (rng.next_range(10_000) + 1) as i64;
                upd.bind(1, new_score)
                    .map_err(|e| format!("W08 ubind1: {e}"))?;
                upd.bind(2, new_grp)
                    .map_err(|e| format!("W08 ubind2: {e}"))?;
                upd.bind(3, target_id)
                    .map_err(|e| format!("W08 ubind3: {e}"))?;
                let _ = upd.step().map_err(|e| format!("W08 ustep: {e}"))?;
                upd.reset().map_err(|e| format!("W08 ureset: {e}"))?;
            }
            upd.finalize().map_err(|e| format!("W08 ufin: {e}"))?;
            conn.execute_batch("COMMIT;")
                .map_err(|e| format!("W08 COMMIT upd: {e}"))?;

            conn.execute_batch("BEGIN;")
                .map_err(|e| format!("W08 BEGIN del: {e}"))?;
            let mut del = conn
                .prepare("DELETE FROM t1 WHERE id = ?1")
                .map_err(|e| format!("W08 prepare del: {e}"))?;
            for i in 1..=2_000_i64 {
                let del_id = i * 5;
                del.bind(1, del_id).map_err(|e| format!("W08 dbind: {e}"))?;
                let _ = del.step().map_err(|e| format!("W08 dstep: {e}"))?;
                del.reset().map_err(|e| format!("W08 dreset: {e}"))?;
            }
            del.finalize().map_err(|e| format!("W08 dfin: {e}"))?;
            conn.execute_batch("COMMIT;")
                .map_err(|e| format!("W08 COMMIT del: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();

            let mut dg = Digest::new();
            let agg = conn
                .query_row("SELECT count(*), sum(grp), sum(score) FROM t1", &[])
                .map_err(|e| format!("W08 verify: {e}"))?
                .ok_or_else(|| "W08 verify missing row".to_owned())?;
            dg.i64(agg.get_i64(0).unwrap_or(0));
            dg.i64(agg.get_i64(1).unwrap_or(0));
            dg.i64(agg.get_i64(2).unwrap_or(0));
            if wants("W08_indexed_update_delete") {
                results.push(WorkloadMetric::finish(
                    "W08_indexed_update_delete",
                    "disk_wal",
                    4_000,
                    "dml_ops",
                    elapsed,
                    dg,
                ));
            }
        }

        // W09: 20 LIKE '%token42%' scans
        if wants("W09_like_text_scan") {
            let mut dg = Digest::new();
            let t0 = Instant::now();
            let mut stmt = conn
                .prepare("SELECT count(*) FROM t1 WHERE bio LIKE '%token42%'")
                .map_err(|e| format!("W09 prepare: {e}"))?;
            for _ in 0..20_u64 {
                while let StepResult::Row(row) =
                    stmt.step().map_err(|e| format!("W09 step: {e}"))?
                {
                    dg.i64(row.get_i64(0).unwrap_or(0));
                }
                stmt.reset().map_err(|e| format!("W09 reset: {e}"))?;
            }
            stmt.finalize().map_err(|e| format!("W09 finalize: {e}"))?;
            let elapsed = t0.elapsed().as_secs_f64();
            results.push(WorkloadMetric::finish(
                "W09_like_text_scan",
                "disk_wal",
                20,
                "scans",
                elapsed,
                dg,
            ));
        }

        conn.try_close()
            .map_err(|e| format!("close core_wal.db: {e}"))?;
        remove_db_family(&core_db);
    }

    // W10: JSONB 5,000 inserts + ->> extraction + json_group_array query (:memory:)
    if wants("W10_jsonb_extract_agg") {
        let conn = Connection::open_in_memory().map_err(|e| format!("W10 open: {e}"))?;
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             CREATE TABLE jdocs(id INTEGER PRIMARY KEY, bucket INTEGER NOT NULL, payload BLOB NOT NULL);
             CREATE INDEX idx_jdocs_bucket ON jdocs(bucket, id);",
        )
        .map_err(|e| format!("W10 DDL: {e}"))?;

        let mut rng = Rng::for_workload(10);
        let mut dg = Digest::new();
        let t0 = Instant::now();

        conn.execute_batch("BEGIN;")
            .map_err(|e| format!("W10 BEGIN: {e}"))?;
        let mut ins = conn
            .prepare("INSERT INTO jdocs(id, bucket, payload) VALUES (?1, ?2, jsonb(?3))")
            .map_err(|e| format!("W10 prepare ins: {e}"))?;
        for id in 1..=5_000_i64 {
            let bucket = rng.next_range(25) as i64;
            let val = rng.next_range(1_000) as i64;
            let active = if (id + val) % 3 != 0 { "true" } else { "false" };
            let json_text = format!(
                "{{\"code\":\"J-{id:04}\",\"active\":{active},\"meta\":{{\"val\":{val},\"grp\":{bucket}}}}}"
            );
            ins.bind(1, id).map_err(|e| format!("W10 ibind1: {e}"))?;
            ins.bind(2, bucket)
                .map_err(|e| format!("W10 ibind2: {e}"))?;
            ins.bind(3, json_text)
                .map_err(|e| format!("W10 ibind3: {e}"))?;
            let _ = ins.step().map_err(|e| format!("W10 istep: {e}"))?;
            ins.reset().map_err(|e| format!("W10 ireset: {e}"))?;
        }
        ins.finalize().map_err(|e| format!("W10 ifin: {e}"))?;
        conn.execute_batch("COMMIT;")
            .map_err(|e| format!("W10 COMMIT: {e}"))?;

        let mut agg_stmt = conn
            .prepare(
                "SELECT bucket, count(*), sum(payload ->> '$.meta.val') \
                 FROM jdocs WHERE (payload ->> '$.active') = 1 \
                 GROUP BY bucket ORDER BY bucket",
            )
            .map_err(|e| format!("W10 prepare agg: {e}"))?;
        while let StepResult::Row(row) =
            agg_stmt.step().map_err(|e| format!("W10 step agg: {e}"))?
        {
            dg.i64(row.get_i64(0).unwrap_or(-1));
            dg.i64(row.get_i64(1).unwrap_or(0));
            dg.i64(row.get_i64(2).unwrap_or(0));
        }
        agg_stmt
            .finalize()
            .map_err(|e| format!("W10 fin agg: {e}"))?;

        let mut grp_stmt = conn
            .prepare(
                "SELECT json_group_array(code) FROM (\
                     SELECT payload ->> '$.code' AS code \
                     FROM jdocs \
                     WHERE bucket = ?1 AND (payload ->> '$.meta.val') >= ?2 \
                     ORDER BY id LIMIT 20\
                 )",
            )
            .map_err(|e| format!("W10 prepare grp: {e}"))?;
        for b in 0..25_i64 {
            let min_val = (b * 37) % 600;
            grp_stmt
                .bind(1, b)
                .map_err(|e| format!("W10 gbind1: {e}"))?;
            grp_stmt
                .bind(2, min_val)
                .map_err(|e| format!("W10 gbind2: {e}"))?;
            while let StepResult::Row(row) =
                grp_stmt.step().map_err(|e| format!("W10 gstep: {e}"))?
            {
                dg.str(row.get_str(0).unwrap_or("[]"));
            }
            grp_stmt.reset().map_err(|e| format!("W10 greset: {e}"))?;
        }
        grp_stmt
            .finalize()
            .map_err(|e| format!("W10 fin grp: {e}"))?;

        let elapsed = t0.elapsed().as_secs_f64();
        conn.try_close().map_err(|e| format!("W10 close: {e}"))?;
        results.push(WorkloadMetric::finish(
            "W10_jsonb_extract_agg",
            "memory",
            5_026,
            "ops",
            elapsed,
            dg,
        ));
    }

    // W11: FTS5 3,000 docs + 200 MATCH queries ordered by bm25() (:memory:)
    if wants("W11_fts5_index_and_bm25") {
        let conn = Connection::open_in_memory().map_err(|e| format!("W11 open: {e}"))?;
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             CREATE VIRTUAL TABLE docs USING fts5(title, body);",
        )
        .map_err(|e| format!("W11 DDL: {e}"))?;

        let mut rng = Rng::for_workload(11);
        let mut dg = Digest::new();
        let t0 = Instant::now();

        conn.execute_batch("BEGIN;")
            .map_err(|e| format!("W11 BEGIN: {e}"))?;
        let mut ins = conn
            .prepare("INSERT INTO docs(rowid, title, body) VALUES (?1, ?2, ?3)")
            .map_err(|e| format!("W11 prepare ins: {e}"))?;
        for doc_id in 1..=3_000_i64 {
            let t0_w = rng.next_range(80);
            let t1_w = rng.next_range(80);
            let t2_w = rng.next_range(80);
            let title = format!("term{t0_w:03} term{t1_w:03} term{t2_w:03}");

            let body_len = (rng.next_range(8) + 6) as usize;
            let mut body = String::with_capacity(body_len * 8);
            for k in 0..body_len {
                if k > 0 {
                    body.push(' ');
                }
                let bw = rng.next_range(200);
                body.push_str(&format!("term{bw:03}"));
            }
            ins.bind(1, doc_id)
                .map_err(|e| format!("W11 ibind1: {e}"))?;
            ins.bind(2, title).map_err(|e| format!("W11 ibind2: {e}"))?;
            ins.bind(3, body).map_err(|e| format!("W11 ibind3: {e}"))?;
            let _ = ins.step().map_err(|e| format!("W11 istep: {e}"))?;
            ins.reset().map_err(|e| format!("W11 ireset: {e}"))?;
        }
        ins.finalize().map_err(|e| format!("W11 ifin: {e}"))?;
        conn.execute_batch("COMMIT;")
            .map_err(|e| format!("W11 COMMIT: {e}"))?;

        let mut qstmt = conn
            .prepare(
                "SELECT rowid, bm25(docs) FROM docs \
                 WHERE docs MATCH ?1 \
                 ORDER BY bm25(docs) ASC, rowid ASC LIMIT 15",
            )
            .map_err(|e| format!("W11 prepare match: {e}"))?;
        for q in 0..200_u64 {
            let w1 = rng.next_range(200);
            let w2 = rng.next_range(80);
            let expr = if q % 2 == 0 {
                format!("term{w1:03}")
            } else {
                format!("term{w1:03} OR term{w2:03}")
            };
            qstmt.bind(1, expr).map_err(|e| format!("W11 qbind: {e}"))?;
            while let StepResult::Row(row) = qstmt.step().map_err(|e| format!("W11 qstep: {e}"))? {
                dg.i64(row.get_i64(0).unwrap_or(-1));
                dg.f64_scaled(row.get_f64(1).unwrap_or(0.0));
            }
            qstmt.reset().map_err(|e| format!("W11 qreset: {e}"))?;
        }
        qstmt.finalize().map_err(|e| format!("W11 qfin: {e}"))?;

        let elapsed = t0.elapsed().as_secs_f64();
        conn.try_close().map_err(|e| format!("W11 close: {e}"))?;
        results.push(WorkloadMetric::finish(
            "W11_fts5_index_and_bm25",
            "memory",
            3_200,
            "ops",
            elapsed,
            dg,
        ));
    }

    // W12: R-Tree 5,000 2D boxes + 500 bounding-box intersection queries (:memory:)
    if wants("W12_rtree_insert_and_bbox") {
        let conn = Connection::open_in_memory().map_err(|e| format!("W12 open: {e}"))?;
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             CREATE VIRTUAL TABLE boxes USING rtree(id, minX, maxX, minY, maxY);",
        )
        .map_err(|e| format!("W12 DDL: {e}"))?;

        let mut rng = Rng::for_workload(12);
        let mut dg = Digest::new();
        let t0 = Instant::now();

        conn.execute_batch("BEGIN;")
            .map_err(|e| format!("W12 BEGIN: {e}"))?;
        let mut ins = conn
            .prepare("INSERT INTO boxes(id, minX, maxX, minY, maxY) VALUES (?1, ?2, ?3, ?4, ?5)")
            .map_err(|e| format!("W12 prepare ins: {e}"))?;
        for id in 1..=5_000_i64 {
            let x0 = rng.next_range(9_500) as i64;
            let w = (rng.next_range(400) + 10) as i64;
            let y0 = rng.next_range(9_500) as i64;
            let h = (rng.next_range(400) + 10) as i64;
            ins.bind(1, id).map_err(|e| format!("W12 ibind1: {e}"))?;
            ins.bind(2, Value::real(x0 as f64))
                .map_err(|e| format!("W12 ibind2: {e}"))?;
            ins.bind(3, Value::real((x0 + w) as f64))
                .map_err(|e| format!("W12 ibind3: {e}"))?;
            ins.bind(4, Value::real(y0 as f64))
                .map_err(|e| format!("W12 ibind4: {e}"))?;
            ins.bind(5, Value::real((y0 + h) as f64))
                .map_err(|e| format!("W12 ibind5: {e}"))?;
            let _ = ins.step().map_err(|e| format!("W12 istep: {e}"))?;
            ins.reset().map_err(|e| format!("W12 ireset: {e}"))?;
        }
        ins.finalize().map_err(|e| format!("W12 ifin: {e}"))?;
        conn.execute_batch("COMMIT;")
            .map_err(|e| format!("W12 COMMIT: {e}"))?;

        let mut qstmt = conn
            .prepare(
                "SELECT count(*), coalesce(sum(id), 0) FROM boxes \
                 WHERE maxX >= ?1 AND minX <= ?2 AND maxY >= ?3 AND minY <= ?4",
            )
            .map_err(|e| format!("W12 prepare bbox: {e}"))?;
        for _ in 0..500_u64 {
            let qx0 = rng.next_range(8_500) as i64;
            let qw = (rng.next_range(1_200) + 100) as i64;
            let qy0 = rng.next_range(8_500) as i64;
            let qh = (rng.next_range(1_200) + 100) as i64;
            qstmt
                .bind(1, Value::real(qx0 as f64))
                .map_err(|e| format!("W12 qbind1: {e}"))?;
            qstmt
                .bind(2, Value::real((qx0 + qw) as f64))
                .map_err(|e| format!("W12 qbind2: {e}"))?;
            qstmt
                .bind(3, Value::real(qy0 as f64))
                .map_err(|e| format!("W12 qbind3: {e}"))?;
            qstmt
                .bind(4, Value::real((qy0 + qh) as f64))
                .map_err(|e| format!("W12 qbind4: {e}"))?;
            while let StepResult::Row(row) = qstmt.step().map_err(|e| format!("W12 qstep: {e}"))? {
                dg.i64(row.get_i64(0).unwrap_or(0));
                dg.i64(row.get_i64(1).unwrap_or(0));
            }
            qstmt.reset().map_err(|e| format!("W12 qreset: {e}"))?;
        }
        qstmt.finalize().map_err(|e| format!("W12 qfin: {e}"))?;

        let elapsed = t0.elapsed().as_secs_f64();
        conn.try_close().map_err(|e| format!("W12 close: {e}"))?;
        results.push(WorkloadMetric::finish(
            "W12_rtree_insert_and_bbox",
            "memory",
            5_500,
            "ops",
            elapsed,
            dg,
        ));
    }

    // W13: 300 individual autocommit WAL INSERT transactions (synchronous = NORMAL)
    if wants("W13_autocommit_wal_txns") {
        let w13_db = db_dir.join("w13_autocommit.db");
        remove_db_family(&w13_db);
        let conn = Connection::open(&w13_db).map_err(|e| format!("W13 open: {e}"))?;
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE wal_events(
                 id INTEGER PRIMARY KEY,
                 ts INTEGER NOT NULL,
                 stream TEXT NOT NULL,
                 detail TEXT NOT NULL
             );
             CREATE INDEX idx_wal_events_stream ON wal_events(stream, ts);",
        )
        .map_err(|e| format!("W13 setup: {e}"))?;

        let mut rng = Rng::for_workload(13);
        let t0 = Instant::now();
        let mut ins = conn
            .prepare("INSERT INTO wal_events(id, ts, stream, detail) VALUES (?1, ?2, ?3, ?4)")
            .map_err(|e| format!("W13 prepare: {e}"))?;
        for id in 1..=300_i64 {
            let ts = id * 10 + (rng.next_range(9) as i64);
            let s_idx = rng.next_range(8);
            let stream = format!("s{s_idx}");
            let detail = format!("evt-{id:04}-ts-{ts}");
            ins.bind(1, id).map_err(|e| format!("W13 bind1: {e}"))?;
            ins.bind(2, ts).map_err(|e| format!("W13 bind2: {e}"))?;
            ins.bind(3, stream).map_err(|e| format!("W13 bind3: {e}"))?;
            ins.bind(4, detail).map_err(|e| format!("W13 bind4: {e}"))?;
            let _ = ins.step().map_err(|e| format!("W13 step: {e}"))?;
            ins.reset().map_err(|e| format!("W13 reset: {e}"))?;
        }
        ins.finalize().map_err(|e| format!("W13 finalize: {e}"))?;
        let elapsed = t0.elapsed().as_secs_f64();

        let mut dg = Digest::new();
        let agg = conn
            .query_row("SELECT count(*), sum(ts) FROM wal_events", &[])
            .map_err(|e| format!("W13 verify: {e}"))?
            .ok_or_else(|| "W13 verify missing row".to_owned())?;
        dg.i64(agg.get_i64(0).unwrap_or(0));
        dg.i64(agg.get_i64(1).unwrap_or(0));

        conn.try_close().map_err(|e| format!("W13 close: {e}"))?;
        remove_db_family(&w13_db);
        results.push(WorkloadMetric::finish(
            "W13_autocommit_wal_txns",
            "disk_wal",
            300,
            "txns",
            elapsed,
            dg,
        ));
    }

    // W14: End-to-end CLI SQL batch script execution
    if wants("W14_cli_sql_script")
        && let Some(cli) = cli_bin
    {
        let w14_db = db_dir.join("w14_cli.db");
        let w14_sql = db_dir.join("w14_batch.sql");
        let w14_out = db_dir.join("w14_stdout.txt");
        remove_db_family(&w14_db);
        let script = build_w14_sql_script();
        {
            let mut f =
                File::create(&w14_sql).map_err(|e| format!("W14 create script file: {e}"))?;
            f.write_all(script.as_bytes())
                .map_err(|e| format!("W14 write script file: {e}"))?;
            f.flush()
                .map_err(|e| format!("W14 flush script file: {e}"))?;
        }

        let stdin_file = File::open(&w14_sql).map_err(|e| format!("W14 open stdin: {e}"))?;
        let stdout_file = File::create(&w14_out).map_err(|e| format!("W14 create stdout: {e}"))?;

        let t0 = Instant::now();
        let status = Command::new(cli)
            .args(["-list", "-noheader"])
            .arg(&w14_db)
            .stdin(Stdio::from(stdin_file))
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::piped())
            .status()
            .map_err(|e| format!("W14 spawn CLI {}: {e}", cli.display()))?;
        let elapsed = t0.elapsed().as_secs_f64();

        if !status.success() {
            return Err(format!("W14 CLI exited with status {status}"));
        }
        let stdout_bytes = fs::read(&w14_out).map_err(|e| format!("W14 read stdout: {e}"))?;
        let mut dg = Digest::new();
        dg.bytes(&stdout_bytes);

        remove_db_family(&w14_db);
        let _ = fs::remove_file(&w14_sql);
        let _ = fs::remove_file(&w14_out);

        results.push(WorkloadMetric::finish(
            "W14_cli_sql_script",
            "disk_wal_cli",
            2_260,
            "sql_stmts",
            elapsed,
            dg,
        ));
    }

    Ok(results)
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn emit_json(engine: &str, results: &[WorkloadMetric]) {
    println!("{{");
    println!("  \"engine\": \"{}\",", json_escape(engine));
    println!("  \"sqlite_version\": \"{}\",", sqliters::libversion());
    println!("  \"workloads\": [");
    for (idx, m) in results.iter().enumerate() {
        let comma = if idx + 1 < results.len() { "," } else { "" };
        println!(
            "    {{\"name\": \"{}\", \"storage\": \"{}\", \"ops\": {}, \"op_unit\": \"{}\", \"elapsed_ms\": {:.6}, \"ops_per_sec\": {:.3}, \"digest\": \"{}\"}}{}",
            json_escape(m.name),
            json_escape(m.storage),
            m.ops,
            json_escape(m.op_unit),
            m.elapsed_ms,
            m.ops_per_sec,
            m.digest_hex,
            comma
        );
    }
    println!("  ]");
    println!("}}");
}

fn emit_table(results: &[WorkloadMetric]) {
    println!(
        "{:<28} {:<13} {:>10} {:>12} {:>14}  {:<16}",
        "Workload", "Storage", "Ops", "Time (ms)", "Ops/sec", "Digest (FNV-1a)"
    );
    println!("{}", "-".repeat(99));
    let mut total_ms = 0.0;
    for m in results {
        total_ms += m.elapsed_ms;
        println!(
            "{:<28} {:<13} {:>10} {:>12.3} {:>14.1}  {}",
            m.name, m.storage, m.ops, m.elapsed_ms, m.ops_per_sec, m.digest_hex
        );
    }
    println!("{}", "-".repeat(99));
    println!("Total elapsed across workloads: {total_ms:.3} ms");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut db_dir: Option<PathBuf> = None;
    let mut cli_bin: Option<PathBuf> = default_cli_bin();
    let mut filter: Option<String> = None;
    let mut json_mode = false;
    let mut dump_w14_sql: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json_mode = true,
            "--no-cli" => cli_bin = None,
            "--db-dir" if i + 1 < args.len() => {
                i += 1;
                db_dir = Some(PathBuf::from(&args[i]));
            }
            "--cli-bin" if i + 1 < args.len() => {
                i += 1;
                cli_bin = Some(PathBuf::from(&args[i]));
            }
            "--filter" if i + 1 < args.len() => {
                i += 1;
                filter = Some(args[i].clone());
            }
            "--dump-w14-sql" if i + 1 < args.len() => {
                i += 1;
                dump_w14_sql = Some(PathBuf::from(&args[i]));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: speedbench [--db-dir DIR] [--cli-bin PATH | --no-cli] [--filter SUBSTR] [--json] [--dump-w14-sql PATH]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("Unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    if let Some(path) = dump_w14_sql {
        let script = build_w14_sql_script();
        if let Err(e) = fs::write(&path, script) {
            eprintln!("Failed to write W14 SQL script to {}: {e}", path.display());
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    let owned_temp_dir: Option<PathBuf>;
    let work_dir = if let Some(d) = db_dir {
        owned_temp_dir = None;
        d
    } else {
        let d = env::temp_dir().join(format!("sqliters_speedbench_{}", std::process::id()));
        owned_temp_dir = Some(d.clone());
        d
    };

    let res = run_suite(&work_dir, cli_bin.as_deref(), filter.as_deref());
    if let Some(ref td) = owned_temp_dir {
        let _ = fs::remove_dir_all(td);
    }

    match res {
        Ok(metrics) => {
            if json_mode {
                emit_json("sqliters-rust-release", &metrics);
            } else {
                emit_table(&metrics);
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("speedbench error: {err}");
            ExitCode::from(1)
        }
    }
}
