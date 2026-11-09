//! Parallel diagnostic runner for SQLite `.test` files using `sqliters-tcltest`.
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqliters_tcltest::{TestFileReport, evaluate_test_script_in_root, strip_test_comments};

pub const CORE_SQL_FAMILIES: &[&str] = &[
    "select",
    "insert",
    "update",
    "delete",
    "join",
    "where",
    "index",
    "fkey",
    "trans",
    "pragma",
    "collate",
    "subquery",
    "orderby",
    "in",
    "between",
    "cast",
    "check",
    "conflict",
    "default",
    "null",
    "notnull",
    "types",
    "affinity",
    "alter",
    "vacuum",
    "savepoint",
    "date",
    "func",
    "like",
    "rowvalue",
    "unique",
    "autoinc",
    "table",
    "distinct",
    "limit",
    "count",
    "coalesce",
    "quote",
    "numcast",
    "intpkey",
    "rowid",
    "enc",
    "lock",
    "wal",
    "autovacuum",
    "incrvacuum",
    "attach",
    "tempdb",
];

pub const CORE_SQL_EXACT: &[&str] = &[
    "with1",
    "with2",
    "with3",
    "with4",
    "with5",
    "with6",
    "expr",
    "expr2",
    "returning1",
    "utf16align",
];

fn is_core_sql_test(stem: &str) -> bool {
    if stem == "countofview" {
        return false;
    }
    if CORE_SQL_EXACT.contains(&stem) {
        return true;
    }
    let alpha_prefix: String = stem
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || *c == '_')
        .collect();
    CORE_SQL_FAMILIES.contains(&alpha_prefix.as_str())
}

fn infer_source_root(path: &Path) -> &'static str {
    let s = path.to_string_lossy();
    if s.contains("sqlite/ext/fts5/test") {
        "sqlite/ext/fts5/test"
    } else if s.contains("sqlite/ext/rtree") {
        "sqlite/ext/rtree"
    } else {
        "sqlite/test"
    }
}

#[derive(Debug, Clone)]
struct ManifestEntryInfo {
    source_path: PathBuf,
    target_phase: String,
    status: String,
    ignore_reason: String,
}

fn load_manifest_entries(manifest_path: &Path) -> Vec<ManifestEntryInfo> {
    let Ok(raw) = fs::read_to_string(manifest_path) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut cur_path: Option<PathBuf> = None;
    let mut cur_phase = String::new();
    let mut cur_status = String::new();
    let mut cur_reason = String::new();

    let extract_str = |rest: &str| -> String {
        rest.trim()
            .trim_end_matches(',')
            .trim()
            .trim_matches('"')
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    };

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("\"source_path\":") {
            cur_path = Some(PathBuf::from(extract_str(rest)));
        } else if let Some(rest) = trimmed
            .strip_prefix("\"phase\":")
            .or_else(|| trimmed.strip_prefix("\"target_phase\":"))
        {
            cur_phase = extract_str(rest);
        } else if let Some(rest) = trimmed.strip_prefix("\"status\":") {
            cur_status = extract_str(rest);
        } else if let Some(rest) = trimmed.strip_prefix("\"ignore_reason\":") {
            cur_reason = extract_str(rest);
        } else if trimmed.starts_with('}')
            && let Some(p) = cur_path.take()
        {
            if p.exists() {
                entries.push(ManifestEntryInfo {
                    source_path: p,
                    target_phase: std::mem::take(&mut cur_phase),
                    status: std::mem::take(&mut cur_status),
                    ignore_reason: std::mem::take(&mut cur_reason),
                });
            } else {
                cur_phase.clear();
                cur_status.clear();
                cur_reason.clear();
            }
        }
    }
    entries
}

