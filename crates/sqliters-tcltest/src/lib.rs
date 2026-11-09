//! `sqliters-tcltest`: Pure safe Rust SQLite `testfixture` / TCL test suite runner.
#![forbid(unsafe_code)]

pub mod expr;
pub mod harness;
pub mod interp;
pub mod list;
pub mod regex;

use std::fs;
use std::path::{Path, PathBuf};

pub use harness::{
    PHASE1_CAPABILITY_TABLE, SqliteTestHost, SubtestFailure, TESTFIXTURE_DEFAULT_PAGE_SIZE,
    TempTestDir, TestFileReport, phase1_capability, sqlite_err_name,
};
use interp::{TclControl, TclInterp};

/// Evaluates an adapted SQLite `.test` script from `sqlite/test` in an isolated temporary
/// directory and returns its detailed [`TestFileReport`] without panicking on subtest failures.
pub fn evaluate_test_script(test_name: &str, script: &str) -> TestFileReport {
    evaluate_test_script_in_root("sqlite/test", test_name, script)
}

/// Evaluates a SQLite `.test` script originating from `source_root_rel` (e.g. `"sqlite/test"`,
/// `"sqlite/ext/fts5/test"`, or `"sqlite/ext/rtree"`) in an isolated temporary working directory.
pub fn evaluate_test_script_in_root(
    source_root_rel: &str,
    test_name: &str,
    script: &str,
) -> TestFileReport {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let repo_root = resolve_repo_root(Path::new(&manifest_dir));
    let script_source_dir = repo_root.join(source_root_rel);
    let source_roots = collect_source_roots(&repo_root, &script_source_dir);
    let temp = TempTestDir::new(test_name);

    let host = SqliteTestHost::new_shared_with_roots(test_name, script_source_dir, source_roots);
    let mut interp = TclInterp::new(temp.path.clone(), Some(host.clone()));
    interp.script_path = format!("{test_name}.test");

    if let Err(ctrl) = harness::bootstrap_tester(&mut interp, &host, &temp.path) {
        interp.record_script_error(ctrl.err_msg());
    } else {
        match interp.eval(script) {
            Ok(_) | Err(TclControl::FinishTest) | Err(TclControl::Return { .. }) => {}
            Err(err) => interp.record_script_error(err.err_msg()),
        }
    }
    drop(interp);

    host.borrow().report.clone()
}

/// Runs an adapted SQLite `.test` script from `sqlite/test` and asserts that **every** subtest
/// passes with zero script errors. Panics with detailed diagnostics on any mismatch.
pub fn run_test_script(test_name: &str, script: &str) {
    run_test_script_in_root("sqlite/test", test_name, script);
}

/// Runs a SQLite `.test` script originating from `source_root_rel` and asserts that **every**
/// subtest passes with zero script errors.
pub fn run_test_script_in_root(source_root_rel: &str, test_name: &str, script: &str) {
    let report = evaluate_test_script_in_root(source_root_rel, test_name, script);
    if !report.is_ok() {
        let mut msg = format!(
            "\nSQLite test suite '{test_name}' failed ({} passed, {} failed):\n",
            report.passed,
            report.failed.len()
        );
        for (i, f) in report.failed.iter().take(20).enumerate() {
            use std::fmt::Write;
            let _ = writeln!(
                &mut msg,
                "  {}. [{}]\n     expected: [{}]\n     got:      [{}]",
                i + 1,
                f.name,
                f.expected,
                f.actual
            );
        }
        if report.failed.len() > 20 {
            use std::fmt::Write;
            let _ = writeln!(
                &mut msg,
                "  ... and {} more failing subtests",
                report.failed.len() - 20
            );
        }
        if report.passed == 0 && report.failed.is_empty() && report.script_error.is_none() {
            use std::fmt::Write;
            let _ = writeln!(
                &mut msg,
                "  Zero subtests executed (early finish_test / capability skip cannot pass an active test)!"
            );
        }
        if !report.unknown_capabilities.is_empty() {
            use std::fmt::Write;
            let _ = writeln!(
                &mut msg,
                "  Unknown capabilities queried: {:?}",
                report.unknown_capabilities
            );
        }
        if let Some(ref serr) = report.script_error {
            use std::fmt::Write;
            let _ = writeln!(&mut msg, "  Script error: {serr}");
        }
        panic!("{msg}");
    }
}

fn resolve_repo_root(base: &Path) -> PathBuf {
    if base.join("sqlite/test").is_dir() {
        return fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    }
    let up2 = base.join("../..");
    if up2.join("sqlite/test").is_dir() {
        return fs::canonicalize(&up2).unwrap_or(up2);
    }
    fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf())
}

fn collect_source_roots(repo_root: &Path, primary: &Path) -> Vec<PathBuf> {
    let mut roots = vec![primary.to_path_buf()];
    for rel in ["sqlite/test", "sqlite/ext/fts5/test", "sqlite/ext/rtree"] {
        let candidate = repo_root.join(rel);
        if candidate.is_dir() && !roots.iter().any(|r| r == &candidate) {
            roots.push(candidate);
        }
    }
    roots
}

/// Strips standalone TCL `#` comment lines and `/* ... */` header comment blocks
/// while preserving all executable test logic, multiline SQL strings, and regexes.
pub fn strip_test_comments(raw_tcl: &str) -> String {
    let mut out = String::with_capacity(raw_tcl.len());
    let mut brace_depth = 0i32;
    let mut in_quotes = false;
    let mut in_block_comment = false;
    let mut prev_backslash_cont = false;

    for line in raw_tcl.lines() {
        let trimmed = line.trim();

        if in_block_comment {
            if let Some(end_idx) = trimmed.find("*/") {
                in_block_comment = false;
                let rest = trimmed[end_idx + 2..].trim();
                if rest.is_empty() {
                    continue;
                }
            } else {
                continue;
            }
        }

        if !in_quotes
            && !prev_backslash_cont
            && trimmed.starts_with("/*")
            && !trimmed.starts_with("/* QUERY PLAN")
            && !trimmed.starts_with("/* *")
        {
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            continue;
        }

        if brace_depth == 0 && !in_quotes && !prev_backslash_cont {
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('#') {
                prev_backslash_cont = trimmed.ends_with('\\');
                continue;
            }
        } else if !in_quotes && !prev_backslash_cont && trimmed.starts_with('#') {
            // Inside a braced proc/do_test/if block (not inside quotes), check if
            // we are inside a SQL single-quoted string or pure TCL block.
            // Only strip if the line has no SQL single quotes and is a pure `#` comment.
            if !trimmed.contains('\'') && !trimmed.contains('/') {
                prev_backslash_cont = trimmed.ends_with('\\');
                continue;
            }
        }

        // Update brace_depth and quote state across this line
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if !in_quotes {
                if bytes[i] == b'{' {
                    brace_depth += 1;
                } else if bytes[i] == b'}' {
                    brace_depth = (brace_depth - 1).max(0);
                } else if bytes[i] == b'"' && brace_depth == 0 {
                    in_quotes = true;
                }
            } else if bytes[i] == b'"' {
                in_quotes = false;
            }
            i += 1;
        }
        prev_backslash_cont = trimmed.ends_with('\\');

        out.push_str(line);
        out.push('\n');
    }
    out
}
