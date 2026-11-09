//! Parallel diagnostic runner for SQLite `.test` files using `sqliters-tcltest`.
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqliters_tcltest::{TestFileReport, evaluate_test_script, strip_test_comments};

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

fn collect_test_files(args: &[String]) -> Vec<PathBuf> {
    let test_dir = Path::new("sqlite/test");
    if !args.is_empty() {
        let mut out = Vec::new();
        for a in args {
            let p = PathBuf::from(a);
            if p.exists() {
                out.push(p);
            } else {
                let candidate = test_dir.join(format!("{a}.test"));
                if candidate.exists() {
                    out.push(candidate);
                }
            }
        }
        return out;
    }

    let mut files = Vec::new();
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
    files.sort();
    files
}

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let mut verbose = false;
    let mut filter_args = Vec::new();
    for a in raw_args {
        if a == "-v" || a == "--verbose" {
            verbose = true;
        } else {
            filter_args.push(a);
        }
    }

    let files = collect_test_files(&filter_args);
    let total_files = files.len();
    if total_files == 0 {
        eprintln!("No matching .test files found.");
        std::process::exit(1);
    }

    let next_idx = AtomicUsize::new(0);
    let results: Mutex<Vec<TestFileReport>> = Mutex::new(Vec::with_capacity(total_files));
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
                        let raw = fs::read_to_string(path).unwrap_or_default();
                        let stripped = strip_test_comments(&raw);
                        let rep = evaluate_test_script(stem, &stripped);
                        results.lock().unwrap().push(rep);
                    }
                })
                .expect("failed to spawn tclsuite worker");
        }
    });

    let mut reports = results.into_inner().unwrap();
    reports.sort_by(|a, b| a.file_name.cmp(&b.file_name));

    let mut files_ok = 0usize;
    let mut files_skipped = 0usize;
    let mut sub_passed = 0usize;
    let mut sub_failed = 0usize;
    let mut script_errs = 0usize;

    for r in &reports {
        sub_passed += r.passed;
        sub_failed += r.failed.len();
        if r.script_error.is_some() {
            script_errs += 1;
        }
        if r.is_ok() {
            files_ok += 1;
            println!("PASS {:20} ({} subtests)", r.file_name, r.passed);
        } else if r.skipped_zero_subtests && r.failed.is_empty() && r.script_error.is_none() {
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

    println!("================================================================");
    println!(
        "Files:    {}/{} passed ({} skipped/0-subtests, {} failed, {} with script errors)",
        files_ok,
        total_files,
        files_skipped,
        total_files - files_ok - files_skipped,
        script_errs
    );
    println!(
        "Subtests: {}/{} passed ({} failed)",
        sub_passed,
        sub_passed + sub_failed,
        sub_failed
    );
}
