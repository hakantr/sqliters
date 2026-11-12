#!/usr/bin/env python3
"""
Deterministic baseline performance comparison harness for `sqliters` vs C SQLite 3.54.1 (-O3).

Builds:
  1. Rust `examples/speedbench.rs` (`--release`) and `target/release/sqliters` CLI
  2. C SQLite 3.54.1 `-O3` amalgamation object (`target/sqlite-ref/sqlite3_o3_bench.o`),
     C API harness (`target/sqlite-ref/c_speedbench`), and C CLI (`target/sqlite-ref/sqlite3_o3`)
     strictly inside `target/sqlite-ref/` from `target/sqlite-ref/amalg/{sqlite3.c,sqlite3.h,shell.c}`.

Runs 1 warmup + 3 interleaved timed iterations across all 14 workloads (W01..W14),
asserts 100% FNV-1a output digest equality between Rust and C on every trial, writes
structured JSON to `target/sqlite-ref/baseline_main_bench.json`, and prints a Markdown report.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

C_SPEEDBENCH_SOURCE = r"""
#define _DEFAULT_SOURCE
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "sqlite3.h"

#define SEED_BASE   UINT64_C(0x53514C4954453335)
#define SEED_STRIDE UINT64_C(0x9E3779B97F4A7C15)
#define FNV_OFFSET  UINT64_C(0xcbf29ce484222325)
#define FNV_PRIME   UINT64_C(0x00000100000001b3)

typedef struct {
    uint64_t state;
} Rng;

static Rng rng_for_workload(uint64_t w_num) {
    Rng r;
    r.state = SEED_BASE ^ (w_num * SEED_STRIDE);
    return r;
}

static uint64_t rng_next_u64(Rng *r) {
    uint64_t x = r->state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    r->state = x;
    return x * UINT64_C(0x2545F4914F6CDD1D);
}

static uint64_t rng_next_range(Rng *r, uint64_t bound) {
    return rng_next_u64(r) % bound;
}

typedef struct {
    uint64_t h;
} Digest;

static Digest digest_new(void) {
    Digest d;
    d.h = FNV_OFFSET;
    return d;
}

static void digest_u8(Digest *d, uint8_t b) {
    d->h ^= (uint64_t)b;
    d->h *= FNV_PRIME;
}

static void digest_i64(Digest *d, int64_t v) {
    uint64_t u = (uint64_t)v;
    for (int i = 0; i < 8; i++) {
        digest_u8(d, (uint8_t)((u >> (i * 8)) & 0xffu));
    }
}

static void digest_opt_i64(Digest *d, int is_some, int64_t v) {
    if (!is_some) {
        digest_u8(d, 0);
    } else {
        digest_u8(d, 1);
        digest_i64(d, v);
    }
}

static void digest_bytes(Digest *d, const uint8_t *data, size_t len) {
    digest_i64(d, (int64_t)len);
    for (size_t i = 0; i < len; i++) {
        digest_u8(d, data[i]);
    }
}

static void digest_cstr(Digest *d, const char *s) {
    if (!s) {
        s = "";
    }
    digest_bytes(d, (const uint8_t *)s, strlen(s));
}

static void digest_f64_scaled(Digest *d, double v) {
    int64_t scaled = (v >= 0.0)
        ? (int64_t)(v * 10000.0 + 0.5)
        : (int64_t)(v * 10000.0 - 0.5);
    digest_i64(d, scaled);
}

static double now_sec(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec * 1e-9;
}

static void die_db(sqlite3 *db, const char *ctx, int rc) {
    fprintf(stderr, "c_speedbench error at %s: rc=%d msg=%s\n",
            ctx, rc, db ? sqlite3_errmsg(db) : "no-db");
    exit(1);
}

static void exec_sql(sqlite3 *db, const char *sql, const char *ctx) {
    char *errmsg = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "c_speedbench exec error at %s: rc=%d msg=%s\n",
                ctx, rc, errmsg ? errmsg : "");
        sqlite3_free(errmsg);
        exit(1);
    }
}

static sqlite3_stmt *prep_sql(sqlite3 *db, const char *sql, const char *ctx) {
    sqlite3_stmt *stmt = NULL;
    int rc = sqlite3_prepare_v2(db, sql, -1, &stmt, NULL);
    if (rc != SQLITE_OK || !stmt) {
        die_db(db, ctx, rc);
    }
    return stmt;
}

static void remove_db_family(const char *db_path) {
    char buf[2048];
    unlink(db_path);
    const char *suffixes[3] = {"-wal", "-shm", "-journal"};
    for (int i = 0; i < 3; i++) {
        snprintf(buf, sizeof(buf), "%s%s", db_path, suffixes[i]);
        unlink(buf);
    }
}

static void read_proc_mem_kib(uint64_t *rss_kib, uint64_t *hwm_kib) {
    *rss_kib = 0;
    *hwm_kib = 0;
    FILE *fp = fopen("/proc/self/status", "r");
    if (!fp) {
        return;
    }
    char line[256];
    while (fgets(line, sizeof(line), fp)) {
        unsigned long long val = 0;
        if (strncmp(line, "VmRSS:", 6) == 0) {
            if (sscanf(line + 6, "%llu", &val) == 1) {
                *rss_kib = (uint64_t)val;
            }
        } else if (strncmp(line, "VmHWM:", 6) == 0) {
            if (sscanf(line + 6, "%llu", &val) == 1) {
                *hwm_kib = (uint64_t)val;
            }
        }
    }
    fclose(fp);
}

typedef struct {
    const char *name;
    const char *storage;
    uint64_t ops;
    const char *op_unit;
    double elapsed_ms;
    double ops_per_sec;
    uint64_t rss_kib;
    uint64_t hwm_kib;
    uint64_t sqlite_heap_hi_kib;
    uint64_t digest;
} WorkloadMetric;

static WorkloadMetric make_metric(
    const char *name,
    const char *storage,
    uint64_t ops,
    const char *op_unit,
    double elapsed_sec,
    Digest dg
) {
    WorkloadMetric m;
    uint64_t rss_kib = 0;
    uint64_t hwm_kib = 0;
    read_proc_mem_kib(&rss_kib, &hwm_kib);
    sqlite3_int64 mem_cur = 0;
    sqlite3_int64 mem_hi = 0;
    sqlite3_status64(SQLITE_STATUS_MEMORY_USED, &mem_cur, &mem_hi, 1);
    m.name = name;
    m.storage = storage;
    m.ops = ops;
    m.op_unit = op_unit;
    m.elapsed_ms = elapsed_sec * 1000.0;
    m.ops_per_sec = (elapsed_sec > 0.0) ? ((double)ops / elapsed_sec) : 0.0;
    m.rss_kib = rss_kib;
    m.hwm_kib = hwm_kib;
    m.sqlite_heap_hi_kib = (mem_hi > 0) ? (uint64_t)((mem_hi + 1023) / 1024) : 0;
    m.digest = dg.h;
    return m;
}

static int wants_workload(const char *filter, const char *name) {
    if (!filter || filter[0] == '\0') {
        return 1;
    }
    return strstr(name, filter) != NULL;
}

