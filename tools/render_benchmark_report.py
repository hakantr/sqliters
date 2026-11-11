#!/usr/bin/env python3
"""
Renders `/home/hakanbiris/github/sqliters/benchmarks/benchmark_report.md` from all
`benchmarks/[0-9][0-9]_*.json` snapshots, recording:
  1. Cross-stage summary (Binary size, CPU time, Peak RSS, Geomean vs Saf Rust & vs C SQLite 3.54.1)
  2. Cross-stage `./sqltest` (126 CLI samples) & `speedbench` (W01-W14) matrices
  3. Dedicated per-branch / per-stage sections under their own headings (`## ...`), evaluating
     `performans-swar`, `performans-simd`, `performans-asm`, and `performans-hibrit` directly
     against Pure Safe Rust (`performans`, `03_performans_p1c`) as well as C SQLite 3.54.1
  4. Optional `benchmarks/final_c_comparison_analysis.md` section for CPU & RAM root-cause analysis
"""

from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any


STAGE_META: dict[str, dict[str, str]] = {
    "00_main_baseline": {
        "short": "main",
        "heading": "Başlangıç Referansı: `main` Dalı (Optimizasyon Öncesi Port)",
    },
    "01_performans_p1a": {
        "short": "P1-A",
        "heading": "`performans` (Adım P1-A) — Sanal Tablolar (`FTS5`/`R-Tree`), İkili `JSONB` ve Kayıt Karşılaştırma",
    },
    "02_performans_p1b": {
        "short": "P1-B",
        "heading": "`performans` (Adım P1-B) — Yoğun Pager/WAL, Ham B-Tree, Sıfır Kilitli `Statement` ve CLI Veri Yükleme",
    },
    "03_performans_p1c": {
        "short": "Saf Rust (performans)",
        "heading": "`performans` (Adım P1-C) — %100 Güvenli Saf Rust Tavanı (`#![forbid(unsafe_code)]`, SWAR/SIMD/ASM Yok)",
    },
    "04_performans_swar": {
        "short": "SWAR (performans-swar)",
        "heading": "Dal 1: `performans-swar` — Saf Rust'a Karşı Yazmaç-İçi SWAR Deneyi (`#![forbid(unsafe_code)]`)",
    },
    "05_performans_simd": {
        "short": "SIMD (performans-simd)",
        "heading": "Dal 2: `performans-simd` — Saf Rust'a Karşı Donanım SIMD (`core::arch` AVX2/SSE2/BMI2) Deneyi",
    },
    "06_performans_asm": {
        "short": "ASM (performans-asm)",
        "heading": "Dal 3: `performans-asm` — Saf Rust'a Karşı El Yazımı Satır-İçi Assembly (`asm!`) Deneyi",
    },
    "07_performans_morsel": {
        "short": "Morsel/SSO (performans-morsel)",
        "heading": "Dal 4: `performans-morsel` — Sütunsal Mikro-Yığın (Vectorized Batch / SOA) & Sıfır-Tahsis Satır/Değer (`German-String SSO`) Deneyi",
    },
    "08_performans_io_mmap": {
        "short": "IO/mmap (performans-io-mmap)",
        "heading": "Dal 5: `performans-io-mmap` — Sıfır-Kopya `mmap` Okuma, Vektörel `pwritev` WAL G/Ç ve Hızlı Oturum Kapısı Deneyi",
    },
    "09_performans_pgo": {
        "short": "PGO/Derleyici (performans-pgo)",
        "heading": "Dal 6: `performans-pgo` — LLVM Profil Güdümlü Optimizasyon (`PGO` + `znver4` + `build-std`/`panic=abort`) Deneyi",
    },
    "10_performans_hibrit": {
        "short": "Birleşik (performans-hibrit)",
        "heading": "Dal 7: `performans-hibrit` — Saf Rust'a Karşı Üstünlük Kanıtlayan Tüm Yaklaşımların Birleşimi",
    },
    "11_performans_1": {
        "short": "performans-1",
        "heading": "Format Türevi 1: `performans-1` — Hizalı Sabit Ofsetli Kayıt & Yaprak Parmak İzi",
    },
    "12_performans_2": {
        "short": "performans-2",
        "heading": "Format Türevi 2: `performans-2` — PAX / Sayfa-İçi Sütunsal Yaprak Düzeni",
    },
}


