#!/usr/bin/env python3
"""
Renders `/home/hakanbiris/github/sqliters/benchmarks/benchmark_report.md` from all
`benchmarks/*.json` stage snapshots (`00_main_baseline.json`, `01_performans_p1a.json`, ...),
putting C SQLite 3.54.1, `main`, and every optimization stage side-by-side for:
  1. `./sqltest` 126-sample CLI command-set timings (ms) & Peak RSS (KiB)
  2. `speedbench` W01..W14 timings (ms) & RSS / C SQLite internal heap peak (KiB)
  3. Binary file sizes & `.text` section sizes
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


STAGE_LABELS = {
    "00_main_baseline": "main (d70eae4)",
    "01_performans_p1a": "P1-A",
    "02_performans_p1b": "P1-B",
    "03_performans_p1c": "P1-C",
    "04_performans_p2": "P2 (SIMD/ASM)",
    "05_performans_1": "performans-1",
    "06_performans_2": "performans-2",
}


def load_stages(bench_dir: Path) -> list[tuple[str, str, dict[str, Any]]]:
    items: list[tuple[str, str, dict[str, Any]]] = []
    for p in sorted(bench_dir.glob("[0-9][0-9]_*.json")):
        doc = json.loads(p.read_text(encoding="utf-8"))
        key = p.stem
        label = STAGE_LABELS.get(key, key)
        items.append((key, label, doc))
    return items


def render_markdown(stages: list[tuple[str, str, dict[str, Any]]]) -> str:
    if not stages:
        return "# Performans Karşılaştırma Raporu\n\nHenüz ölçüm bulunamadı.\n"

    base_doc = stages[0][2]
    latest_doc = stages[-1][2]
    lines: list[str] = []
    lines.append("# `sqliters` vs C SQLite 3.54.1 — Kapsamlı Performans, RAM ve İkili Boyut Raporu\n")
    lines.append(
        f"- **Donanım / İS**: `{latest_doc['environment']['cpu']}` "
        f"({latest_doc['environment']['logical_cpus']} iş parçacığı), `{latest_doc['environment']['os']}`"
    )
    lines.append(f"- **Rust Derleyicisi**: `{latest_doc['environment']['rustc']}`")
    lines.append(
        f"- **C SQLite Referansı**: `{latest_doc['environment']['c_compiler']}` "
        f"(SQLite `3.54.1`, `speedbench` için `-O3 -DNDEBUG`, `./sqltest` sabit baz için `-O2 MAX_COLUMN=2000`)\n"
    )

    # 1. Binary & Suite Summary Table
    lines.append("## 1. Özet: İkili Boyut, Tepe RAM (`Peak RSS`) ve Genel Hız Oranları\n")
    lines.append(
        "| Aşama | Dal / Commit | İkili Boyut (`KiB`) | `.text` (`KiB`) | `speedbench` Toplam Süre (`ms`) | `speedbench` Geomean (`Rust/C`) | `speedbench` Tepe RSS (`KiB`) | `./sqltest` CLI Geomean (`Rust/C`) |"
    )
    lines.append("| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |")

    c_bin = latest_doc.get("binaries", {}).get("c_cli_o3", {})
    c_total_ms = latest_doc["summary"]["sum_median_c_ms"]
    c_peak_rss = latest_doc["summary"]["c_suite_peak_rss_kib"]
    lines.append(
        f"| **C SQLite 3.54.1 (`-O3` / `-O2`)** | `referans` | "
        f"`{c_bin.get('file_kib', 0):,.1f}` | `{c_bin.get('text_kib', 0):,.1f}` | "
        f"**`{c_total_ms:,.2f}`** | **`1.00x`** | **`{c_peak_rss:,}`** | **`1.00x`** |"
    )
    for _, label, doc in stages:
        r_bin = doc.get("binaries", {}).get("rust_cli", {})
        s = doc["summary"]
        st = doc.get("sqltest_cli_command_set", {})
        st_geo = st.get("geometric_mean_candidate_over_c")
        st_str = f"**`{st_geo:.2f}x`**" if st_geo is not None else "`N/A`"
        lines.append(
            f"| **{label}** | `{doc.get('git_branch', '')}@{doc.get('git_commit', '')}` | "
            f"`{r_bin.get('file_kib', 0):,.1f}` | `{r_bin.get('text_kib', 0):,.1f}` | "
            f"`{s['sum_median_rust_ms']:,.2f}` | **`{s['geometric_mean_rust_over_c']:.2f}x`** | "
            f"`{s['rust_suite_peak_rss_kib']:,}` | {st_str} |"
        )
    lines.append("")

    # 2. ./sqltest 126-sample CLI command-set table (Time + RAM)
    sqltest_stages = [(k, lbl, d) for (k, lbl, d) in stages if "sqltest_cli_command_set" in d]
    if sqltest_stages:
        lines.append("## 2. `./sqltest` Uçtan Uca CLI Komut Seti Ölçümleri (`126` Dondurulmuş Örnek + `audit.py`)\n")
        lines.append("### 2A. Medyan Süreler (`ms`) ve C SQLite Oranı (`Rust / C`)\n")
        hdr = ["Komut Ailesi (`18` örnek/satır)", "C SQLite (`ms`)"]
        for _, lbl, _ in sqltest_stages:
            hdr.append(f"{lbl} (`ms` / oran)")
        lines.append("| " + " | ".join(hdr) + " |")
        lines.append("| :--- | ---: | " + " | ".join(["---:" for _ in sqltest_stages]) + " |")

        fam_names = [
            f["family_case"]
            for f in sqltest_stages[0][2]["sqltest_cli_command_set"]["families"]
        ]
        for fam in fam_names:
            c_ms = next(
                f["c_reference_median_ms"]
                for f in sqltest_stages[0][2]["sqltest_cli_command_set"]["families"]
                if f["family_case"] == fam
            )
            row_cells = [f"`{fam}`", f"`{c_ms:.2f}`"]
            for _, _, d in sqltest_stages:
                item = next(
                    f for f in d["sqltest_cli_command_set"]["families"] if f["family_case"] == fam
                )
                row_cells.append(
                    f"`{item['candidate_median_ms']:.2f}` (**{item['candidate_over_c_ratio']:.2f}x**)"
                )
            lines.append("| " + " | ".join(row_cells) + " |")
        lines.append("")

        lines.append("### 2B. `./sqltest` Çocuk CLI Süreci Tepe RAM Kullanımı (`Peak RSS`, `KiB`)\n")
        hdr_rss = ["Komut Ailesi", "C SQLite Peak RSS (`KiB`)"]
        for _, lbl, _ in sqltest_stages:
            hdr_rss.append(f"{lbl} Peak RSS (`KiB`)")
        lines.append("| " + " | ".join(hdr_rss) + " |")
        lines.append("| :--- | ---: | " + " | ".join(["---:" for _ in sqltest_stages]) + " |")
        for fam in fam_names:
            c_rss = next(
                f["c_peak_rss_kib"]
                for f in sqltest_stages[-1][2]["sqltest_cli_command_set"]["families"]
                if f["family_case"] == fam
            )
            row_cells = [f"`{fam}`", f"`{c_rss:,}`"]
            for _, _, d in sqltest_stages:
                item = next(
                    f for f in d["sqltest_cli_command_set"]["families"] if f["family_case"] == fam
                )
                row_cells.append(f"`{item['candidate_peak_rss_kib']:,}`")
            lines.append("| " + " | ".join(row_cells) + " |")
        lines.append("")

    # 3. speedbench W01..W14 table (Time + RAM)
    lines.append("## 3. `speedbench` Özellik & Motor API Ölçümleri (`W01`–`W14`, `FNV-1a` Doğrulamalı)\n")
    lines.append("### 3A. Medyan İşlem Süreleri (`ms`) ve C SQLite `-O3` Oranı (`Rust / C`)\n")
    sb_hdr = ["İş Yükü", "Depolama", "C `-O3` (`ms`)"]
    for _, lbl, _ in stages:
        sb_hdr.append(f"{lbl} (`ms` / oran)")
    lines.append("| " + " | ".join(sb_hdr) + " |")
    lines.append("| :--- | :--- | ---: | " + " | ".join(["---:" for _ in stages]) + " |")

    w_names = [w["name"] for w in base_doc["workloads"]]
    for w_name in w_names:
        latest_w = next(w for w in latest_doc["workloads"] if w["name"] == w_name)
        row = [f"`{w_name}`", f"`{latest_w['storage']}`", f"`{latest_w['c_median_ms']:.2f}`"]
        for _, _, d in stages:
            w = next(x for x in d["workloads"] if x["name"] == w_name)
            marker = " 🏆" if w["rust_over_c_ratio"] <= 1.0 else ""
            row.append(f"`{w['rust_median_ms']:.2f}` (**{w['rust_over_c_ratio']:.2f}x**{marker})")
        lines.append("| " + " | ".join(row) + " |")
    lines.append("")

    lines.append("### 3B. `speedbench` RAM Kullanımı (`VmRSS` / Çocuk `Peak RSS` ve C SQLite İç Yığın Tepe Belleği, `KiB`)\n")
    ram_hdr = ["İş Yükü", "C `-O3` RSS (`KiB`)", "C `SQLITE_STATUS_MEMORY_USED` Tepe (`KiB`)"]
    for _, lbl, _ in stages:
        ram_hdr.append(f"{lbl} RSS (`KiB`)")
    lines.append("| " + " | ".join(ram_hdr) + " |")
    lines.append("| :--- | ---: | ---: | " + " | ".join(["---:" for _ in stages]) + " |")
    for w_name in w_names:
        latest_w = next(w for w in latest_doc["workloads"] if w["name"] == w_name)
        row = [
            f"`{w_name}`",
            f"`{latest_w['c_rss_kib']:,}`",
            f"`{latest_w['c_sqlite_heap_peak_kib']:,}`",
        ]
        for _, _, d in stages:
            w = next(x for x in d["workloads"] if x["name"] == w_name)
            row.append(f"`{w['rust_rss_kib']:,}`")
        lines.append("| " + " | ".join(row) + " |")
    lines.append("")

    return "\n".join(lines)


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    bench_dir = repo_root / "benchmarks"
    bench_dir.mkdir(parents=True, exist_ok=True)
    stages = load_stages(bench_dir)
    md = render_markdown(stages)
    out_path = bench_dir / "benchmark_report.md"
    out_path.write_text(md, encoding="utf-8")
    print(f"Wrote {out_path} ({len(stages)} stages)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