fn collect_test_files(
    args: &[String],
    manifest_only: bool,
    phase_filter: Option<&str>,
    manifest: &[ManifestEntryInfo],
) -> Vec<PathBuf> {
    if manifest_only || phase_filter.is_some() {
        let mut out: Vec<PathBuf> = manifest
            .iter()
            .filter(|e| {
                if let Some(ph) = phase_filter
                    && e.target_phase != ph
                {
                    return false;
                }
                args.is_empty()
                    || args.iter().any(|f| {
                        e.source_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|st| st.starts_with(f.as_str()))
                    })
            })
            .map(|e| e.source_path.clone())
            .collect();
        out.sort();
        return out;
    }

    let roots = [
        Path::new("sqlite/test"),
        Path::new("sqlite/ext/fts5/test"),
        Path::new("sqlite/ext/rtree"),
    ];
    if !args.is_empty() {
        let mut out = Vec::new();
        for a in args {
            let p = PathBuf::from(a);
            if p.exists() {
                out.push(p);
            } else {
                for root in roots {
                    let candidate = root.join(format!("{a}.test"));
                    if candidate.exists() {
                        out.push(candidate);
                        break;
                    }
                }
            }
        }
        return out;
    }

    let mut files = Vec::new();
    let test_dir = Path::new("sqlite/test");
    if let Ok(entries) = fs::read_dir(test_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("test")
                && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                && is_core_sql_test(stem)
            {
                files.push(p);
            }
        }
    }
    for me in manifest {
        if !files.contains(&me.source_path) {
            files.push(me.source_path.clone());
        }
    }
    files.sort();
    files
}

enum SuiteItemOutcome {
    Executed(TestFileReport),
    ManifestSkipped { file_name: String, reason: String },
}