def load_stages(bench_dir: Path) -> list[tuple[str, str, str, dict[str, Any]]]:
    items: list[tuple[str, str, str, dict[str, Any]]] = []
    for p in sorted(bench_dir.glob("[0-9][0-9]_*.json")):
        doc = json.loads(p.read_text(encoding="utf-8"))
        key = p.stem
        meta = STAGE_META.get(key, {"short": key, "heading": f"Aşama / Dal: `{key}`"})
        items.append((key, meta["short"], meta["heading"], doc))
    return items


def geomean_ratios(pairs: list[tuple[float, float]]) -> float | None:
    vals = [a / b for (a, b) in pairs if a > 0 and b > 0]
    if not vals:
        return None
    return math.exp(sum(math.log(v) for v in vals) / len(vals))


def render_stage_section(
    key: str,
    short_label: str,
    heading: str,
    doc: dict[str, Any],
    pure_rust_doc: dict[str, Any] | None,
) -> list[str]:
    lines: list[str] = []
    lines.append(f"## {heading}\n")

    branch = doc.get("git_branch", "")
    commit = doc.get("git_commit", "")
    r_bin = doc.get("binaries", {}).get("rust_cli", {})
    c_bin = doc.get("binaries", {}).get("c_cli_o3", {})
    s = doc["summary"]

    compare_to_pure = pure_rust_doc is not None and key not in (
        "00_main_baseline",
        "01_performans_p1a",
        "02_performans_p1b",
        "03_performans_p1c",
    )

    lines.append(f"- **Dal / Commit**: `{branch}@{commit}`")
    lines.append(
        f"- **CLI İkili Boyutu**: `{r_bin.get('file_kib', 0):,.1f} KiB` "
        f"(`.text`: `{r_bin.get('text_kib', 0):,.1f} KiB`) "
        f"| C `-O3` Referans: `{c_bin.get('file_kib', 0):,.1f} KiB` (`.text`: `{c_bin.get('text_kib', 0):,.1f} KiB`)"
    )
    lines.append(
        f"- **`speedbench` Toplam Süre & Tepe RSS**: `{s['sum_median_rust_ms']:,.2f} ms` "
        f"(C `-O3`: `{s['sum_median_c_ms']:,.2f} ms`, Geomean `Rust/C`: **`{s['geometric_mean_rust_over_c']:.3f}x`**) | "
        f"Tepe RSS: **`{s['rust_suite_peak_rss_kib']:,} KiB`** (C: `{s['c_suite_peak_rss_kib']:,} KiB`)"
    )

    if compare_to_pure and pure_rust_doc is not None:
        pr_map = {w["name"]: w for w in pure_rust_doc["workloads"]}
        sb_gm_vs_pure = geomean_ratios(
            [
                (w["rust_median_ms"], pr_map[w["name"]]["rust_median_ms"])
                for w in doc["workloads"]
                if w["name"] in pr_map
            ]
        )
        pr_sum = pure_rust_doc["summary"]["sum_median_rust_ms"]
        pr_rss = pure_rust_doc["summary"]["rust_suite_peak_rss_kib"]
        if sb_gm_vs_pure is not None:
            lines.append(
                f"- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: "
                f"Toplam `{pr_sum:,.2f} ms` → **`{s['sum_median_rust_ms']:,.2f} ms`**, "
                f"Geomean (`Bu Dal / Saf Rust`): **`{sb_gm_vs_pure:.3f}x`** "
                f"(`%{(1.0 - sb_gm_vs_pure) * 100.0:+.1f}` süre değişimi), "
                f"Tepe RSS: `{pr_rss:,} KiB` → **`{s['rust_suite_peak_rss_kib']:,} KiB`**"
            )

    if "sqltest_cli_command_set" in doc:
        st = doc["sqltest_cli_command_set"]
        st_line = (
            f"- **`./sqltest` 126-Örnek CLI Özeti**: Durum `{st.get('status')}`, "
            f"Geomean (`Bu Dal / C`): **`{st.get('geometric_mean_candidate_over_c', 0.0):.3f}x`**"
        )
        if (
            compare_to_pure
            and pure_rust_doc is not None
            and "sqltest_cli_command_set" in pure_rust_doc
        ):
            pr_fams = {
                f["family_case"]: f
                for f in pure_rust_doc["sqltest_cli_command_set"]["families"]
            }
            st_gm_vs_pure = geomean_ratios(
                [
                    (f["candidate_median_ms"], pr_fams[f["family_case"]]["candidate_median_ms"])
                    for f in st["families"]
                    if f["family_case"] in pr_fams
                ]
            )
            if st_gm_vs_pure is not None:
                st_line += (
                    f", Geomean (`Bu Dal / Saf Rust`): **`{st_gm_vs_pure:.3f}x`** "
                    f"(`%{(1.0 - st_gm_vs_pure) * 100.0:+.1f}`)"
                )
        lines.append(st_line)

    lines.append("")

    # Optional kernel scope notes (included vs excluded kernels)
    scope_notes = doc.get("experiment_notes")
    if isinstance(scope_notes, dict):
        if scope_notes.get("summary"):
            lines.append(f"> {scope_notes['summary']}\n")
        inc = scope_notes.get("included_kernels", [])
        exc = scope_notes.get("excluded_kernels", [])
        if inc:
            lines.append("**Saf Rust'a Karşı Üstünlük Sağlayıp Kapsama Alınan Çekirdekler:**")
            for item in inc:
                lines.append(f"- {item}")
            lines.append("")
        if exc:
            lines.append("**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**")
            for item in exc:
                lines.append(f"- {item}")
            lines.append("")

    # Per-stage ./sqltest table
    if "sqltest_cli_command_set" in doc:
        lines.append(f"### {short_label} — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)\n")
        if compare_to_pure and pure_rust_doc is not None and "sqltest_cli_command_set" in pure_rust_doc:
            pr_fams = {
                f["family_case"]: f
                for f in pure_rust_doc["sqltest_cli_command_set"]["families"]
            }
            lines.append(
                "| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |"
            )
            lines.append("| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
            for f in doc["sqltest_cli_command_set"]["families"]:
                pf = pr_fams[f["family_case"]]
                vs_pure = f["candidate_median_ms"] / pf["candidate_median_ms"]
                lines.append(
                    f"| `{f['family_case']}` | `{pf['candidate_median_ms']:.2f}` | "
                    f"**`{f['candidate_median_ms']:.2f}`** | **`{vs_pure:.3f}x`** | "
                    f"`{f['c_reference_median_ms']:.2f}` | `{f['candidate_over_c_ratio']:.3f}x` | "
                    f"`{pf['candidate_peak_rss_kib']:,}` | `{f['candidate_peak_rss_kib']:,}` | "
                    f"`{f['c_peak_rss_kib']:,}` |"
                )
        else:
            lines.append(
                "| Komut Ailesi | Medyan (`ms`) | P95 (`ms`) | C Medyan (`ms`) | `Rust / C` | Rust Peak RSS (`KiB`) | C Peak RSS (`KiB`) |"
            )
            lines.append("| :--- | ---: | ---: | ---: | ---: | ---: | ---: |")
            for f in doc["sqltest_cli_command_set"]["families"]:
                lines.append(
                    f"| `{f['family_case']}` | **`{f['candidate_median_ms']:.2f}`** | "
                    f"`{f['candidate_p95_ms']:.2f}` | `{f['c_reference_median_ms']:.2f}` | "
                    f"**`{f['candidate_over_c_ratio']:.3f}x`** | `{f['candidate_peak_rss_kib']:,}` | "
                    f"`{f['c_peak_rss_kib']:,}` |"
                )
        lines.append("")

    # Per-stage speedbench table
    lines.append(f"### {short_label} — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)\n")
    if compare_to_pure and pure_rust_doc is not None:
        pr_map = {w["name"]: w for w in pure_rust_doc["workloads"]}
        lines.append(
            "| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |"
        )
        lines.append("| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
        for w in doc["workloads"]:
            pw = pr_map[w["name"]]
            vs_pure = w["rust_median_ms"] / pw["rust_median_ms"]
            lines.append(
                f"| `{w['name']}` | `{pw['rust_median_ms']:.2f}` | "
                f"**`{w['rust_median_ms']:.2f}`** | **`{vs_pure:.3f}x`** | "
                f"`{w['c_median_ms']:.2f}` | `{w['rust_over_c_ratio']:.2f}x` | "
                f"`{pw['rust_rss_kib']:,}` | `{w['rust_rss_kib']:,}` | `{w['c_rss_kib']:,}` |"
            )
    else:
        lines.append(
            "| İş Yükü | Depolama | Rust (`ms`) | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | C RSS (`KiB`) | C İç Yığın (`KiB`) | FNV-1a |"
        )
        lines.append("| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |")
        for w in doc["workloads"]:
            lines.append(
                f"| `{w['name']}` | `{w['storage']}` | **`{w['rust_median_ms']:.2f}`** | "
                f"`{w['c_median_ms']:.2f}` | **`{w['rust_over_c_ratio']:.2f}x`** | "
                f"`{w['rust_rss_kib']:,}` | `{w['c_rss_kib']:,}` | "
                f"`{w['c_sqlite_heap_peak_kib']:,}` | `{w['digest_fnv1a64']}` ✓ |"
            )
    lines.append("")
    return lines


def render_markdown(
    stages: list[tuple[str, str, str, dict[str, Any]]],
    bench_dir: Path,
) -> str:
    if not stages:
        return "# Performans Karşılaştırma Raporu\n\nHenüz ölçüm bulunamadı.\n"

    base_doc = stages[0][3]
    latest_doc = stages[-1][3]
    pure_rust_doc = next((d for (k, _, _, d) in stages if k == "03_performans_p1c"), None)

    lines: list[str] = []
    lines.append("# `sqliters` — Kapsamlı Performans, Saf Rust / SWAR / SIMD / ASM Deney Matrisi ve RAM Raporu\n")
    lines.append(
        f"- **Donanım / İS**: `{latest_doc['environment']['cpu']}` "
        f"({latest_doc['environment']['logical_cpus']} iş parçacığı), `{latest_doc['environment']['os']}`"
    )
    lines.append(f"- **Rust Derleyicisi**: `{latest_doc['environment']['rustc']}`")
    lines.append(
        f"- **C SQLite Referansı**: `{latest_doc['environment']['c_compiler']}` "
        f"(SQLite `3.54.1`, `speedbench` için `-O3 -DNDEBUG`, `./sqltest` sabit baz için `-O2 MAX_COLUMN=2000`)\n"
    )

    # 1. Cross-stage summary table
    lines.append("## 1. Genel Karşılaştırma Özeti (Tüm Kayıtlı Dallar ve Aşamalar)\n")
    lines.append(
        "| Aşama / Dal | Git (`dal@commit`) | İkili Boyut (`KiB`) | `.text` (`KiB`) | `speedbench` Toplam (`ms`) | `speedbench` Geomean (`vs Saf Rust`) | `speedbench` Geomean (`vs C`) | `speedbench` Tepe RSS (`KiB`) | `./sqltest` Geomean (`vs Saf Rust`) | `./sqltest` Geomean (`vs C`) |"
    )
    lines.append("| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")

    c_bin = latest_doc.get("binaries", {}).get("c_cli_o3", {})
    c_total_ms = latest_doc["summary"]["sum_median_c_ms"]
    c_peak_rss = latest_doc["summary"]["c_suite_peak_rss_kib"]
    lines.append(
        f"| **C SQLite 3.54.1 (`-O3` / `-O2`)** | `referans` | "
        f"`{c_bin.get('file_kib', 0):,.1f}` | `{c_bin.get('text_kib', 0):,.1f}` | "
        f"**`{c_total_ms:,.2f}`** | `—` | **`1.000x`** | **`{c_peak_rss:,}`** | `—` | **`1.000x`** |"
    )

    for key, short_label, _, doc in stages:
        r_bin = doc.get("binaries", {}).get("rust_cli", {})
        s = doc["summary"]
        st = doc.get("sqltest_cli_command_set", {})
        st_geo = st.get("geometric_mean_candidate_over_c")
        st_str = f"**`{st_geo:.3f}x`**" if st_geo is not None else "`N/A`"

        sb_vs_pure_str = "`1.000x` *(baz)*" if key == "03_performans_p1c" else "`—`"
        st_vs_pure_str = "`1.000x` *(baz)*" if key == "03_performans_p1c" else "`—`"
        if pure_rust_doc is not None and key not in (
            "00_main_baseline",
            "01_performans_p1a",
            "02_performans_p1b",
            "03_performans_p1c",
        ):
            pr_map = {w["name"]: w for w in pure_rust_doc["workloads"]}
            sb_gm = geomean_ratios(
                [
                    (w["rust_median_ms"], pr_map[w["name"]]["rust_median_ms"])
                    for w in doc["workloads"]
                    if w["name"] in pr_map
                ]
            )
            if sb_gm is not None:
                sb_vs_pure_str = f"**`{sb_gm:.3f}x`**"
            if "sqltest_cli_command_set" in doc and "sqltest_cli_command_set" in pure_rust_doc:
                pr_fams = {
                    f["family_case"]: f
                    for f in pure_rust_doc["sqltest_cli_command_set"]["families"]
                }
                st_gm = geomean_ratios(
                    [
                        (f["candidate_median_ms"], pr_fams[f["family_case"]]["candidate_median_ms"])
                        for f in doc["sqltest_cli_command_set"]["families"]
                        if f["family_case"] in pr_fams
                    ]
                )
                if st_gm is not None:
                    st_vs_pure_str = f"**`{st_gm:.3f}x`**"

        lines.append(
            f"| **{short_label}** | `{doc.get('git_branch', '')}@{doc.get('git_commit', '')}` | "
            f"`{r_bin.get('file_kib', 0):,.1f}` | `{r_bin.get('text_kib', 0):,.1f}` | "
            f"`{s['sum_median_rust_ms']:,.2f}` | {sb_vs_pure_str} | "
            f"**`{s['geometric_mean_rust_over_c']:.3f}x`** | "
            f"`{s['rust_suite_peak_rss_kib']:,}` | {st_vs_pure_str} | {st_str} |"
        )
    lines.append("")

    # 2. Cross-stage ./sqltest matrix
    sqltest_stages = [
        (k, lbl, d) for (k, lbl, _, d) in stages if "sqltest_cli_command_set" in d
    ]
    if sqltest_stages:
        lines.append("## 2. Toplu `./sqltest` Uçtan Uca CLI Matrisi (`126` Örnek)\n")
        hdr = ["Komut Ailesi (`18` örnek)", "C SQLite (`ms`)"]
        for _, lbl, _ in sqltest_stages:
            hdr.append(f"{lbl} (`ms` / C oranı)")
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

    # 3. Cross-stage speedbench matrix
    lines.append("## 3. Toplu `speedbench` Özellik & Motor API Matrisi (`W01`–`W14`)\n")
    sb_hdr = ["İş Yükü", "C `-O3` (`ms`)"]
    for _, lbl, _, _ in stages:
        sb_hdr.append(f"{lbl} (`ms` / C oranı)")
    lines.append("| " + " | ".join(sb_hdr) + " |")
    lines.append("| :--- | ---: | " + " | ".join(["---:" for _ in stages]) + " |")
    w_names = [w["name"] for w in base_doc["workloads"]]
    for w_name in w_names:
        latest_w = next(w for w in latest_doc["workloads"] if w["name"] == w_name)
        row = [f"`{w_name}`", f"`{latest_w['c_median_ms']:.2f}`"]
        for _, _, _, d in stages:
            w = next(x for x in d["workloads"] if x["name"] == w_name)
            row.append(f"`{w['rust_median_ms']:.2f}` (**{w['rust_over_c_ratio']:.2f}x**)")
        lines.append("| " + " | ".join(row) + " |")
    lines.append("")

    lines.append("---\n")

    # 4+. Dedicated Individual Headings for EVERY Recorded Branch / Stage
    for key, short_label, heading, doc in stages:
        lines.extend(render_stage_section(key, short_label, heading, doc, pure_rust_doc))
        lines.append("---\n")

    # Optional Final Root-Cause Analysis Appendix
    analysis_md = bench_dir / "final_c_comparison_analysis.md"
    if analysis_md.exists():
        lines.append(analysis_md.read_text(encoding="utf-8").rstrip())
        lines.append("")

    return "\n".join(lines)


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    bench_dir = repo_root / "benchmarks"
    bench_dir.mkdir(parents=True, exist_ok=True)
    stages = load_stages(bench_dir)
    md = render_markdown(stages, bench_dir)
    out_path = bench_dir / "benchmark_report.md"
    out_path.write_text(md, encoding="utf-8")
    print(f"Wrote {out_path} ({len(stages)} stages)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
