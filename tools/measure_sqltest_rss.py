#!/usr/bin/env python3
"""
Read-only external helper that collects `./sqltest` 126-sample CLI command-set
timings from `sqltest/artifacts/external/runs/<run>/status.json` AND measures
exact child CLI process Peak RSS (`os.wait4` `ru_maxrss` in KiB) in `/tmp`
for both pinned C `sqlite3` and candidate `sqliters` without modifying any file
inside `sqltest/`.
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


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
        subprocess.run([cc, "-O2", str(src), "-o", str(bin_p)], check=True)
    return bin_p


def wait4_cli_rss_kib(binary: Path, db_path: Path, payload: bytes, cwd: Path) -> int:
    probe = ensure_rss_probe(Path(__file__).resolve().parent.parent)
    proc = subprocess.Popen(
        [str(probe), str(binary), str(db_path)],
        cwd=cwd,
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    assert proc.stdin is not None
    proc.stdin.write(payload)
    proc.stdin.close()
    err = (proc.stderr.read() if proc.stderr is not None else b"").decode("utf-8", "replace")
    code = proc.wait()
    maxrss_kib = 0
    real_err: list[str] = []
    for line in err.splitlines():
        if line.startswith("SQLITERS_PEAK_RSS_KIB="):
            maxrss_kib = int(line.split("=", 1)[1].strip())
        elif line.strip():
            real_err.append(line)
    if code != 0 or real_err:
        raise RuntimeError(
            f"CLI RSS probe failed ({binary} -> exit {code}): {' | '.join(real_err)}"
        )
    return maxrss_kib


def measure_7_cli_families_rss_kib(binary: Path, sqltest_root: Path, seed: int = 20261120) -> dict[str, int]:
    manifest = json.loads((sqltest_root / "external-input-manifest.json").read_text(encoding="utf-8"))
    data_dir = sqltest_root / "artifacts" / "external" / "inputs"
    full_dir = sqltest_root / manifest["datasets"][f"{seed}-full"]["folder"]
    single_dir = sqltest_root / manifest["datasets"][f"{seed}-single"]["folder"]
    uuid_dir = sqltest_root / manifest["datasets"][f"uuid7-{seed}-full"]["folder"]

    out: dict[str, int] = {}
    with tempfile.TemporaryDirectory(prefix="sqltest_rss_") as tmp:
        tmp_path = Path(tmp)

        for case_key, ds_dir, driver_name in [
            ("ingest/cli-import", full_dir, "import.txt"),
            ("ingest/cli-read-auto", single_dir, "read-auto.txt"),
            ("ingest/cli-read-batch", full_dir, "read-batch.txt"),
        ]:
            db = tmp_path / f"{case_key.replace('/', '_')}.db"
            rel_driver = str((ds_dir / driver_name).relative_to(sqltest_root))
            payload = (".read " + json.dumps(rel_driver) + "\n").encode("utf-8")
            out[case_key] = wait4_cli_rss_kib(binary, db, payload, cwd=sqltest_root)

        for mode_suffix, ds_dir in [("auto", single_dir), ("batch", full_dir)]:
            case_key = f"ingest/cli-persistent-{mode_suffix}"
            db = tmp_path / f"persistent_{mode_suffix}.db"
            payload = (
                (ds_dir / "persistent-start.txt").read_bytes()
                + (ds_dir / f"persistent-{mode_suffix}.txt").read_bytes()
                + (data_dir / "quit.txt").read_bytes()
            )
            out[case_key] = wait4_cli_rss_kib(binary, db, payload, cwd=sqltest_root)

        uuid_db = tmp_path / "uuid_loaded.db"
        rel_load = str((uuid_dir / "load-cli.txt").relative_to(sqltest_root))
        wait4_cli_rss_kib(
            binary,
            uuid_db,
            (".read " + json.dumps(rel_load) + "\n").encode("utf-8"),
            cwd=sqltest_root,
        )
        for case_key, leaf in [
            ("uuid-query/join-index", "join-cli-index.txt"),
            ("uuid-query/join-noindex", "join-cli-noindex.txt"),
        ]:
            out[case_key] = wait4_cli_rss_kib(
                binary, uuid_db, (uuid_dir / leaf).read_bytes(), cwd=sqltest_root
            )

    return out


def summarize_sqltest_run(
    status_json: Path,
    cand_rss_by_family: dict[str, int],
    c_rss_by_family: dict[str, int],
) -> dict[str, Any]:
    st = json.loads(status_json.read_text(encoding="utf-8"))
    by_family: dict[str, dict[str, list[float]]] = {}
    for s in st.get("samples", []):
        if s.get("status") != "PASS":
            continue
        fam_case = f"{s['family']}/{s['case']}"
        entry = by_family.setdefault(fam_case, {"cand_ms": [], "c_ms": []})
        entry["cand_ms"].append(float(s["ns"]) / 1e6)
        entry["c_ms"].append(float(s["reference_ns"]) / 1e6)

    families_out: list[dict[str, Any]] = []
    ratios: list[float] = []
    for fam_case in sorted(by_family.keys()):
        cand_vals = sorted(by_family[fam_case]["cand_ms"])
        c_vals = sorted(by_family[fam_case]["c_ms"])
        cand_med = statistics.median(cand_vals)
        c_med = statistics.median(c_vals)
        p95_idx = min(len(cand_vals) - 1, int(round(0.95 * (len(cand_vals) - 1))))
        c_p95_idx = min(len(c_vals) - 1, int(round(0.95 * (len(c_vals) - 1))))
        ratio = (cand_med / c_med) if c_med > 0 else 0.0
        ratios.append(ratio)
        families_out.append(
            {
                "family_case": fam_case,
                "samples": len(cand_vals),
                "candidate_median_ms": round(cand_med, 3),
                "candidate_p95_ms": round(cand_vals[p95_idx], 3),
                "c_reference_median_ms": round(c_med, 3),
                "c_reference_p95_ms": round(c_vals[c_p95_idx], 3),
                "candidate_over_c_ratio": round(ratio, 3),
                "candidate_peak_rss_kib": cand_rss_by_family.get(fam_case, 0),
                "c_peak_rss_kib": c_rss_by_family.get(fam_case, 0),
            }
        )

    import math

    geomean = math.exp(sum(math.log(r) for r in ratios) / len(ratios)) if ratios else 0.0
    return {
        "status": st.get("status"),
        "samples_total": len(st.get("samples", [])),
        "geometric_mean_candidate_over_c": round(geomean, 3),
        "families": families_out,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--sqltest-run-status", type=Path, required=True)
    ap.add_argument("--candidate-bin", type=Path, required=True)
    ap.add_argument("--output-json", type=Path, required=True)
    args = ap.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    sqltest_root = repo_root / "sqltest"
    if not sqltest_root.exists():
        sys.stderr.write("sqltest directory not present; skipping.\n")
        return 0

    c_cli = sqltest_root / "artifacts" / "external" / "build" / "max-2000" / "sqlite3"
    c_rss = measure_7_cli_families_rss_kib(c_cli, sqltest_root)
    cand_rss = measure_7_cli_families_rss_kib(args.candidate_bin.resolve(), sqltest_root)
    summary = summarize_sqltest_run(args.sqltest_run_status.resolve(), cand_rss, c_rss)

    args.output_json.parent.mkdir(parents=True, exist_ok=True)
    args.output_json.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