impl SuiteItemOutcome {
    fn file_name(&self) -> &str {
        match self {
            Self::Executed(r) => &r.file_name,
            Self::ManifestSkipped { file_name, .. } => file_name,
        }
    }
}

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let mut verbose = false;
    let mut list_only = false;
    let mut manifest_only = false;
    let mut manifest_path = PathBuf::from("tests/sqlite_test_manifest.json");
    let mut phase_filter: Option<String> = None;
    let mut run_ignored = false;
    let mut filter_args = Vec::new();
    let mut idx = 0usize;
    while idx < raw_args.len() {
        let a = &raw_args[idx];
        if a == "-v" || a == "--verbose" {
            verbose = true;
            idx += 1;
        } else if a == "--list" {
            list_only = true;
            idx += 1;
        } else if a == "--manifest" {
            manifest_only = true;
            if idx + 1 < raw_args.len()
                && !raw_args[idx + 1].starts_with('-')
                && raw_args[idx + 1].ends_with(".json")
            {
                manifest_path = PathBuf::from(&raw_args[idx + 1]);
                idx += 2;
            } else {
                idx += 1;
            }
        } else if let Some(p) = a.strip_prefix("--manifest=") {
            manifest_only = true;
            manifest_path = PathBuf::from(p);
            idx += 1;
        } else if a == "--phase" {
            manifest_only = true;
            if idx + 1 < raw_args.len() {
                phase_filter = Some(raw_args[idx + 1].clone());
                idx += 2;
            } else {
                idx += 1;
            }
        } else if let Some(ph) = a.strip_prefix("--phase=") {
            manifest_only = true;
            phase_filter = Some(ph.to_owned());
            idx += 1;
        } else if a == "--run-ignored" {
            run_ignored = true;
            idx += 1;
        } else {
            filter_args.push(a.clone());
            idx += 1;
        }
    }

    let manifest = load_manifest_entries(&manifest_path);
    let files = collect_test_files(
        &filter_args,
        manifest_only,
        phase_filter.as_deref(),
        &manifest,
    );
    let total_files = files.len();
    if total_files == 0 {
        eprintln!("No matching .test files found.");
        std::process::exit(1);
    }

    if list_only {
        let mut active_cnt = 0usize;
        let mut ignored_cnt = 0usize;
        let mut unmanifested_cnt = 0usize;
        for path in &files {
            if let Some(me) = manifest.iter().find(|e| &e.source_path == path) {
                if me.status == "active" {
                    active_cnt += 1;
                    println!("ACTIVE  [phase {:>4}] {}", me.target_phase, path.display());
                } else {
                    ignored_cnt += 1;
                    println!(
                        "IGNORED [phase {:>4}] {} ({})",
                        me.target_phase,
                        path.display(),
                        me.ignore_reason
                    );
                }
            } else {
                unmanifested_cnt += 1;
                println!("CORE    [static    ] {}", path.display());
            }
        }
        println!(
            "Total listed: {} ({} manifest-active, {} manifest-ignored, {} core-static)",
            total_files, active_cnt, ignored_cnt, unmanifested_cnt
        );
        return;
    }

    // Respect manifest `ignored` status unless explicitly running individual named stems without
    // `--manifest`, or when `--run-ignored` is passed.
    let honor_manifest_ignores = !run_ignored && (manifest_only || filter_args.is_empty());

    let next_idx = AtomicUsize::new(0);
    let results: Mutex<Vec<SuiteItemOutcome>> = Mutex::new(Vec::with_capacity(total_files));
    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get().min(12))
        .unwrap_or(4)
        .min(total_files);

    std::thread::scope(|s| {
        for _ in 0..num_threads {
            std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn_scoped(s, || {
                    loop {
                        let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                        if idx >= total_files {
                            break;
                        }
                        let path = &files[idx];
                        let stem = path
                            .file_stem()
                            .and_then(|st| st.to_str())
                            .unwrap_or("unknown");
                        if honor_manifest_ignores
                            && let Some(me) = manifest.iter().find(|e| &e.source_path == path)
                            && me.status == "ignored"
                        {
                            results
                                .lock()
                                .unwrap()
                                .push(SuiteItemOutcome::ManifestSkipped {
                                    file_name: stem.to_owned(),
                                    reason: me.ignore_reason.clone(),
                                });
                            continue;
                        }
                        let raw = fs::read_to_string(path).unwrap_or_default();
                        let stripped = strip_test_comments(&raw);
                        let src_root = infer_source_root(path);
                        let rep = evaluate_test_script_in_root(src_root, stem, &stripped);
                        results
                            .lock()
                            .unwrap()
                            .push(SuiteItemOutcome::Executed(rep));
                    }
                })
                .expect("failed to spawn tclsuite worker");
        }
    });

    let mut outcomes = results.into_inner().unwrap();
    outcomes.sort_by(|a, b| a.file_name().cmp(b.file_name()));

    let mut files_ok = 0usize;
    let mut files_skipped = 0usize;
    let mut sub_passed = 0usize;
    let mut sub_failed = 0usize;
    let mut script_errs = 0usize;

    for item in &outcomes {
        match item {
            SuiteItemOutcome::ManifestSkipped { file_name, reason } => {
                files_skipped += 1;
                println!("SKIP {:20} (manifest ignored: {})", file_name, reason);
            }
            SuiteItemOutcome::Executed(r) => {
                sub_passed += r.passed;
                sub_failed += r.failed.len();
                if r.script_error.is_some() {
                    script_errs += 1;
                }
                if r.is_ok() {
                    files_ok += 1;
                    println!("PASS {:20} ({} subtests)", r.file_name, r.passed);
                } else if r.skipped_zero_subtests && r.failed.is_empty() && r.script_error.is_none()
                {
                    files_skipped += 1;
                    println!("SKIP {:20} (0 subtests executed)", r.file_name);
                } else {
                    println!(
                        "FAIL {:20} ({} passed, {} failed{})",
                        r.file_name,
                        r.passed,
                        r.failed.len(),
                        if let Some(ref se) = r.script_error {
                            format!(", script_error: {se}")
                        } else {
                            String::new()
                        }
                    );
                    if verbose {
                        for f in r.failed.iter().take(15) {
                            println!(
                                "     ! {} => expected [{}] | got [{}]",
                                f.name, f.expected, f.actual
                            );
                        }
                    }
                }
            }
        }
    }

    let files_failed = total_files - files_ok - files_skipped;
    println!("================================================================");
    println!(
        "Files:    {}/{} passed ({} skipped/0-subtests, {} failed, {} with script errors)",
        files_ok, total_files, files_skipped, files_failed, script_errs
    );
    println!(
        "Subtests: {}/{} passed ({} failed)",
        sub_passed,
        sub_passed + sub_failed,
        sub_failed
    );
    if files_failed > 0 {
        std::process::exit(1);
    }
}