int main(int argc, char **argv) {
    const char *db_dir = NULL;
    const char *cli_bin = NULL;
    const char *w14_sql_path = NULL;
    const char *filter = NULL;
    int json_mode = 0;

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--json") == 0) {
            json_mode = 1;
        } else if (strcmp(argv[i], "--db-dir") == 0 && i + 1 < argc) {
            db_dir = argv[++i];
        } else if (strcmp(argv[i], "--cli-bin") == 0 && i + 1 < argc) {
            cli_bin = argv[++i];
        } else if (strcmp(argv[i], "--w14-sql") == 0 && i + 1 < argc) {
            w14_sql_path = argv[++i];
        } else if (strcmp(argv[i], "--filter") == 0 && i + 1 < argc) {
            filter = argv[++i];
        } else {
            fprintf(stderr, "Unknown arg: %s\n", argv[i]);
            return 2;
        }
    }

    if (!db_dir) {
        fprintf(stderr, "--db-dir is required\n");
        return 2;
    }
    mkdir(db_dir, 0755);

    WorkloadMetric results[16];
    int n_results = 0;

    int need_core =
        wants_workload(filter, "W01_bulk_insert_prepared") ||
        wants_workload(filter, "W02_create_indexes") ||
        wants_workload(filter, "W03_pk_point_select") ||
        wants_workload(filter, "W04_secondary_idx_range") ||
        wants_workload(filter, "W05_full_scan_filter_agg") ||
        wants_workload(filter, "W06_join_group_order") ||
        wants_workload(filter, "W07_window_functions") ||
        wants_workload(filter, "W08_indexed_update_delete") ||
        wants_workload(filter, "W09_like_text_scan");

    if (need_core) {
        char core_db[1536];
        snprintf(core_db, sizeof(core_db), "%s/core_wal.db", db_dir);
        remove_db_family(core_db);

        sqlite3 *db = NULL;
        int rc = sqlite3_open(core_db, &db);
        if (rc != SQLITE_OK) {
            die_db(db, "open core_wal.db", rc);
        }
        exec_sql(
            db,
            "PRAGMA page_size = 4096;"
            "PRAGMA journal_mode = WAL;"
            "PRAGMA synchronous = NORMAL;"
            "CREATE TABLE t1("
            "  id INTEGER PRIMARY KEY,"
            "  grp INTEGER NOT NULL,"
            "  score INTEGER NOT NULL,"
            "  code TEXT NOT NULL,"
            "  bio TEXT NOT NULL"
            ");",
            "setup core_wal.db"
        );

        /* W01: 10,000 prepared INSERTs */
        {
            Rng rng = rng_for_workload(1);
            char code[32];
            char bio[128];
            double t0 = now_sec();
            exec_sql(db, "BEGIN;", "W01 BEGIN");
            sqlite3_stmt *stmt = prep_sql(
                db,
                "INSERT INTO t1(id, grp, score, code, bio) VALUES (?1, ?2, ?3, ?4, ?5)",
                "W01 prepare"
            );
            for (int64_t id = 1; id <= 10000; id++) {
                int64_t grp = (int64_t)rng_next_range(&rng, 50);
                int64_t score = (int64_t)rng_next_range(&rng, 10000);
                uint64_t code_num = rng_next_range(&rng, 1000);
                uint64_t tok_num = rng_next_range(&rng, 64);
                snprintf(code, sizeof(code), "C-%04" PRIu64, code_num);
                snprintf(
                    bio,
                    sizeof(bio),
                    "entry-%05" PRId64 "-g%02" PRId64 "-token%02" PRIu64 "-payload-%04" PRId64,
                    id, grp, tok_num, score
                );
                sqlite3_bind_int64(stmt, 1, id);
                sqlite3_bind_int64(stmt, 2, grp);
                sqlite3_bind_int64(stmt, 3, score);
                sqlite3_bind_text(stmt, 4, code, -1, SQLITE_TRANSIENT);
                sqlite3_bind_text(stmt, 5, bio, -1, SQLITE_TRANSIENT);
                rc = sqlite3_step(stmt);
                if (rc != SQLITE_DONE) {
                    die_db(db, "W01 step", rc);
                }
                rc = sqlite3_reset(stmt);
                if (rc != SQLITE_OK) {
                    die_db(db, "W01 reset", rc);
                }
            }
            sqlite3_finalize(stmt);
            exec_sql(db, "COMMIT;", "W01 COMMIT");
            double elapsed = now_sec() - t0;

            Digest dg = digest_new();
            sqlite3_stmt *agg = prep_sql(db, "SELECT count(*), sum(grp), sum(score) FROM t1", "W01 agg");
            if (sqlite3_step(agg) == SQLITE_ROW) {
                digest_i64(&dg, sqlite3_column_int64(agg, 0));
                digest_i64(&dg, sqlite3_column_int64(agg, 1));
                digest_i64(&dg, sqlite3_column_int64(agg, 2));
            }
            sqlite3_finalize(agg);

            sqlite3_stmt *smp = prep_sql(
                db,
                "SELECT code, bio FROM t1 WHERE id IN (1, 2500, 5000, 7500, 10000) ORDER BY id",
                "W01 sample"
            );
            while ((rc = sqlite3_step(smp)) == SQLITE_ROW) {
                digest_cstr(&dg, (const char *)sqlite3_column_text(smp, 0));
                digest_cstr(&dg, (const char *)sqlite3_column_text(smp, 1));
            }
            sqlite3_finalize(smp);

            if (wants_workload(filter, "W01_bulk_insert_prepared")) {
                results[n_results++] = make_metric(
                    "W01_bulk_insert_prepared", "disk_wal", 10000, "inserts", elapsed, dg
                );
            }
        }

        /* W02: CREATE INDEX x2 */
        {
            double t0 = now_sec();
            exec_sql(
                db,
                "CREATE INDEX idx_t1_grp_score ON t1(grp, score);"
                "CREATE INDEX idx_t1_code ON t1(code);",
                "W02 create indexes"
            );
            double elapsed = now_sec() - t0;

            Digest dg = digest_new();
            sqlite3_stmt *sch = prep_sql(
                db,
                "SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = 't1' ORDER BY name",
                "W02 schema"
            );
            while (sqlite3_step(sch) == SQLITE_ROW) {
                digest_cstr(&dg, (const char *)sqlite3_column_text(sch, 0));
            }
            sqlite3_finalize(sch);

            sqlite3_stmt *prb = prep_sql(
                db,
                "SELECT id FROM t1 WHERE code = 'C-0042' ORDER BY id",
                "W02 probe"
            );
            int64_t ids[256];
            int n_ids = 0;
            while (sqlite3_step(prb) == SQLITE_ROW && n_ids < 256) {
                ids[n_ids++] = sqlite3_column_int64(prb, 0);
            }
            sqlite3_finalize(prb);
            digest_i64(&dg, (int64_t)n_ids);
            for (int i = 0; i < n_ids; i++) {
                digest_i64(&dg, ids[i]);
            }

            if (wants_workload(filter, "W02_create_indexes")) {
                results[n_results++] = make_metric(
                    "W02_create_indexes", "disk_wal", 2, "indexes", elapsed, dg
                );
            }
        }

        /* W03: 10,000 random PK point selects */
        if (wants_workload(filter, "W03_pk_point_select")) {
            Rng rng = rng_for_workload(3);
            Digest dg = digest_new();
            double t0 = now_sec();
            sqlite3_stmt *stmt = prep_sql(
                db, "SELECT id, grp, score, code FROM t1 WHERE id = ?1", "W03 prep"
            );
            for (uint64_t i = 0; i < 10000; i++) {
                int64_t id = (int64_t)(rng_next_range(&rng, 10000) + 1);
                sqlite3_bind_int64(stmt, 1, id);
                while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
                    digest_i64(&dg, sqlite3_column_int64(stmt, 0));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 1));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 2));
                    digest_cstr(&dg, (const char *)sqlite3_column_text(stmt, 3));
                }
                if (rc != SQLITE_DONE) {
                    die_db(db, "W03 step", rc);
                }
                sqlite3_reset(stmt);
            }
            sqlite3_finalize(stmt);
            double elapsed = now_sec() - t0;
            results[n_results++] = make_metric(
                "W03_pk_point_select", "disk_wal", 10000, "queries", elapsed, dg
            );
        }

        /* W04: 2,000 secondary index range scans */
        if (wants_workload(filter, "W04_secondary_idx_range")) {
            Rng rng = rng_for_workload(4);
            Digest dg = digest_new();
            double t0 = now_sec();
            sqlite3_stmt *stmt = prep_sql(
                db,
                "SELECT count(*), sum(score) FROM t1 WHERE grp = ?1 AND score BETWEEN ?2 AND ?3",
                "W04 prep"
            );
            for (uint64_t i = 0; i < 2000; i++) {
                int64_t grp = (int64_t)rng_next_range(&rng, 50);
                int64_t lo = (int64_t)rng_next_range(&rng, 8500);
                int64_t hi = lo + (int64_t)rng_next_range(&rng, 1500);
                sqlite3_bind_int64(stmt, 1, grp);
                sqlite3_bind_int64(stmt, 2, lo);
                sqlite3_bind_int64(stmt, 3, hi);
                while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
                    digest_i64(&dg, sqlite3_column_int64(stmt, 0));
                    if (sqlite3_column_type(stmt, 1) == SQLITE_NULL) {
                        digest_opt_i64(&dg, 0, 0);
                    } else {
                        digest_opt_i64(&dg, 1, sqlite3_column_int64(stmt, 1));
                    }
                }
                if (rc != SQLITE_DONE) {
                    die_db(db, "W04 step", rc);
                }
                sqlite3_reset(stmt);
            }
            sqlite3_finalize(stmt);
            double elapsed = now_sec() - t0;
            results[n_results++] = make_metric(
                "W04_secondary_idx_range", "disk_wal", 2000, "queries", elapsed, dg
            );
        }

        /* W05: 20 full-table scans + filter + agg */
        if (wants_workload(filter, "W05_full_scan_filter_agg")) {
            Rng rng = rng_for_workload(5);
            Digest dg = digest_new();
            double t0 = now_sec();
            sqlite3_stmt *stmt = prep_sql(
                db,
                "SELECT grp, count(*), sum(score), avg(score) "
                "FROM t1 WHERE score > ?1 GROUP BY grp ORDER BY grp",
                "W05 prep"
            );
            for (uint64_t i = 0; i < 20; i++) {
                int64_t threshold = (int64_t)(rng_next_range(&rng, 7000) + 500);
                sqlite3_bind_int64(stmt, 1, threshold);
                while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
                    digest_i64(&dg, sqlite3_column_int64(stmt, 0));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 1));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 2));
                    digest_f64_scaled(&dg, sqlite3_column_double(stmt, 3));
                }
                if (rc != SQLITE_DONE) {
                    die_db(db, "W05 step", rc);
                }
                sqlite3_reset(stmt);
            }
            sqlite3_finalize(stmt);
            double elapsed = now_sec() - t0;
            results[n_results++] = make_metric(
                "W05_full_scan_filter_agg", "disk_wal", 20, "scans", elapsed, dg
            );
        }

        /* W06: Join t1 (10k) with t2 (1k) + GROUP BY + ORDER BY (10 queries) */
        if (wants_workload(filter, "W06_join_group_order")) {
            exec_sql(
                db,
                "CREATE TABLE t2("
                "  id INTEGER PRIMARY KEY,"
                "  code TEXT NOT NULL,"
                "  category INTEGER NOT NULL,"
                "  weight INTEGER NOT NULL"
                ");"
                "CREATE INDEX idx_t2_code ON t2(code);"
                "BEGIN;",
                "W06 setup DDL"
            );
            sqlite3_stmt *ins = prep_sql(
                db, "INSERT INTO t2(id, code, category, weight) VALUES (?1, ?2, ?3, ?4)", "W06 ins"
            );
            char code[32];
            for (int64_t id = 1; id <= 1000; id++) {
                snprintf(code, sizeof(code), "C-%04" PRId64, id - 1);
                int64_t category = (id - 1) % 20;
                int64_t weight = ((id * 7) % 13) + 1;
                sqlite3_bind_int64(ins, 1, id);
                sqlite3_bind_text(ins, 2, code, -1, SQLITE_TRANSIENT);
                sqlite3_bind_int64(ins, 3, category);
                sqlite3_bind_int64(ins, 4, weight);
                if ((rc = sqlite3_step(ins)) != SQLITE_DONE) {
                    die_db(db, "W06 ins step", rc);
                }
                sqlite3_reset(ins);
            }
            sqlite3_finalize(ins);
            exec_sql(db, "COMMIT;", "W06 setup COMMIT");

            Rng rng = rng_for_workload(6);
            Digest dg = digest_new();
            double t0 = now_sec();
            sqlite3_stmt *stmt = prep_sql(
                db,
                "SELECT t2.category, count(*), sum(t1.score * t2.weight) AS wsum "
                "FROM t2 JOIN t1 ON t1.code = t2.code "
                "WHERE t2.id BETWEEN ?1 AND ?2 "
                "GROUP BY t2.category "
                "ORDER BY wsum DESC, t2.category ASC",
                "W06 prep"
            );
            for (uint64_t q = 0; q < 10; q++) {
                int64_t lo_id = (int64_t)(rng_next_range(&rng, 400) + 1);
                int64_t hi_id = lo_id + 500;
                sqlite3_bind_int64(stmt, 1, lo_id);
                sqlite3_bind_int64(stmt, 2, hi_id);
                while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
                    digest_i64(&dg, sqlite3_column_int64(stmt, 0));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 1));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 2));
                }
                if (rc != SQLITE_DONE) {
                    die_db(db, "W06 step", rc);
                }
                sqlite3_reset(stmt);
            }
            sqlite3_finalize(stmt);
            double elapsed = now_sec() - t0;
            results[n_results++] = make_metric(
                "W06_join_group_order", "disk_wal", 10, "queries", elapsed, dg
            );
        }

        /* W07: Window functions over 5,000 rows (3 passes) */
        if (wants_workload(filter, "W07_window_functions")) {
            Rng rng = rng_for_workload(7);
            Digest dg = digest_new();
            double t0 = now_sec();
            sqlite3_stmt *stmt = prep_sql(
                db,
                "SELECT "
                "  id, "
                "  ROW_NUMBER() OVER (PARTITION BY grp ORDER BY id), "
                "  RANK() OVER (PARTITION BY grp ORDER BY score DESC), "
                "  SUM(score) OVER (PARTITION BY grp ORDER BY id ROWS BETWEEN 3 PRECEDING AND CURRENT ROW) "
                "FROM t1 "
                "WHERE id BETWEEN ?1 AND ?2 "
                "ORDER BY id",
                "W07 prep"
            );
            for (uint64_t q = 0; q < 3; q++) {
                int64_t start_id = (int64_t)(rng_next_range(&rng, 5000) + 1);
                int64_t end_id = start_id + 4999;
                sqlite3_bind_int64(stmt, 1, start_id);
                sqlite3_bind_int64(stmt, 2, end_id);
                while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
                    digest_i64(&dg, sqlite3_column_int64(stmt, 0));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 1));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 2));
                    digest_i64(&dg, sqlite3_column_int64(stmt, 3));
                }
                if (rc != SQLITE_DONE) {
                    die_db(db, "W07 step", rc);
                }
                sqlite3_reset(stmt);
            }
            sqlite3_finalize(stmt);
            double elapsed = now_sec() - t0;
            results[n_results++] = make_metric(
                "W07_window_functions", "disk_wal", 3, "queries", elapsed, dg
            );
        }

        /* W08: 2,000 indexed UPDATEs + 2,000 indexed DELETEs */
        if (wants_workload(filter, "W08_indexed_update_delete") ||
            wants_workload(filter, "W09_like_text_scan")) {
            Rng rng = rng_for_workload(8);
            double t0 = now_sec();
            exec_sql(db, "BEGIN;", "W08 BEGIN upd");
            sqlite3_stmt *upd = prep_sql(
                db, "UPDATE t1 SET score = ?1, grp = ?2 WHERE id = ?3", "W08 prep upd"
            );
            for (uint64_t i = 0; i < 2000; i++) {
                int64_t new_score = (int64_t)rng_next_range(&rng, 10000);
                int64_t new_grp = (int64_t)rng_next_range(&rng, 50);
                int64_t target_id = (int64_t)(rng_next_range(&rng, 10000) + 1);
                sqlite3_bind_int64(upd, 1, new_score);
                sqlite3_bind_int64(upd, 2, new_grp);
                sqlite3_bind_int64(upd, 3, target_id);
                if ((rc = sqlite3_step(upd)) != SQLITE_DONE) {
                    die_db(db, "W08 ustep", rc);
                }
                sqlite3_reset(upd);
            }
            sqlite3_finalize(upd);
            exec_sql(db, "COMMIT;", "W08 COMMIT upd");

            exec_sql(db, "BEGIN;", "W08 BEGIN del");
            sqlite3_stmt *del = prep_sql(db, "DELETE FROM t1 WHERE id = ?1", "W08 prep del");
            for (int64_t i = 1; i <= 2000; i++) {
                int64_t del_id = i * 5;
                sqlite3_bind_int64(del, 1, del_id);
                if ((rc = sqlite3_step(del)) != SQLITE_DONE) {
                    die_db(db, "W08 dstep", rc);
                }
                sqlite3_reset(del);
            }
            sqlite3_finalize(del);
            exec_sql(db, "COMMIT;", "W08 COMMIT del");
            double elapsed = now_sec() - t0;

            Digest dg = digest_new();
            sqlite3_stmt *agg = prep_sql(db, "SELECT count(*), sum(grp), sum(score) FROM t1", "W08 agg");
            if (sqlite3_step(agg) == SQLITE_ROW) {
                digest_i64(&dg, sqlite3_column_int64(agg, 0));
                digest_i64(&dg, sqlite3_column_int64(agg, 1));
                digest_i64(&dg, sqlite3_column_int64(agg, 2));
            }
            sqlite3_finalize(agg);

            if (wants_workload(filter, "W08_indexed_update_delete")) {
                results[n_results++] = make_metric(
                    "W08_indexed_update_delete", "disk_wal", 4000, "dml_ops", elapsed, dg
                );
            }
        }

        /* W09: 20 LIKE '%token42%' scans */
        if (wants_workload(filter, "W09_like_text_scan")) {
            Digest dg = digest_new();
            double t0 = now_sec();
            sqlite3_stmt *stmt = prep_sql(
                db, "SELECT count(*) FROM t1 WHERE bio LIKE '%token42%'", "W09 prep"
            );
            for (uint64_t i = 0; i < 20; i++) {
                while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
                    digest_i64(&dg, sqlite3_column_int64(stmt, 0));
                }
                if (rc != SQLITE_DONE) {
                    die_db(db, "W09 step", rc);
                }
                sqlite3_reset(stmt);
            }
            sqlite3_finalize(stmt);
            double elapsed = now_sec() - t0;
            results[n_results++] = make_metric(
                "W09_like_text_scan", "disk_wal", 20, "scans", elapsed, dg
            );
        }

        sqlite3_close(db);
        remove_db_family(core_db);
    }

    /* W10: JSONB 5,000 inserts + ->> + json_group_array (:memory:) */
    if (wants_workload(filter, "W10_jsonb_extract_agg")) {
        sqlite3 *db = NULL;
        int rc = sqlite3_open(":memory:", &db);
        if (rc != SQLITE_OK) {
            die_db(db, "W10 open", rc);
        }
        exec_sql(
            db,
            "PRAGMA page_size = 4096;"
            "CREATE TABLE jdocs(id INTEGER PRIMARY KEY, bucket INTEGER NOT NULL, payload BLOB NOT NULL);"
            "CREATE INDEX idx_jdocs_bucket ON jdocs(bucket, id);",
            "W10 DDL"
        );

        Rng rng = rng_for_workload(10);
        Digest dg = digest_new();
        char json_buf[256];
        double t0 = now_sec();

        exec_sql(db, "BEGIN;", "W10 BEGIN");
        sqlite3_stmt *ins = prep_sql(
            db, "INSERT INTO jdocs(id, bucket, payload) VALUES (?1, ?2, jsonb(?3))", "W10 ins"
        );
        for (int64_t id = 1; id <= 5000; id++) {
            int64_t bucket = (int64_t)rng_next_range(&rng, 25);
            int64_t val = (int64_t)rng_next_range(&rng, 1000);
            const char *active = ((id + val) % 3 != 0) ? "true" : "false";
            snprintf(
                json_buf,
                sizeof(json_buf),
                "{\"code\":\"J-%04" PRId64 "\",\"active\":%s,\"meta\":{\"val\":%" PRId64 ",\"grp\":%" PRId64 "}}",
                id, active, val, bucket
            );
            sqlite3_bind_int64(ins, 1, id);
            sqlite3_bind_int64(ins, 2, bucket);
            sqlite3_bind_text(ins, 3, json_buf, -1, SQLITE_TRANSIENT);
            if ((rc = sqlite3_step(ins)) != SQLITE_DONE) {
                die_db(db, "W10 istep", rc);
            }
            sqlite3_reset(ins);
        }
        sqlite3_finalize(ins);
        exec_sql(db, "COMMIT;", "W10 COMMIT");

        sqlite3_stmt *agg = prep_sql(
            db,
            "SELECT bucket, count(*), sum(payload ->> '$.meta.val') "
            "FROM jdocs WHERE (payload ->> '$.active') = 1 "
            "GROUP BY bucket ORDER BY bucket",
            "W10 agg"
        );
        while ((rc = sqlite3_step(agg)) == SQLITE_ROW) {
            digest_i64(&dg, sqlite3_column_int64(agg, 0));
            digest_i64(&dg, sqlite3_column_int64(agg, 1));
            digest_i64(&dg, sqlite3_column_int64(agg, 2));
        }
        if (rc != SQLITE_DONE) {
            die_db(db, "W10 agg step", rc);
        }
        sqlite3_finalize(agg);

        sqlite3_stmt *grp = prep_sql(
            db,
            "SELECT json_group_array(code) FROM ("
            "  SELECT payload ->> '$.code' AS code "
            "  FROM jdocs "
            "  WHERE bucket = ?1 AND (payload ->> '$.meta.val') >= ?2 "
            "  ORDER BY id LIMIT 20"
            ")",
            "W10 grp"
        );
        for (int64_t b = 0; b < 25; b++) {
            int64_t min_val = (b * 37) % 600;
            sqlite3_bind_int64(grp, 1, b);
            sqlite3_bind_int64(grp, 2, min_val);
            while ((rc = sqlite3_step(grp)) == SQLITE_ROW) {
                digest_cstr(&dg, (const char *)sqlite3_column_text(grp, 0));
            }
            if (rc != SQLITE_DONE) {
                die_db(db, "W10 grp step", rc);
            }
            sqlite3_reset(grp);
        }
        sqlite3_finalize(grp);

        double elapsed = now_sec() - t0;
        WorkloadMetric w10_m = make_metric(
            "W10_jsonb_extract_agg", "memory", 5026, "ops", elapsed, dg
        );
        sqlite3_close(db);
        results[n_results++] = w10_m;
    }

    /* W11: FTS5 3,000 docs + 200 MATCH queries ordered by bm25() (:memory:) */
    if (wants_workload(filter, "W11_fts5_index_and_bm25")) {
        sqlite3 *db = NULL;
        int rc = sqlite3_open(":memory:", &db);
        if (rc != SQLITE_OK) {
            die_db(db, "W11 open", rc);
        }
        exec_sql(
            db,
            "PRAGMA page_size = 4096;"
            "CREATE VIRTUAL TABLE docs USING fts5(title, body);",
            "W11 DDL"
        );

        Rng rng = rng_for_workload(11);
        Digest dg = digest_new();
        char title[64];
        char body[256];
        char expr[64];
        double t0 = now_sec();

        exec_sql(db, "BEGIN;", "W11 BEGIN");
        sqlite3_stmt *ins = prep_sql(
            db, "INSERT INTO docs(rowid, title, body) VALUES (?1, ?2, ?3)", "W11 ins"
        );
        for (int64_t doc_id = 1; doc_id <= 3000; doc_id++) {
            uint64_t t0_w = rng_next_range(&rng, 80);
            uint64_t t1_w = rng_next_range(&rng, 80);
            uint64_t t2_w = rng_next_range(&rng, 80);
            snprintf(title, sizeof(title), "term%03" PRIu64 " term%03" PRIu64 " term%03" PRIu64,
                     t0_w, t1_w, t2_w);

            size_t body_len = (size_t)(rng_next_range(&rng, 8) + 6);
            size_t pos = 0;
            for (size_t k = 0; k < body_len; k++) {
                uint64_t bw = rng_next_range(&rng, 200);
                pos += (size_t)snprintf(
                    body + pos,
                    sizeof(body) - pos,
                    "%sterm%03" PRIu64,
                    (k > 0) ? " " : "",
                    bw
                );
            }
            sqlite3_bind_int64(ins, 1, doc_id);
            sqlite3_bind_text(ins, 2, title, -1, SQLITE_TRANSIENT);
            sqlite3_bind_text(ins, 3, body, -1, SQLITE_TRANSIENT);
            if ((rc = sqlite3_step(ins)) != SQLITE_DONE) {
                die_db(db, "W11 istep", rc);
            }
            sqlite3_reset(ins);
        }
        sqlite3_finalize(ins);
        exec_sql(db, "COMMIT;", "W11 COMMIT");

        sqlite3_stmt *qstmt = prep_sql(
            db,
            "SELECT rowid, bm25(docs) FROM docs "
            "WHERE docs MATCH ?1 "
            "ORDER BY bm25(docs) ASC, rowid ASC LIMIT 15",
            "W11 query"
        );
        for (uint64_t q = 0; q < 200; q++) {
            uint64_t w1 = rng_next_range(&rng, 200);
            uint64_t w2 = rng_next_range(&rng, 80);
            if (q % 2 == 0) {
                snprintf(expr, sizeof(expr), "term%03" PRIu64, w1);
            } else {
                snprintf(expr, sizeof(expr), "term%03" PRIu64 " OR term%03" PRIu64, w1, w2);
            }
            sqlite3_bind_text(qstmt, 1, expr, -1, SQLITE_TRANSIENT);
            while ((rc = sqlite3_step(qstmt)) == SQLITE_ROW) {
                digest_i64(&dg, sqlite3_column_int64(qstmt, 0));
                digest_f64_scaled(&dg, sqlite3_column_double(qstmt, 1));
            }
            if (rc != SQLITE_DONE) {
                die_db(db, "W11 qstep", rc);
            }
            sqlite3_reset(qstmt);
        }
        sqlite3_finalize(qstmt);

        double elapsed = now_sec() - t0;
        WorkloadMetric w11_m = make_metric(
            "W11_fts5_index_and_bm25", "memory", 3200, "ops", elapsed, dg
        );
        sqlite3_close(db);
        results[n_results++] = w11_m;
    }

    /* W12: R-Tree 5,000 boxes + 500 bbox queries (:memory:) */
    if (wants_workload(filter, "W12_rtree_insert_and_bbox")) {
        sqlite3 *db = NULL;
        int rc = sqlite3_open(":memory:", &db);
        if (rc != SQLITE_OK) {
            die_db(db, "W12 open", rc);
        }
        exec_sql(
            db,
            "PRAGMA page_size = 4096;"
            "CREATE VIRTUAL TABLE boxes USING rtree(id, minX, maxX, minY, maxY);",
            "W12 DDL"
        );

        Rng rng = rng_for_workload(12);
        Digest dg = digest_new();
        double t0 = now_sec();

        exec_sql(db, "BEGIN;", "W12 BEGIN");
        sqlite3_stmt *ins = prep_sql(
            db, "INSERT INTO boxes(id, minX, maxX, minY, maxY) VALUES (?1, ?2, ?3, ?4, ?5)", "W12 ins"
        );
        for (int64_t id = 1; id <= 5000; id++) {
            int64_t x0 = (int64_t)rng_next_range(&rng, 9500);
            int64_t w = (int64_t)(rng_next_range(&rng, 400) + 10);
            int64_t y0 = (int64_t)rng_next_range(&rng, 9500);
            int64_t h = (int64_t)(rng_next_range(&rng, 400) + 10);
            sqlite3_bind_int64(ins, 1, id);
            sqlite3_bind_double(ins, 2, (double)x0);
            sqlite3_bind_double(ins, 3, (double)(x0 + w));
            sqlite3_bind_double(ins, 4, (double)y0);
            sqlite3_bind_double(ins, 5, (double)(y0 + h));
            if ((rc = sqlite3_step(ins)) != SQLITE_DONE) {
                die_db(db, "W12 istep", rc);
            }
            sqlite3_reset(ins);
        }
        sqlite3_finalize(ins);
        exec_sql(db, "COMMIT;", "W12 COMMIT");

        sqlite3_stmt *qstmt = prep_sql(
            db,
            "SELECT count(*), coalesce(sum(id), 0) FROM boxes "
            "WHERE maxX >= ?1 AND minX <= ?2 AND maxY >= ?3 AND minY <= ?4",
            "W12 bbox"
        );
        for (uint64_t q = 0; q < 500; q++) {
            int64_t qx0 = (int64_t)rng_next_range(&rng, 8500);
            int64_t qw = (int64_t)(rng_next_range(&rng, 1200) + 100);
            int64_t qy0 = (int64_t)rng_next_range(&rng, 8500);
            int64_t qh = (int64_t)(rng_next_range(&rng, 1200) + 100);
            sqlite3_bind_double(qstmt, 1, (double)qx0);
            sqlite3_bind_double(qstmt, 2, (double)(qx0 + qw));
            sqlite3_bind_double(qstmt, 3, (double)qy0);
            sqlite3_bind_double(qstmt, 4, (double)(qy0 + qh));
            while ((rc = sqlite3_step(qstmt)) == SQLITE_ROW) {
                digest_i64(&dg, sqlite3_column_int64(qstmt, 0));
                digest_i64(&dg, sqlite3_column_int64(qstmt, 1));
            }
            if (rc != SQLITE_DONE) {
                die_db(db, "W12 qstep", rc);
            }
            sqlite3_reset(qstmt);
        }
        sqlite3_finalize(qstmt);

        double elapsed = now_sec() - t0;
        WorkloadMetric w12_m = make_metric(
            "W12_rtree_insert_and_bbox", "memory", 5500, "ops", elapsed, dg
        );
        sqlite3_close(db);
        results[n_results++] = w12_m;
    }

    /* W13: 300 autocommit WAL transactions */
    if (wants_workload(filter, "W13_autocommit_wal_txns")) {
        char w13_db[1536];
        snprintf(w13_db, sizeof(w13_db), "%s/w13_autocommit.db", db_dir);
        remove_db_family(w13_db);

        sqlite3 *db = NULL;
        int rc = sqlite3_open(w13_db, &db);
        if (rc != SQLITE_OK) {
            die_db(db, "W13 open", rc);
        }
        exec_sql(
            db,
            "PRAGMA page_size = 4096;"
            "PRAGMA journal_mode = WAL;"
            "PRAGMA synchronous = NORMAL;"
            "CREATE TABLE wal_events("
            "  id INTEGER PRIMARY KEY,"
            "  ts INTEGER NOT NULL,"
            "  stream TEXT NOT NULL,"
            "  detail TEXT NOT NULL"
            ");"
            "CREATE INDEX idx_wal_events_stream ON wal_events(stream, ts);",
            "W13 setup"
        );

        Rng rng = rng_for_workload(13);
        char stream[32];
        char detail[64];
        double t0 = now_sec();
        sqlite3_stmt *ins = prep_sql(
            db, "INSERT INTO wal_events(id, ts, stream, detail) VALUES (?1, ?2, ?3, ?4)", "W13 ins"
        );
        for (int64_t id = 1; id <= 300; id++) {
            int64_t ts = id * 10 + (int64_t)rng_next_range(&rng, 9);
            uint64_t s_idx = rng_next_range(&rng, 8);
            snprintf(stream, sizeof(stream), "s%" PRIu64, s_idx);
            snprintf(detail, sizeof(detail), "evt-%04" PRId64 "-ts-%" PRId64, id, ts);
            sqlite3_bind_int64(ins, 1, id);
            sqlite3_bind_int64(ins, 2, ts);
            sqlite3_bind_text(ins, 3, stream, -1, SQLITE_TRANSIENT);
            sqlite3_bind_text(ins, 4, detail, -1, SQLITE_TRANSIENT);
            if ((rc = sqlite3_step(ins)) != SQLITE_DONE) {
                die_db(db, "W13 step", rc);
            }
            sqlite3_reset(ins);
        }
        sqlite3_finalize(ins);
        double elapsed = now_sec() - t0;

        Digest dg = digest_new();
        sqlite3_stmt *agg = prep_sql(db, "SELECT count(*), sum(ts) FROM wal_events", "W13 agg");
        if (sqlite3_step(agg) == SQLITE_ROW) {
            digest_i64(&dg, sqlite3_column_int64(agg, 0));
            digest_i64(&dg, sqlite3_column_int64(agg, 1));
        }
        sqlite3_finalize(agg);

        WorkloadMetric w13_m = make_metric(
            "W13_autocommit_wal_txns", "disk_wal", 300, "txns", elapsed, dg
        );
        sqlite3_close(db);
        remove_db_family(w13_db);
        results[n_results++] = w13_m;
    }

    /* W14: End-to-end CLI SQL batch execution */
    if (wants_workload(filter, "W14_cli_sql_script") && cli_bin && w14_sql_path) {
        char w14_db[1536];
        char w14_out[1536];
        snprintf(w14_db, sizeof(w14_db), "%s/w14_cli.db", db_dir);
        snprintf(w14_out, sizeof(w14_out), "%s/w14_stdout.txt", db_dir);
        remove_db_family(w14_db);

        int fd_in = open(w14_sql_path, O_RDONLY);
        if (fd_in < 0) {
            fprintf(stderr, "W14 open sql failed: %s\n", strerror(errno));
            return 1;
        }
        int fd_out = open(w14_out, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd_out < 0) {
            fprintf(stderr, "W14 open stdout failed: %s\n", strerror(errno));
            close(fd_in);
            return 1;
        }

        double t0 = now_sec();
        pid_t pid = fork();
        if (pid < 0) {
            fprintf(stderr, "W14 fork failed\n");
            return 1;
        }
        if (pid == 0) {
            dup2(fd_in, STDIN_FILENO);
            dup2(fd_out, STDOUT_FILENO);
            close(fd_in);
            close(fd_out);
            char *const child_argv[] = {
                (char *)cli_bin,
                (char *)"-batch",
                (char *)"-list",
                (char *)"-noheader",
                w14_db,
                NULL
            };
            execv(cli_bin, child_argv);
            _exit(127);
        }
        close(fd_in);
        close(fd_out);
        int status = 0;
        struct rusage child_ru;
        memset(&child_ru, 0, sizeof(child_ru));
        if (wait4(pid, &status, 0, &child_ru) < 0 || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            fprintf(stderr, "W14 C CLI failed: status=%d\n", status);
            return 1;
        }
        double elapsed = now_sec() - t0;

        FILE *fp = fopen(w14_out, "rb");
        if (!fp) {
            fprintf(stderr, "W14 read stdout failed\n");
            return 1;
        }
        if (fseek(fp, 0, SEEK_END) != 0) {
            fclose(fp);
            return 1;
        }
        long fsize = ftell(fp);
        if (fseek(fp, 0, SEEK_SET) != 0 || fsize < 0) {
            fclose(fp);
            return 1;
        }
        uint8_t *buf = (uint8_t *)malloc((size_t)fsize + 1);
        size_t nread = fread(buf, 1, (size_t)fsize, fp);
        fclose(fp);

        Digest dg = digest_new();
        digest_bytes(&dg, buf, nread);
        free(buf);

        remove_db_family(w14_db);
        unlink(w14_out);
        WorkloadMetric w14_m = make_metric(
            "W14_cli_sql_script", "disk_wal_cli", 2260, "sql_stmts", elapsed, dg
        );
        if (child_ru.ru_maxrss > 0) {
            w14_m.rss_kib = (uint64_t)child_ru.ru_maxrss;
            w14_m.hwm_kib = (uint64_t)child_ru.ru_maxrss;
        }
        results[n_results++] = w14_m;
    }

    if (json_mode) {
        uint64_t suite_rss_kib = 0;
        uint64_t suite_hwm_kib = 0;
        read_proc_mem_kib(&suite_rss_kib, &suite_hwm_kib);
        printf("{\n");
        printf("  \"engine\": \"c-sqlite-3.54.1-O3\",\n");
        printf("  \"sqlite_version\": \"%s\",\n", sqlite3_libversion());
        printf("  \"suite_rss_kib\": %" PRIu64 ",\n", suite_rss_kib);
        printf("  \"suite_hwm_kib\": %" PRIu64 ",\n", suite_hwm_kib);
        printf("  \"workloads\": [\n");
        for (int i = 0; i < n_results; i++) {
            const WorkloadMetric *m = &results[i];
            printf(
                "    {\"name\": \"%s\", \"storage\": \"%s\", \"ops\": %" PRIu64 ", "
                "\"op_unit\": \"%s\", \"elapsed_ms\": %.6f, \"ops_per_sec\": %.3f, "
                "\"rss_kib\": %" PRIu64 ", \"hwm_kib\": %" PRIu64 ", "
                "\"sqlite_heap_hi_kib\": %" PRIu64 ", "
                "\"digest\": \"%016" PRIx64 "\"}%s\n",
                m->name,
                m->storage,
                m->ops,
                m->op_unit,
                m->elapsed_ms,
                m->ops_per_sec,
                m->rss_kib,
                m->hwm_kib,
                m->sqlite_heap_hi_kib,
                m->digest,
                (i + 1 < n_results) ? "," : ""
            );
        }
        printf("  ]\n}\n");
    } else {
        for (int i = 0; i < n_results; i++) {
            const WorkloadMetric *m = &results[i];
            printf("%-28s %-13s %10" PRIu64 " %12.3f ms %14.1f ops/s %8" PRIu64 " KiB  %016" PRIx64 "\n",
                   m->name, m->storage, m->ops, m->elapsed_ms, m->ops_per_sec, m->rss_kib, m->digest);
        }
    }

    return 0;
}
"""

C_O3_FLAGS = [
    "-O3",
    "-DNDEBUG",
    "-DSQLITE_THREADSAFE=1",
    "-DSQLITE_ENABLE_FTS5",
    "-DSQLITE_ENABLE_RTREE",
    "-DSQLITE_ENABLE_DBSTAT_VTAB",
    "-DSQLITE_ENABLE_MATH_FUNCTIONS",
]


def run_cmd(cmd: list[str], cwd: Path, desc: str) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True)
    if proc.returncode != 0:
        sys.stderr.write(f"ERROR during {desc}:\nCommand: {' '.join(cmd)}\n")
        if proc.stdout:
            sys.stderr.write(f"--- stdout ---\n{proc.stdout}\n")
        if proc.stderr:
            sys.stderr.write(f"--- stderr ---\n{proc.stderr}\n")
        raise SystemExit(1)
    return proc


def detect_cpu_model() -> str:
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.lower().startswith("model name"):
                _, _, val = line.partition(":")
                return val.strip()
    return platform.processor() or "unknown"


def detect_tool_version(cmd: list[str]) -> str:
    try:
        proc = subprocess.run(cmd, text=True, capture_output=True, check=False)
        first = (proc.stdout or proc.stderr).splitlines()
        return first[0].strip() if first else "unknown"
    except OSError:
        return "unknown"


def ensure_rust_binaries(repo_root: Path, skip_build: bool = False) -> tuple[Path, Path]:
    rust_bench = repo_root / "target" / "release" / "examples" / "speedbench"
    rust_cli = repo_root / "target" / "release" / "sqliters"
    pgo_script = repo_root / "tools" / "build_pgo.py"
    if skip_build:
        sys.stderr.write("[1/4] Skipping Rust build (--skip-rust-build); using existing target/release/ binaries...\n")
    elif pgo_script.exists():
        sys.stderr.write("[1/4] Ensuring PGO-optimized Rust binaries via tools/build_pgo.py --reuse-profdata...\n")
        run_cmd(
            [sys.executable, str(pgo_script), "--reuse-profdata"],
            cwd=repo_root,
            desc="tools/build_pgo.py --reuse-profdata",
        )
    else:
        sys.stderr.write("[1/4] Building Rust sqliters (--release: example speedbench + sqliters CLI)...\n")
        run_cmd(
            ["cargo", "build", "--release", "-p", "sqliters", "--example", "speedbench"],
            cwd=repo_root,
            desc="cargo build --release --example speedbench",
        )
        run_cmd(
            ["cargo", "build", "--release", "-p", "sqliters-cli", "--bin", "sqliters"],
            cwd=repo_root,
            desc="cargo build --release --bin sqliters",
        )
    if not rust_bench.exists() or not rust_cli.exists():
        raise SystemExit("Missing compiled Rust release binaries under target/release/")
    return rust_bench, rust_cli


def ensure_c_binaries(repo_root: Path) -> tuple[Path, Path]:
    ref_dir = repo_root / "target" / "sqlite-ref"
    amalg_dir = ref_dir / "amalg"
    sqlite3_c = amalg_dir / "sqlite3.c"
    sqlite3_h = amalg_dir / "sqlite3.h"
    shell_c = amalg_dir / "shell.c"
    for p in (sqlite3_c, sqlite3_h, shell_c):
        if not p.exists():
            raise SystemExit(f"Required amalgamation file missing: {p}")

    cc = os.environ.get("CC", "cc")
    sqlite3_obj = ref_dir / "sqlite3_o3_bench.o"
    sqlite3_cli = ref_dir / "sqlite3_o3"
    c_bench_src = ref_dir / "c_speedbench.c"
    c_bench_bin = ref_dir / "c_speedbench"

    sys.stderr.write("[2/4] Building C SQLite 3.54.1 (-O3) reference binaries in target/sqlite-ref/...\n")
    if not sqlite3_obj.exists() or sqlite3_obj.stat().st_mtime < sqlite3_c.stat().st_mtime:
        run_cmd(
            [cc, *C_O3_FLAGS, f"-I{amalg_dir}", "-c", str(sqlite3_c), "-o", str(sqlite3_obj)],
            cwd=ref_dir,
            desc="compile sqlite3_o3_bench.o",
        )

    if not sqlite3_cli.exists() or sqlite3_cli.stat().st_mtime < sqlite3_obj.stat().st_mtime:
        run_cmd(
            [
                cc,
                *C_O3_FLAGS,
                f"-I{amalg_dir}",
                str(shell_c),
                str(sqlite3_obj),
                "-lpthread",
                "-ldl",
                "-lm",
                "-o",
                str(sqlite3_cli),
            ],
            cwd=ref_dir,
            desc="compile sqlite3_o3 CLI",
        )

    existing_src = c_bench_src.read_text(encoding="utf-8") if c_bench_src.exists() else ""
    if existing_src != C_SPEEDBENCH_SOURCE:
        c_bench_src.write_text(C_SPEEDBENCH_SOURCE, encoding="utf-8")

    if (
        not c_bench_bin.exists()
        or c_bench_bin.stat().st_mtime < c_bench_src.stat().st_mtime
        or c_bench_bin.stat().st_mtime < sqlite3_obj.stat().st_mtime
    ):
        run_cmd(
            [
                cc,
                *C_O3_FLAGS,
                "-Wall",
                "-Wextra",
                f"-I{amalg_dir}",
                str(c_bench_src),
                str(sqlite3_obj),
                "-lpthread",
                "-ldl",
                "-lm",
                "-o",
                str(c_bench_bin),
            ],
            cwd=ref_dir,
            desc="compile c_speedbench",
        )

    ver_proc = run_cmd([str(sqlite3_cli), "--version"], cwd=ref_dir, desc="verify sqlite3_o3 version")
    if not ver_proc.stdout.strip().startswith("3.54.1"):
        raise SystemExit(f"Unexpected C SQLite version: {ver_proc.stdout.strip()}")

    return c_bench_bin, sqlite3_cli


def measure_binary_sections(bin_path: Path) -> dict[str, int]:
    file_bytes = bin_path.stat().st_size if bin_path.exists() else 0
    text_bytes = 0
    rodata_bytes = 0
    eh_bytes = 0
    try:
        proc = subprocess.run(["size", "-A", str(bin_path)], text=True, capture_output=True, check=False)
        for line in proc.stdout.splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[1].isdigit():
                sec, sz = parts[0], int(parts[1])
                if sec == ".text":
                    text_bytes = sz
                elif sec == ".rodata":
                    rodata_bytes = sz
                elif sec in (".eh_frame", ".eh_frame_hdr", ".gcc_except_table"):
                    eh_bytes += sz
    except OSError:
        pass
    return {
        "file_bytes": file_bytes,
        "file_kib": round(file_bytes / 1024.0, 1),
        "text_bytes": text_bytes,
        "text_kib": round(text_bytes / 1024.0, 1),
        "rodata_bytes": rodata_bytes,
        "unwind_tables_bytes": eh_bytes,
    }


RSS_PROBE_SOURCE = r"""
#define _DEFAULT_SOURCE
#include <stdio.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    pid_t pid = fork();
    if (pid < 0) return 2;
    if (pid == 0) { execv(argv[1], &argv[1]); _exit(127); }
    int st = 0; struct rusage ru = {0};
    if (wait4(pid, &st, 0, &ru) < 0) return 2;
    fprintf(stderr, "\nSQLITERS_PEAK_RSS_KIB=%ld\n", ru.ru_maxrss);
    return WIFEXITED(st) ? WEXITSTATUS(st) : 1;
}
"""


def ensure_rss_probe(repo_root: Path) -> Path:
    ref_dir = repo_root / "target" / "sqlite-ref"
    ref_dir.mkdir(parents=True, exist_ok=True)
    src = ref_dir / "rss_probe.c"
    bin_p = ref_dir / "rss_probe"
    if not src.exists() or src.read_text(encoding="utf-8") != RSS_PROBE_SOURCE:
        src.write_text(RSS_PROBE_SOURCE, encoding="utf-8")
    if not bin_p.exists() or bin_p.stat().st_mtime < src.stat().st_mtime:
        cc = os.environ.get("CC", "cc")
        run_cmd([cc, "-O2", str(src), "-o", str(bin_p)], cwd=ref_dir, desc="compile rss_probe")
    return bin_p


def run_wait4_maxrss_kib(
    cmd: list[str], cwd: Path, stdin_path: Path | None = None, stdout_path: Path | None = None
) -> tuple[str, int]:
    probe = ensure_rss_probe(Path(__file__).resolve().parent.parent)
    stdin_fh = stdin_path.open("rb") if stdin_path else None
    stdout_fh = stdout_path.open("wb") if stdout_path else None
    try:
        proc = subprocess.Popen(
            [str(probe), *cmd],
            cwd=cwd,
            stdin=stdin_fh if stdin_fh else subprocess.DEVNULL,
            stdout=stdout_fh if stdout_fh else subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=(stdout_fh is None),
        )
        out_str = ""
        if stdout_fh is None and proc.stdout is not None:
            out_str = proc.stdout.read()
        err_raw = proc.stderr.read() if proc.stderr is not None else ""
        err_str = err_raw.decode("utf-8", "replace") if isinstance(err_raw, bytes) else err_raw
        exit_code = proc.wait()
        maxrss_kib = 0
        cleaned_err: list[str] = []
        for line in err_str.splitlines():
            if line.startswith("SQLITERS_PEAK_RSS_KIB="):
                maxrss_kib = int(line.split("=", 1)[1].strip())
            elif line.strip():
                cleaned_err.append(line)
        if exit_code != 0:
            sys.stderr.write(f"ERROR in {' '.join(cmd)} (exit {exit_code}):\n" + "\n".join(cleaned_err) + "\n")
            raise SystemExit(1)
        return out_str, maxrss_kib
    finally:
        if stdin_fh:
            stdin_fh.close()
        if stdout_fh:
            stdout_fh.close()


def run_single_trial(
    cmd: list[str], cwd: Path, label: str
) -> dict[str, Any]:
    out_str, maxrss_kib = run_wait4_maxrss_kib(cmd, cwd=cwd)
    try:
        payload = json.loads(out_str)
        payload["wait4_maxrss_kib"] = maxrss_kib
        return payload
    except json.JSONDecodeError as exc:
        sys.stderr.write(f"Invalid JSON from {label}:\n{out_str}\n")
        raise SystemExit(1) from exc


def geometric_mean(values: list[float]) -> float:
    if not values:
        return 0.0
    return math.exp(sum(math.log(v) for v in values) / len(values))


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run deterministic baseline benchmark comparing Rust sqliters vs C SQLite 3.54.1 (-O3)."
    )
    parser.add_argument("--warmup", type=int, default=1, help="Number of warmup iterations (default: 1)")
    parser.add_argument("--trials", type=int, default=3, help="Number of timed iterations (default: 3)")
    parser.add_argument("--filter", type=str, default="", help="Optional substring filter on workload name")
    parser.add_argument(
        "--skip-rust-build",
        action="store_true",
        help="Use existing target/release/{examples/speedbench,sqliters} binaries without invoking cargo",
    )
    parser.add_argument(
        "--output-json",
        type=Path,
        default=None,
        help="Path to write JSON report (default: target/sqlite-ref/baseline_main_bench.json)",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    ref_dir = repo_root / "target" / "sqlite-ref"
    ref_dir.mkdir(parents=True, exist_ok=True)
    out_json = args.output_json or (ref_dir / "baseline_main_bench.json")

    rust_bench, rust_cli = ensure_rust_binaries(repo_root, skip_build=args.skip_rust_build)
    c_bench, c_cli = ensure_c_binaries(repo_root)

    bench_scratch_parent = ref_dir / "bench_runs"
    if bench_scratch_parent.exists():
        shutil.rmtree(bench_scratch_parent)
    bench_scratch_parent.mkdir(parents=True, exist_ok=True)

    w14_sql_file = bench_scratch_parent / "w14_shared_batch.sql"
    run_cmd(
        [str(rust_bench), "--dump-w14-sql", str(w14_sql_file)],
        cwd=repo_root,
        desc="generate deterministic W14 SQL batch script",
    )

    total_iters = args.warmup + args.trials
    rust_timed_runs: list[dict[str, Any]] = []
    c_timed_runs: list[dict[str, Any]] = []

    sys.stderr.write(
        f"[3/4] Running {args.warmup} warmup + {args.trials} interleaved timed trials...\n"
    )
    wall_t0 = time.perf_counter()

    for idx in range(total_iters):
        is_warmup = idx < args.warmup
        phase_label = f"warmup #{idx + 1}" if is_warmup else f"trial #{idx - args.warmup + 1}"

        with tempfile.TemporaryDirectory(dir=bench_scratch_parent, prefix="rust_") as r_dir:
            r_cmd = [
                str(rust_bench),
                "--json",
                "--db-dir",
                r_dir,
                "--cli-bin",
                str(rust_cli),
            ]
            if args.filter:
                r_cmd.extend(["--filter", args.filter])
            r_payload = run_single_trial(r_cmd, cwd=repo_root, label=f"Rust {phase_label}")

        with tempfile.TemporaryDirectory(dir=bench_scratch_parent, prefix="c_") as c_dir:
            c_cmd = [
                str(c_bench),
                "--json",
                "--db-dir",
                c_dir,
                "--cli-bin",
                str(c_cli),
                "--w14-sql",
                str(w14_sql_file),
            ]
            if args.filter:
                c_cmd.extend(["--filter", args.filter])
            c_payload = run_single_trial(c_cmd, cwd=repo_root, label=f"C {phase_label}")

        # Verify 100% workload list & output digest parity on EVERY run (warmup + timed)!
        r_wlist = r_payload["workloads"]
        c_wlist = c_payload["workloads"]
        if len(r_wlist) != len(c_wlist):
            raise SystemExit(
                f"Workload count mismatch on {phase_label}: Rust={len(r_wlist)} vs C={len(c_wlist)}"
            )
        for rw, cw in zip(r_wlist, c_wlist):
            if rw["name"] != cw["name"]:
                raise SystemExit(f"Workload ordering mismatch: {rw['name']} vs {cw['name']}")
            if rw["digest"] != cw["digest"]:
                raise SystemExit(
                    f"CRITICAL DIGEST MISMATCH in {rw['name']} ({phase_label}): "
                    f"Rust={rw['digest']} vs C={cw['digest']}"
                )

        r_sum_ms = sum(w["elapsed_ms"] for w in r_wlist)
        c_sum_ms = sum(w["elapsed_ms"] for w in c_wlist)
        sys.stderr.write(
            f"  - {phase_label:<10}: Rust={r_sum_ms:8.2f} ms | C(-O3)={c_sum_ms:8.2f} ms | digests=OK ({len(r_wlist)}/{len(c_wlist)})\n"
        )

        if not is_warmup:
            rust_timed_runs.append(r_payload)
            c_timed_runs.append(c_payload)

    # Measure standalone Rust CLI child Peak RSS on W14 (matching C's wait4 child measurement)
    rust_w14_cli_peak_rss_kib = 0
    if not args.filter or "W14_cli_sql_script".find(args.filter) >= 0:
        with tempfile.TemporaryDirectory(dir=bench_scratch_parent, prefix="w14_rss_") as w14_dir:
            w14_db = Path(w14_dir) / "w14_cli.db"
            w14_out = Path(w14_dir) / "w14_stdout.txt"
            _, rust_w14_cli_peak_rss_kib = run_wait4_maxrss_kib(
                [str(rust_cli), "-list", "-noheader", str(w14_db)],
                cwd=repo_root,
                stdin_path=w14_sql_file,
                stdout_path=w14_out,
            )

    wall_elapsed_s = time.perf_counter() - wall_t0
    shutil.rmtree(bench_scratch_parent, ignore_errors=True)

    workload_names = [w["name"] for w in rust_timed_runs[0]["workloads"]]
    summary_rows: list[dict[str, Any]] = []
    ratios: list[float] = []
    disk_ratios: list[float] = []
    mem_ratios: list[float] = []

    for w_idx, w_name in enumerate(workload_names):
        r_samples = [run["workloads"][w_idx]["elapsed_ms"] for run in rust_timed_runs]
        c_samples = [run["workloads"][w_idx]["elapsed_ms"] for run in c_timed_runs]
        r_rss_samples = [int(run["workloads"][w_idx].get("rss_kib", 0)) for run in rust_timed_runs]
        r_hwm_samples = [int(run["workloads"][w_idx].get("hwm_kib", 0)) for run in rust_timed_runs]
        c_rss_samples = [int(run["workloads"][w_idx].get("rss_kib", 0)) for run in c_timed_runs]
        c_hwm_samples = [int(run["workloads"][w_idx].get("hwm_kib", 0)) for run in c_timed_runs]
        c_heap_samples = [int(run["workloads"][w_idx].get("sqlite_heap_hi_kib", 0)) for run in c_timed_runs]
        meta = rust_timed_runs[0]["workloads"][w_idx]
        ops = int(meta["ops"])
        op_unit = meta["op_unit"]
        storage = meta["storage"]
        digest = meta["digest"]

        # Confirm stability of digest across all timed iterations as well
        for run in rust_timed_runs[1:]:
            if run["workloads"][w_idx]["digest"] != digest:
                raise SystemExit(f"Non-deterministic Rust digest across trials in {w_name}")

        r_med_ms = statistics.median(r_samples)
        c_med_ms = statistics.median(c_samples)
        r_rss_kib = (
            rust_w14_cli_peak_rss_kib
            if w_name == "W14_cli_sql_script" and rust_w14_cli_peak_rss_kib > 0
            else int(statistics.median(r_rss_samples))
        )
        r_hwm_kib = (
            rust_w14_cli_peak_rss_kib
            if w_name == "W14_cli_sql_script" and rust_w14_cli_peak_rss_kib > 0
            else int(statistics.median(r_hwm_samples))
        )
        c_rss_kib = int(statistics.median(c_rss_samples))
        c_hwm_kib = int(statistics.median(c_hwm_samples))
        c_heap_kib = int(statistics.median(c_heap_samples))
        r_ops_sec = (ops / (r_med_ms / 1000.0)) if r_med_ms > 0.0 else 0.0
        c_ops_sec = (ops / (c_med_ms / 1000.0)) if c_med_ms > 0.0 else 0.0
        ratio = (r_med_ms / c_med_ms) if c_med_ms > 0.0 else 0.0

        ratios.append(ratio)
        if storage == "memory":
            mem_ratios.append(ratio)
        else:
            disk_ratios.append(ratio)

        summary_rows.append(
            {
                "name": w_name,
                "storage": storage,
                "ops": ops,
                "op_unit": op_unit,
                "rust_median_ms": round(r_med_ms, 3),
                "rust_trials_ms": [round(x, 3) for x in r_samples],
                "rust_ops_per_sec": round(r_ops_sec, 1),
                "rust_rss_kib": r_rss_kib,
                "rust_hwm_kib": r_hwm_kib,
                "c_median_ms": round(c_med_ms, 3),
                "c_trials_ms": [round(x, 3) for x in c_samples],
                "c_ops_per_sec": round(c_ops_sec, 1),
                "c_rss_kib": c_rss_kib,
                "c_hwm_kib": c_hwm_kib,
                "c_sqlite_heap_peak_kib": c_heap_kib,
                "rust_over_c_ratio": round(ratio, 2),
                "digest_fnv1a64": digest,
                "digest_verified": True,
            }
        )

    geomean_all = geometric_mean(ratios)
    geomean_disk = geometric_mean(disk_ratios)
    geomean_mem = geometric_mean(mem_ratios)
    total_rust_med_ms = sum(r["rust_median_ms"] for r in summary_rows)
    total_c_med_ms = sum(r["c_median_ms"] for r in summary_rows)
    total_wall_ratio = (total_rust_med_ms / total_c_med_ms) if total_c_med_ms > 0.0 else 0.0
    rust_suite_peak_rss_kib = int(
        statistics.median(int(run.get("wait4_maxrss_kib", 0)) for run in rust_timed_runs)
    )
    c_suite_peak_rss_kib = int(
        statistics.median(int(run.get("wait4_maxrss_kib", 0)) for run in c_timed_runs)
    )

    hotspots_sorted = sorted(summary_rows, key=lambda r: r["rust_over_c_ratio"], reverse=True)

    git_head = detect_tool_version(["git", "rev-parse", "--short", "HEAD"])
    git_branch = detect_tool_version(["git", "rev-parse", "--abbrev-ref", "HEAD"])
    rustc_ver = detect_tool_version(["rustc", "--version"])
    cc_bin = os.environ.get("CC", "cc")
    cc_ver = detect_tool_version([cc_bin, "--version"])
    rust_bin_info = measure_binary_sections(rust_cli)
    c_bin_info = measure_binary_sections(c_cli)

    report_doc = {
        "benchmark": "sqliters_speedbench",
        "git_branch": git_branch,
        "git_commit": git_head,
        "sqlite_target_version": "3.54.1",
        "binaries": {
            "rust_cli": rust_bin_info,
            "c_cli_o3": c_bin_info,
        },
        "environment": {
            "os": f"{platform.system()} {platform.release()} ({platform.machine()})",
            "cpu": detect_cpu_model(),
            "logical_cpus": os.cpu_count() or 1,
            "rustc": rustc_ver,
            "rust_profile": (
                "release+pgo+lld-relr-split"
                if (repo_root / "tools" / "build_pgo.py").exists()
                else "release"
            ),
            "c_compiler": cc_ver,
            "c_flags": C_O3_FLAGS,
        },
        "methodology": {
            "prng_seed_base": "0x53514C4954453335",
            "page_size": 4096,
            "disk_journal_mode": "WAL",
            "disk_synchronous": "NORMAL",
            "warmup_iterations": args.warmup,
            "timed_iterations": args.trials,
            "interleaved_trials": True,
            "wall_clock_total_sec": round(wall_elapsed_s, 2),
            "all_digests_matched": True,
        },
        "summary": {
            "workload_count": len(summary_rows),
            "geometric_mean_rust_over_c": round(geomean_all, 3),
            "geometric_mean_disk_wal_rust_over_c": round(geomean_disk, 3),
            "geometric_mean_memory_rust_over_c": round(geomean_mem, 3),
            "sum_median_rust_ms": round(total_rust_med_ms, 3),
            "sum_median_c_ms": round(total_c_med_ms, 3),
            "sum_median_rust_over_c": round(total_wall_ratio, 3),
            "rust_suite_peak_rss_kib": rust_suite_peak_rss_kib,
            "c_suite_peak_rss_kib": c_suite_peak_rss_kib,
            "top_slowdown_workloads": [
                {
                    "name": h["name"],
                    "rust_over_c_ratio": h["rust_over_c_ratio"],
                    "rust_median_ms": h["rust_median_ms"],
                    "c_median_ms": h["c_median_ms"],
                }
                for h in hotspots_sorted[:5]
            ],
        },
        "workloads": summary_rows,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(report_doc, indent=2) + "\n", encoding="utf-8")
    sys.stderr.write(f"[4/4] Wrote structured results to {out_json}\n\n")

    # Print clean Markdown report to stdout
    print(f"## Speedbench Comparison (`{git_branch}` @ `{git_head}` vs C SQLite 3.54.1 `-O3`)\n")
    print(
        f"- **Hardware / OS**: {report_doc['environment']['cpu']} "
        f"({report_doc['environment']['logical_cpus']} threads), {report_doc['environment']['os']}"
    )
    print(
        f"- **Rust CLI Binary**: `{rust_bin_info['file_kib']:.1f} KiB` total (`{rust_bin_info['text_kib']:.1f} KiB` `.text`) | "
        f"**C `-O3` CLI Binary**: `{c_bin_info['file_kib']:.1f} KiB` total (`{c_bin_info['text_kib']:.1f} KiB` `.text`)"
    )
    print(
        f"- **Suite Peak RSS (`wait4 ru_maxrss`)**: Rust **`{rust_suite_peak_rss_kib:,} KiB`** (`{rust_suite_peak_rss_kib / 1024.0:.2f} MiB`) | "
        f"C `-O3` **`{c_suite_peak_rss_kib:,} KiB`** (`{c_suite_peak_rss_kib / 1024.0:.2f} MiB`)\n"
    )

    print(
        "| Workload | Storage | Ops | Rust (ms) | C `-O3` (ms) | Ratio | Rust RSS (KiB) | C RSS (KiB) | C Heap Peak (KiB) | Digest (`FNV-1a`) |"
    )
    print("| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |")
    for r in summary_rows:
        print(
            f"| `{r['name']}` | `{r['storage']}` | {r['ops']:,} | "
            f"{r['rust_median_ms']:.2f} | {r['c_median_ms']:.2f} | "
            f"**{r['rust_over_c_ratio']:.2f}x** | "
            f"{r['rust_rss_kib']:,} | {r['c_rss_kib']:,} | {r['c_sqlite_heap_peak_kib']:,} | "
            f"`{r['digest_fnv1a64']}` ✓ |"
        )

    print("\n### Aggregate Summary\n")
    print(f"- **Geometric Mean (`Rust_ms / C_ms`, all {len(summary_rows)} workloads)**: **{geomean_all:.2f}x**")
    print(f"- **Geometric Mean (On-Disk WAL workloads)**: **{geomean_disk:.2f}x**")
    print(f"- **Geometric Mean (`:memory:` Extension workloads)**: **{geomean_mem:.2f}x**")
    print(
        f"- **Total Median Suite Time**: Rust **{total_rust_med_ms:.2f} ms** vs C **{total_c_med_ms:.2f} ms** "
        f"(aggregate wall-time ratio **{total_wall_ratio:.2f}x**)"
    )
    print(
        f"- **Checksum Verification**: **{len(summary_rows)}/{len(summary_rows)} workloads matched 100%** "
        f"across all {(args.warmup + args.trials)} runs ({(args.warmup + args.trials) * len(summary_rows)} verified workload executions)"
    )

    print("\n### Top Slowdown Hotspots (Ranked by `Rust_ms / C_ms`)\n")
    for rank, h in enumerate(hotspots_sorted[:5], start=1):
        print(
            f"{rank}. **`{h['name']}`** — **{h['rust_over_c_ratio']:.2f}x** "
            f"(`{h['rust_median_ms']:.2f} ms` vs `{h['c_median_ms']:.2f} ms`)"
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
