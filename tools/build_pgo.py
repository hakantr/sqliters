#!/usr/bin/env python3
"""
Reproducible LLVM Profile-Guided Optimization (PGO) build & training pipeline
for `sqliters` (`performans-pgo`).

Pipeline stages:
  1. Instrumented build (`-C profile-generate=<dir>`) into isolated `--target-dir target/pgo-gen`
     so instrumented artifacts never pollute `target/release/`.
  2. Independent deterministic representative training workload (distinct PRNG seed `0x50474F5F32303236`,
     distinct schemas and row counts from `speedbench`):
       - Rust API driver exercising WAL bulk `INSERT`, `CREATE INDEX`, PK point `SELECT`,
         secondary-index range/covering scans, `JOIN` + `GROUP BY` + `ORDER BY`,
         `OVER (...)` window frames, indexed `UPDATE`/`DELETE`, `LIKE` scans,
         `JSONB` extract/agg, `FTS5 + bm25()`, 2D `R-Tree`, and autocommit WAL txns.
       - End-to-end `sqliters` CLI shell workload exercising `.import --csv`, `.read`
         autocommit & batch scripts, UUID-key joins, and persistent stdin batch SQL.
  3. Profile merge via `llvm-profdata merge` into `target/pgo-data/merged.profdata`.
  4. Final PGO-optimized `--release` build (`-C profile-use=.../target/pgo-data/merged.profdata`)
     producing `target/release/examples/speedbench` and `target/release/sqliters`.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_PROFDATA = REPO_ROOT / "target" / "pgo-data" / "merged.profdata"

# Default winning compiler/linker flags verified via 20-trial CPU=2 interleaved ABBA
DEFAULT_EXTRA_RUSTFLAGS: list[str] = [
    "-C target-feature=+bmi1,+bmi2,+lzcnt,+popcnt,+movbe",
    "-C link-arg=-fuse-ld=lld",
    "-C link-arg=-Wl,-z,pack-relative-relocs",
    "-C link-arg=-Wl,--gc-sections",
    "-C link-arg=-Wl,--icf=safe",
    "-C link-arg=-Wl,--call-graph-profile-sort=cdsort",
    "-C llvm-args=--enable-split-machine-functions",
    "-C llvm-args=--hot-callsite-threshold=900",
]


PGO_API_TRAINER_RS = r'''#![forbid(unsafe_code)]
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use sqliters::{Connection, StepResult, Value};

const TRAIN_SEED: u64 = 0x5047_4F5F_3230_3236;
const TRAIN_STRIDE: u64 = 0xD1B5_4A32_D192_ED03;

struct Rng(u64);

impl Rng {
    fn new(tag: u64) -> Self {
        Self(TRAIN_SEED ^ tag.wrapping_mul(TRAIN_STRIDE))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn range(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

fn clean_db(p: &Path) {
    let base = p.to_string_lossy();
    let _ = fs::remove_file(p);
    for s in ["-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(format!("{base}{s}"));
    }
}

fn run_training(work_dir: &Path) -> Result<u64, String> {
    fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let mut sink: u64 = 0;

    // 1. Disk WAL core relational & analytical workloads
    let wal_db = work_dir.join("train_core.db");
    clean_db(&wal_db);
    {
        let conn = Connection::open(&wal_db).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE inventory(
                 id INTEGER PRIMARY KEY,
                 dept INTEGER NOT NULL,
                 price INTEGER NOT NULL,
                 sku TEXT NOT NULL,
                 summary TEXT NOT NULL
             );",
        )
        .map_err(|e| e.to_string())?;

        // Bulk prepared INSERT (6,000 rows)
        let mut rng = Rng::new(101);
        conn.execute_batch("BEGIN;").map_err(|e| e.to_string())?;
        let mut ins = conn
            .prepare("INSERT INTO inventory(id, dept, price, sku, summary) VALUES (?1, ?2, ?3, ?4, ?5)")
            .map_err(|e| e.to_string())?;
        for id in 1..=6_000_i64 {
            let dept = rng.range(40) as i64;
            let price = rng.range(8_000) as i64;
            let sku_n = rng.range(600);
            let b_n = rng.range(48);
            let sku = format!("SKU-{sku_n:04}");
            let summary = format!("item-{id:05}-d{dept:02}-batch{b_n:02}-val-{price:04}");
            ins.bind(1, id).map_err(|e| e.to_string())?;
            ins.bind(2, dept).map_err(|e| e.to_string())?;
            ins.bind(3, price).map_err(|e| e.to_string())?;
            ins.bind(4, sku).map_err(|e| e.to_string())?;
            ins.bind(5, summary).map_err(|e| e.to_string())?;
            let _ = ins.step().map_err(|e| e.to_string())?;
            ins.reset().map_err(|e| e.to_string())?;
        }
        ins.finalize().map_err(|e| e.to_string())?;
        conn.execute_batch("COMMIT;").map_err(|e| e.to_string())?;

        // CREATE INDEX x2
        conn.execute_batch(
            "CREATE INDEX idx_inv_dept_price ON inventory(dept, price);
             CREATE INDEX idx_inv_sku ON inventory(sku);",
        )
        .map_err(|e| e.to_string())?;

        // PK point SELECT (6,000 lookups)
        let mut rng = Rng::new(103);
        let mut pk_stmt = conn
            .prepare("SELECT id, dept, price, sku FROM inventory WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        for _ in 0..6_000_u64 {
            let id = (rng.range(6_000) + 1) as i64;
            pk_stmt.bind(1, id).map_err(|e| e.to_string())?;
            while let StepResult::Row(row) = pk_stmt.step().map_err(|e| e.to_string())? {
                sink = sink.wrapping_add(row.get_i64(0).unwrap_or(0) as u64);
                sink = sink.wrapping_add(row.get_i64(2).unwrap_or(0) as u64);
                sink ^= row.get_str(3).map_or(0, |s| s.len() as u64);
            }
            pk_stmt.reset().map_err(|e| e.to_string())?;
        }
        pk_stmt.finalize().map_err(|e| e.to_string())?;

        // Secondary index range + covering scans (1,200 queries)
        let mut rng = Rng::new(104);
        let mut rng_stmt = conn
            .prepare("SELECT count(*), sum(price) FROM inventory WHERE dept = ?1 AND price BETWEEN ?2 AND ?3")
            .map_err(|e| e.to_string())?;
        for _ in 0..1_200_u64 {
            let dept = rng.range(40) as i64;
            let lo = rng.range(6_500) as i64;
            let hi = lo + (rng.range(1_400) as i64);
            rng_stmt.bind(1, dept).map_err(|e| e.to_string())?;
            rng_stmt.bind(2, lo).map_err(|e| e.to_string())?;
            rng_stmt.bind(3, hi).map_err(|e| e.to_string())?;
            while let StepResult::Row(row) = rng_stmt.step().map_err(|e| e.to_string())? {
                sink = sink.wrapping_add(row.get_i64(0).unwrap_or(0) as u64);
                sink = sink.wrapping_add(row.get_i64(1).unwrap_or(0) as u64);
            }
            rng_stmt.reset().map_err(|e| e.to_string())?;
        }
        rng_stmt.finalize().map_err(|e| e.to_string())?;

        // Full-table scan + filter + GROUP BY + ORDER BY (36 scans)
        let mut rng = Rng::new(105);
        let mut scan_stmt = conn
            .prepare(
                "SELECT dept, count(*), sum(price), avg(price)
                 FROM inventory WHERE price > ?1 GROUP BY dept ORDER BY dept",
            )
            .map_err(|e| e.to_string())?;
        for _ in 0..36_u64 {
            let thr = (rng.range(5_500) + 400) as i64;
            scan_stmt.bind(1, thr).map_err(|e| e.to_string())?;
            while let StepResult::Row(row) = scan_stmt.step().map_err(|e| e.to_string())? {
                sink = sink.wrapping_add(row.get_i64(1).unwrap_or(0) as u64);
                sink = sink.wrapping_add(row.get_f64(3).unwrap_or(0.0) as u64);
            }
            scan_stmt.reset().map_err(|e| e.to_string())?;
        }
        scan_stmt.finalize().map_err(|e| e.to_string())?;

        // JOIN + GROUP BY + ORDER BY (600-row dimension table)
        conn.execute_batch(
            "CREATE TABLE suppliers(
                 id INTEGER PRIMARY KEY,
                 sku TEXT NOT NULL,
                 tier INTEGER NOT NULL,
                 mult INTEGER NOT NULL
             );
             CREATE INDEX idx_sup_sku ON suppliers(sku);
             BEGIN;",
        )
        .map_err(|e| e.to_string())?;
        let mut sup_ins = conn
            .prepare("INSERT INTO suppliers(id, sku, tier, mult) VALUES (?1, ?2, ?3, ?4)")
            .map_err(|e| e.to_string())?;
        for id in 1..=600_i64 {
            let sku = format!("SKU-{:04}", id - 1);
            sup_ins.bind(1, id).map_err(|e| e.to_string())?;
            sup_ins.bind(2, sku).map_err(|e| e.to_string())?;
            sup_ins.bind(3, (id - 1) % 16).map_err(|e| e.to_string())?;
            sup_ins.bind(4, ((id * 5) % 11) + 1).map_err(|e| e.to_string())?;
            let _ = sup_ins.step().map_err(|e| e.to_string())?;
            sup_ins.reset().map_err(|e| e.to_string())?;
        }
        sup_ins.finalize().map_err(|e| e.to_string())?;
        conn.execute_batch("COMMIT;").map_err(|e| e.to_string())?;

        let mut rng = Rng::new(106);
        let mut join_stmt = conn
            .prepare(
                "SELECT s.tier, count(*), sum(i.price * s.mult) AS total_v
                 FROM suppliers s JOIN inventory i ON i.sku = s.sku
                 WHERE s.id BETWEEN ?1 AND ?2
                 GROUP BY s.tier
                 ORDER BY total_v DESC, s.tier ASC",
            )
            .map_err(|e| e.to_string())?;
        for _ in 0..36_u64 {
            let lo = (rng.range(250) + 1) as i64;
            join_stmt.bind(1, lo).map_err(|e| e.to_string())?;
            join_stmt.bind(2, lo + 300).map_err(|e| e.to_string())?;
            while let StepResult::Row(row) = join_stmt.step().map_err(|e| e.to_string())? {
                sink = sink.wrapping_add(row.get_i64(2).unwrap_or(0) as u64);
            }
            join_stmt.reset().map_err(|e| e.to_string())?;
        }
        join_stmt.finalize().map_err(|e| e.to_string())?;

        // Window functions (4 passes over 3,000 rows)
        let mut rng = Rng::new(107);
        let mut win_stmt = conn
            .prepare(
                "SELECT id,
                        ROW_NUMBER() OVER (PARTITION BY dept ORDER BY id),
                        RANK() OVER (PARTITION BY dept ORDER BY price DESC),
                        SUM(price) OVER (PARTITION BY dept ORDER BY id ROWS BETWEEN 4 PRECEDING AND CURRENT ROW)
                 FROM inventory
                 WHERE id BETWEEN ?1 AND ?2
                 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        for _ in 0..4_u64 {
            let start = (rng.range(2_800) + 1) as i64;
            win_stmt.bind(1, start).map_err(|e| e.to_string())?;
            win_stmt.bind(2, start + 2_999).map_err(|e| e.to_string())?;
            while let StepResult::Row(row) = win_stmt.step().map_err(|e| e.to_string())? {
                sink = sink.wrapping_add(row.get_i64(3).unwrap_or(0) as u64);
            }
            win_stmt.reset().map_err(|e| e.to_string())?;
        }
        win_stmt.finalize().map_err(|e| e.to_string())?;

        // Indexed UPDATE + DELETE (1,200 + 1,200 ops)
        let mut rng = Rng::new(108);
        conn.execute_batch("BEGIN;").map_err(|e| e.to_string())?;
        let mut upd = conn
            .prepare("UPDATE inventory SET price = ?1, dept = ?2 WHERE id = ?3")
            .map_err(|e| e.to_string())?;
        for _ in 0..1_200_u64 {
            upd.bind(1, rng.range(8_000) as i64).map_err(|e| e.to_string())?;
            upd.bind(2, rng.range(40) as i64).map_err(|e| e.to_string())?;
            upd.bind(3, (rng.range(6_000) + 1) as i64).map_err(|e| e.to_string())?;
            let _ = upd.step().map_err(|e| e.to_string())?;
            upd.reset().map_err(|e| e.to_string())?;
        }
        upd.finalize().map_err(|e| e.to_string())?;
        conn.execute_batch("COMMIT;").map_err(|e| e.to_string())?;

        conn.execute_batch("BEGIN;").map_err(|e| e.to_string())?;
        let mut del = conn
            .prepare("DELETE FROM inventory WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        for k in 1..=1_200_i64 {
            del.bind(1, k * 5).map_err(|e| e.to_string())?;
            let _ = del.step().map_err(|e| e.to_string())?;
            del.reset().map_err(|e| e.to_string())?;
        }
        del.finalize().map_err(|e| e.to_string())?;
        conn.execute_batch("COMMIT;").map_err(|e| e.to_string())?;

        // LIKE scans (40 passes)
        let mut like_stmt = conn
            .prepare("SELECT count(*) FROM inventory WHERE summary LIKE '%batch17%'")
            .map_err(|e| e.to_string())?;
        for _ in 0..40_u64 {
            while let StepResult::Row(row) = like_stmt.step().map_err(|e| e.to_string())? {
                sink = sink.wrapping_add(row.get_i64(0).unwrap_or(0) as u64);
            }
            like_stmt.reset().map_err(|e| e.to_string())?;
        }
        like_stmt.finalize().map_err(|e| e.to_string())?;

        let _ = conn.query_row("SELECT count(*), sum(price) FROM inventory", &[]).map_err(|e| e.to_string())?;
        conn.try_close().map_err(|e| e.to_string())?;
    }
    clean_db(&wal_db);

    // 2. JSONB in-memory extract + json_group_array
    {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE events_json(id INTEGER PRIMARY KEY, shard INTEGER NOT NULL, doc BLOB NOT NULL);
             CREATE INDEX idx_events_shard ON events_json(shard, id);
             BEGIN;",
        )
        .map_err(|e| e.to_string())?;
        let mut rng = Rng::new(110);
        let mut ins = conn
            .prepare("INSERT INTO events_json(id, shard, doc) VALUES (?1, ?2, jsonb(?3))")
            .map_err(|e| e.to_string())?;
        for id in 1..=3_000_i64 {
            let shard = rng.range(20) as i64;
            let amt = rng.range(900) as i64;
            let ok = if (id + amt) % 3 != 0 { "true" } else { "false" };
            let raw = format!(
                "{{\"tag\":\"E-{id:04}\",\"ok\":{ok},\"nested\":{{\"amt\":{amt},\"sh\":{shard}}}}}"
            );
            ins.bind(1, id).map_err(|e| e.to_string())?;
            ins.bind(2, shard).map_err(|e| e.to_string())?;
            ins.bind(3, raw).map_err(|e| e.to_string())?;
            let _ = ins.step().map_err(|e| e.to_string())?;
            ins.reset().map_err(|e| e.to_string())?;
        }
        ins.finalize().map_err(|e| e.to_string())?;
        conn.execute_batch("COMMIT;").map_err(|e| e.to_string())?;

        let rows = conn
            .query(
                "SELECT shard, count(*), sum(doc ->> '$.nested.amt')
                 FROM events_json WHERE (doc ->> '$.ok') = 1
                 GROUP BY shard ORDER BY shard",
                &[],
            )
            .map_err(|e| e.to_string())?;
        sink = sink.wrapping_add(rows.rows.len() as u64);

        let mut grp = conn
            .prepare(
                "SELECT json_group_array(tag) FROM (
                     SELECT doc ->> '$.tag' AS tag
                     FROM events_json
                     WHERE shard = ?1 AND (doc ->> '$.nested.amt') >= ?2
                     ORDER BY id LIMIT 15
                 )",
            )
            .map_err(|e| e.to_string())?;
        for sh in 0..20_i64 {
            grp.bind(1, sh).map_err(|e| e.to_string())?;
            grp.bind(2, (sh * 29) % 500).map_err(|e| e.to_string())?;
            while let StepResult::Row(row) = grp.step().map_err(|e| e.to_string())? {
                sink ^= row.get_str(0).map_or(0, |s| s.len() as u64);
            }
            grp.reset().map_err(|e| e.to_string())?;
        }
        grp.finalize().map_err(|e| e.to_string())?;
        conn.try_close().map_err(|e| e.to_string())?;
    }

    // 3. FTS5 + bm25() in-memory
    {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE articles USING fts5(headline, content);
             BEGIN;",
        )
        .map_err(|e| e.to_string())?;
        let mut rng = Rng::new(111);
        let mut ins = conn
            .prepare("INSERT INTO articles(rowid, headline, content) VALUES (?1, ?2, ?3)")
            .map_err(|e| e.to_string())?;
        for doc_id in 1..=1_800_i64 {
            let h = format!("word{:03} word{:03} word{:03}", rng.range(70), rng.range(70), rng.range(70));
            let n_tok = (rng.range(7) + 5) as usize;
            let mut body = String::with_capacity(n_tok * 8);
            for k in 0..n_tok {
                if k > 0 {
                    body.push(' ');
                }
                body.push_str(&format!("word{:03}", rng.range(160)));
            }
            ins.bind(1, doc_id).map_err(|e| e.to_string())?;
            ins.bind(2, h).map_err(|e| e.to_string())?;
            ins.bind(3, body).map_err(|e| e.to_string())?;
            let _ = ins.step().map_err(|e| e.to_string())?;
            ins.reset().map_err(|e| e.to_string())?;
        }
        ins.finalize().map_err(|e| e.to_string())?;
        conn.execute_batch("COMMIT;").map_err(|e| e.to_string())?;

        let mut qstmt = conn
            .prepare(
                "SELECT rowid, bm25(articles) FROM articles
                 WHERE articles MATCH ?1
                 ORDER BY bm25(articles) ASC, rowid ASC LIMIT 12",
            )
            .map_err(|e| e.to_string())?;
        for q in 0..120_u64 {
            let expr = if q % 2 == 0 {
                format!("word{:03}", rng.range(160))
            } else {
                format!("word{:03} OR word{:03}", rng.range(160), rng.range(70))
            };
            qstmt.bind(1, expr).map_err(|e| e.to_string())?;
            while let StepResult::Row(row) = qstmt.step().map_err(|e| e.to_string())? {
                sink = sink.wrapping_add(row.get_i64(0).unwrap_or(0) as u64);
                sink = sink.wrapping_add((row.get_f64(1).unwrap_or(0.0) * -100.0) as u64);
            }
            qstmt.reset().map_err(|e| e.to_string())?;
        }
        qstmt.finalize().map_err(|e| e.to_string())?;
        conn.try_close().map_err(|e| e.to_string())?;
    }

    // 4. R-Tree 2D insert + bounding-box queries
    {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE cells USING rtree(id, x0, x1, y0, y1);
             BEGIN;",
        )
        .map_err(|e| e.to_string())?;
        let mut rng = Rng::new(112);
        let mut ins = conn
            .prepare("INSERT INTO cells(id, x0, x1, y0, y1) VALUES (?1, ?2, ?3, ?4, ?5)")
            .map_err(|e| e.to_string())?;
        for id in 1..=3_000_i64 {
            let x0 = rng.range(8_000) as f64;
            let w = (rng.range(350) + 12) as f64;
            let y0 = rng.range(8_000) as f64;
            let h = (rng.range(350) + 12) as f64;
            ins.bind(1, id).map_err(|e| e.to_string())?;
            ins.bind(2, Value::real(x0)).map_err(|e| e.to_string())?;
            ins.bind(3, Value::real(x0 + w)).map_err(|e| e.to_string())?;
            ins.bind(4, Value::real(y0)).map_err(|e| e.to_string())?;
            ins.bind(5, Value::real(y0 + h)).map_err(|e| e.to_string())?;
            let _ = ins.step().map_err(|e| e.to_string())?;
            ins.reset().map_err(|e| e.to_string())?;
        }
        ins.finalize().map_err(|e| e.to_string())?;
        conn.execute_batch("COMMIT;").map_err(|e| e.to_string())?;

        let mut qstmt = conn
            .prepare(
                "SELECT count(*), coalesce(sum(id), 0) FROM cells
                 WHERE x1 >= ?1 AND x0 <= ?2 AND y1 >= ?3 AND y0 <= ?4",
            )
            .map_err(|e| e.to_string())?;
        for _ in 0..300_u64 {
            let qx = rng.range(7_000) as f64;
            let qw = (rng.range(1_000) + 80) as f64;
            let qy = rng.range(7_000) as f64;
            let qh = (rng.range(1_000) + 80) as f64;
            qstmt.bind(1, Value::real(qx)).map_err(|e| e.to_string())?;
            qstmt.bind(2, Value::real(qx + qw)).map_err(|e| e.to_string())?;
            qstmt.bind(3, Value::real(qy)).map_err(|e| e.to_string())?;
            qstmt.bind(4, Value::real(qy + qh)).map_err(|e| e.to_string())?;
            while let StepResult::Row(row) = qstmt.step().map_err(|e| e.to_string())? {
                sink = sink.wrapping_add(row.get_i64(1).unwrap_or(0) as u64);
            }
            qstmt.reset().map_err(|e| e.to_string())?;
        }
        qstmt.finalize().map_err(|e| e.to_string())?;
        conn.try_close().map_err(|e| e.to_string())?;
    }

    // 5. Autocommit WAL individual transactions
    let ac_db = work_dir.join("train_ac.db");
    clean_db(&ac_db);
    {
        let conn = Connection::open(&ac_db).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE audit_log(
                 id INTEGER PRIMARY KEY,
                 ts INTEGER NOT NULL,
                 chan TEXT NOT NULL,
                 msg TEXT NOT NULL
             );
             CREATE INDEX idx_audit_chan ON audit_log(chan, ts);",
        )
        .map_err(|e| e.to_string())?;
        let mut rng = Rng::new(113);
        let mut ins = conn
            .prepare("INSERT INTO audit_log(id, ts, chan, msg) VALUES (?1, ?2, ?3, ?4)")
            .map_err(|e| e.to_string())?;
        for id in 1..=180_i64 {
            let ts = id * 12 + (rng.range(11) as i64);
            let chan = format!("ch{}", rng.range(6));
            let msg = format!("audit-{id:04}-{ts}");
            ins.bind(1, id).map_err(|e| e.to_string())?;
            ins.bind(2, ts).map_err(|e| e.to_string())?;
            ins.bind(3, chan).map_err(|e| e.to_string())?;
            ins.bind(4, msg).map_err(|e| e.to_string())?;
            let _ = ins.step().map_err(|e| e.to_string())?;
            ins.reset().map_err(|e| e.to_string())?;
        }
        ins.finalize().map_err(|e| e.to_string())?;
        conn.try_close().map_err(|e| e.to_string())?;
    }
    clean_db(&ac_db);

    Ok(sink)
}

fn main() {
    let dir = env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: pgo_api_trainer <work_dir>");
    match run_training(&dir) {
        Ok(sink) => {
            std::hint::black_box(sink);
        }
        Err(e) => {
            eprintln!("pgo_api_trainer error: {e}");
            std::process::exit(1);
        }
    }
}
'''


def find_llvm_profdata() -> str:
    env_override = os.environ.get("LLVM_PROFDATA")
    if env_override:
        return env_override
    try:
        sysroot = subprocess.run(
            ["rustc", "--print", "sysroot"], text=True, capture_output=True, check=True
        ).stdout.strip()
        for p in Path(sysroot).rglob("llvm-profdata"):
            if os.access(p, os.X_OK):
                return str(p)
    except OSError:
        pass
    which_p = shutil.which("llvm-profdata")
    if which_p:
        return which_p
    raise SystemExit("ERROR: `llvm-profdata` not found in rustup sysroot or PATH")


def run_checked(cmd: list[str], cwd: Path, env: dict[str, str], desc: str) -> None:
    proc = subprocess.run(cmd, cwd=cwd, env=env, text=True, capture_output=True)
    if proc.returncode != 0:
        sys.stderr.write(f"ERROR during {desc}:\n  cmd: {' '.join(cmd)}\n")
        if proc.stdout:
            sys.stderr.write(proc.stdout + "\n")
        if proc.stderr:
            sys.stderr.write(proc.stderr + "\n")
        raise SystemExit(1)


def run_cli_training(cli_bin: Path, work_dir: Path, profraw_dir: Path) -> None:
    work_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["LLVM_PROFILE_FILE"] = str(profraw_dir / "cli_%p_%m.profraw")

    # 1. CSV `.import` training (3,500 rows)
    csv_path = work_dir / "train_import.csv"
    with csv_path.open("w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        for i in range(1, 3501):
            dept = f"dept_{i % 24:02d}"
            uid = f"018f4a00-{i:04x}-7000-8000-{i * 13:012x}"
            w.writerow([i, uid, dept, (i * 37) % 10000, f"note,with,\"quotes\",row-{i}"])

    imp_db = work_dir / "cli_import.db"
    imp_sql = (
        "PRAGMA page_size = 4096;\n"
        "PRAGMA journal_mode = WAL;\n"
        "PRAGMA synchronous = NORMAL;\n"
        "CREATE TABLE imp_tbl(id INTEGER PRIMARY KEY, uid TEXT, dept TEXT, score INTEGER, note TEXT);\n"
        ".mode csv\n"
        f".import {json.dumps(str(csv_path))} imp_tbl\n"
        "CREATE INDEX idx_imp_uid ON imp_tbl(uid);\n"
        "SELECT dept, count(*), sum(score) FROM imp_tbl GROUP BY dept ORDER BY dept;\n"
    )
    proc = subprocess.run(
        [str(cli_bin), "-batch", str(imp_db)],
        input=imp_sql,
        cwd=work_dir,
        env=env,
        text=True,
        capture_output=True,
    )
    if proc.returncode != 0:
        raise SystemExit(f"CLI .import training failed:\n{proc.stderr}")

    # 2. `.read` batch + autocommit + UUID join training
    batch_sql_path = work_dir / "train_batch.sql"
    lines = [
        "PRAGMA page_size = 4096;",
        "PRAGMA journal_mode = WAL;",
        "PRAGMA synchronous = NORMAL;",
        "CREATE TABLE tx_items(id INTEGER PRIMARY KEY, uid TEXT NOT NULL, bucket TEXT NOT NULL, val INTEGER NOT NULL, meta BLOB NOT NULL);",
        "CREATE INDEX idx_tx_bucket ON tx_items(bucket, val);",
        "BEGIN;",
    ]
    buckets = ["alpha", "bravo", "charlie", "delta", "echo"]
    for i in range(1, 1601):
        uid = f"018f4a00-{i:04x}-7000-8000-{i * 13:012x}"
        b = buckets[i % len(buckets)]
        v = (i * 53) % 4500 + 10
        lvl = "high" if v >= 3000 else ("mid" if v >= 1200 else "low")
        lines.append(
            f"INSERT INTO tx_items VALUES({i}, '{uid}', '{b}', {v}, jsonb('{{\"lvl\":\"{lvl}\",\"n\":{i}}}'));"
        )
    lines.append("COMMIT;")
    for q in range(120):
        b = buckets[q % len(buckets)]
        thr = (q * 31) % 2500 + 100
        if q % 3 == 0:
            lines.append(
                f"SELECT count(*), coalesce(sum(val), 0) FROM tx_items WHERE bucket = '{b}' AND val >= {thr};"
            )
        elif q % 3 == 1:
            lines.append(
                f"SELECT id, meta ->> '$.lvl' FROM tx_items WHERE bucket = '{b}' ORDER BY val DESC, id ASC LIMIT 6;"
            )
        else:
            lines.append(
                f"SELECT bucket, count(*), sum(val) FROM tx_items WHERE val >= {thr} GROUP BY bucket ORDER BY bucket;"
            )
    lines.append("BEGIN;")
    for u in range(1, 81):
        lines.append(f"UPDATE tx_items SET val = val + {u * 3} WHERE id = {u * 15};")
    lines.append("DELETE FROM tx_items WHERE id % 35 = 0;")
    lines.append("COMMIT;")
    lines.append("PRAGMA integrity_check;")
    batch_sql_path.write_text("\n".join(lines) + "\n", encoding="utf-8")

    read_db = work_dir / "cli_read.db"
    proc2 = subprocess.run(
        [str(cli_bin), "-list", "-noheader", str(read_db)],
        input=f".read {json.dumps(str(batch_sql_path))}\n",
        cwd=work_dir,
        env=env,
        text=True,
        capture_output=True,
    )
    if proc2.returncode != 0:
        raise SystemExit(f"CLI .read training failed:\n{proc2.stderr}")

    # 3. Direct stdin persistent batch + join against imported UUID table
    join_sql = (
        f"ATTACH DATABASE '{imp_db}' AS imp;\n"
        "SELECT t.bucket, count(*), sum(t.val + i.score) "
        "FROM tx_items t JOIN imp.imp_tbl i ON i.uid = t.uid "
        "GROUP BY t.bucket ORDER BY t.bucket;\n"
    )
    proc3 = subprocess.run(
        [str(cli_bin), "-list", "-noheader", str(read_db)],
        input=join_sql,
        cwd=work_dir,
        env=env,
        text=True,
        capture_output=True,
    )
    if proc3.returncode != 0:
        raise SystemExit(f"CLI join training failed:\n{proc3.stderr}")


def find_latest_rlib(deps_dir: Path, prefix: str) -> Path:
    candidates = sorted(deps_dir.glob(f"{prefix}-*.rlib"), key=lambda p: p.stat().st_mtime, reverse=True)
    if not candidates:
        raise SystemExit(f"Missing `{prefix}-*.rlib` in {deps_dir}")
    return candidates[0]


def build_pgo(
    extra_rustflags: list[str],
    profdata_out: Path,
    reuse_profdata: bool,
    keep_pgo_gen: bool,
) -> None:
    t0 = time.perf_counter()
    llvm_profdata = find_llvm_profdata()
    pgo_gen_target = REPO_ROOT / "target" / "pgo-gen"
    pgo_data_dir = profdata_out.parent
    profraw_dir = pgo_data_dir / "profraw"

    if not (reuse_profdata and profdata_out.exists()):
        sys.stderr.write("[PGO 1/4] Building instrumented library + CLI in target/pgo-gen/...\n")
        if profraw_dir.exists():
            shutil.rmtree(profraw_dir)
        profraw_dir.mkdir(parents=True, exist_ok=True)

        gen_flags = [*extra_rustflags, f"-C profile-generate={profraw_dir}"]
        gen_env = os.environ.copy()
        gen_env.pop("CARGO_ENCODED_RUSTFLAGS", None)
        gen_env["CARGO_TARGET_DIR"] = str(pgo_gen_target)
        gen_env["RUSTFLAGS"] = " ".join(gen_flags)

        run_checked(
            ["cargo", "build", "--release", "-p", "sqliters", "--lib", "-p", "sqliters-cli", "--bin", "sqliters"],
            cwd=REPO_ROOT,
            env=gen_env,
            desc="instrumented cargo build (--lib + --bin sqliters)",
        )

        deps_dir = pgo_gen_target / "release" / "deps"
        sqliters_rlib = find_latest_rlib(deps_dir, "libsqliters")
        inst_cli = pgo_gen_target / "release" / "sqliters"

        sys.stderr.write("[PGO 2/4] Compiling & running independent API + CLI training workloads...\n")
        with tempfile.TemporaryDirectory(prefix="sqliters_pgo_train_") as tmp_dir:
            tmp_p = Path(tmp_dir)
            trainer_rs = tmp_p / "pgo_api_trainer.rs"
            trainer_bin = tmp_p / "pgo_api_trainer"
            trainer_rs.write_text(PGO_API_TRAINER_RS, encoding="utf-8")

            rustc_cmd = [
                "rustc",
                "--edition=2024",
                "-C",
                "opt-level=3",
                "-C",
                "lto=fat",
                "-C",
                "codegen-units=1",
                *shlex.split(" ".join(gen_flags)),
                "--extern",
                f"sqliters={sqliters_rlib}",
                "-L",
                f"dependency={deps_dir}",
                str(trainer_rs),
                "-o",
                str(trainer_bin),
            ]
            run_checked(rustc_cmd, cwd=REPO_ROOT, env=os.environ.copy(), desc="compile pgo_api_trainer")

            api_env = os.environ.copy()
            api_env["LLVM_PROFILE_FILE"] = str(profraw_dir / "api_%p_%m.profraw")
            run_checked(
                [str(trainer_bin), str(tmp_p / "api_work")],
                cwd=REPO_ROOT,
                env=api_env,
                desc="run pgo_api_trainer",
            )

            run_cli_training(inst_cli, tmp_p / "cli_work", profraw_dir)

        profraw_files = sorted(profraw_dir.glob("*.profraw"))
        if not profraw_files:
            raise SystemExit(f"No .profraw files produced in {profraw_dir}")

        sys.stderr.write(
            f"[PGO 3/4] Merging {len(profraw_files)} profile(s) -> {profdata_out.relative_to(REPO_ROOT)}...\n"
        )
        pgo_data_dir.mkdir(parents=True, exist_ok=True)
        run_checked(
            [llvm_profdata, "merge", "-o", str(profdata_out), *[str(p) for p in profraw_files]],
            cwd=REPO_ROOT,
            env=os.environ.copy(),
            desc="llvm-profdata merge",
        )
        shutil.rmtree(profraw_dir, ignore_errors=True)
        if not keep_pgo_gen:
            shutil.rmtree(pgo_gen_target, ignore_errors=True)
    else:
        sys.stderr.write(f"[PGO 1-3/4] Reusing existing {profdata_out.relative_to(REPO_ROOT)}\n")

    sys.stderr.write("[PGO 4/4] Building final PGO-optimized release binaries in target/release/...\n")
    use_flags = [
        *extra_rustflags,
        f"-C profile-use={profdata_out.resolve()}",
        "-C llvm-args=-pgo-warn-missing-function",
    ]
    use_env = os.environ.copy()
    use_env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    use_env.pop("CARGO_TARGET_DIR", None)
    use_env["RUSTFLAGS"] = " ".join(use_flags)

    run_checked(
        [
            "cargo",
            "build",
            "--release",
            "-p",
            "sqliters",
            "--example",
            "speedbench",
            "-p",
            "sqliters-cli",
            "--bin",
            "sqliters",
        ],
        cwd=REPO_ROOT,
        env=use_env,
        desc="final PGO-optimized cargo build --release",
    )

    sb_bin = REPO_ROOT / "target" / "release" / "examples" / "speedbench"
    cli_bin = REPO_ROOT / "target" / "release" / "sqliters"
    if not sb_bin.exists() or not cli_bin.exists():
        raise SystemExit("Expected PGO binaries missing under target/release/")

    elapsed = time.perf_counter() - t0
    sys.stderr.write(
        f"[PGO DONE] Built {cli_bin.relative_to(REPO_ROOT)} ({cli_bin.stat().st_size / 1024:.1f} KiB) "
        f"and {sb_bin.relative_to(REPO_ROOT)} in {elapsed:.1f}s\n"
    )


def get_winning_use_rustflags(profdata_path: Path = DEFAULT_PROFDATA) -> str:
    flags = [
        *DEFAULT_EXTRA_RUSTFLAGS,
        f"-C profile-use={profdata_path.resolve()}",
        "-C llvm-args=-pgo-warn-missing-function",
    ]
    return " ".join(flags)


def main() -> int:
    ap = argparse.ArgumentParser(description="Build PGO-optimized sqliters and speedbench binaries.")
    ap.add_argument(
        "--extra-rustflags",
        type=str,
        default=None,
        help="Override extra RUSTFLAGS applied identically in profile-generate and profile-use",
    )
    ap.add_argument(
        "--profdata",
        type=Path,
        default=DEFAULT_PROFDATA,
        help="Path to merged.profdata (default: target/pgo-data/merged.profdata)",
    )
    ap.add_argument(
        "--reuse-profdata",
        action="store_true",
        help="Skip instrumentation/training if merged.profdata already exists",
    )
    ap.add_argument(
        "--keep-pgo-gen",
        action="store_true",
        help="Keep target/pgo-gen directory after merging profiles",
    )
    ap.add_argument(
        "--print-use-rustflags",
        action="store_true",
        help="Print the exact RUSTFLAGS for -C profile-use and exit",
    )
    args = ap.parse_args()

    if args.print_use_rustflags:
        print(get_winning_use_rustflags(args.profdata))
        return 0

    extra = shlex.split(args.extra_rustflags) if args.extra_rustflags is not None else list(DEFAULT_EXTRA_RUSTFLAGS)
    build_pgo(
        extra_rustflags=extra,
        profdata_out=args.profdata.resolve(),
        reuse_profdata=args.reuse_profdata,
        keep_pgo_gen=args.keep_pgo_gen,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
