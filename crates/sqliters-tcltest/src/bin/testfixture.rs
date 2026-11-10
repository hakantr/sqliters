//! Standalone `testfixture` subprocess binary for multi-process Tcl lock/WAL tests (`lock2.test`, `lock4.test`).
#![forbid(unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sqliters_tcltest::SqliteTestHost;
use sqliters_tcltest::interp::{TclControl, TclInterp};
use sqliters_tcltest::list::format_list;

fn resolve_repo_root(start: &Path) -> PathBuf {
    let mut cur = start.to_path_buf();
    for _ in 0..6 {
        if cur.join("sqlite/test").is_dir() {
            return cur;
        }
        if !cur.pop() {
            break;
        }
    }
    start.to_path_buf()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        std::process::exit(0);
    }
    let script_arg = &args[1];
    let extra_args = &args[2..];

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.clone());
    let repo_root = resolve_repo_root(&exe_dir);
    let sqlite_test_dir = repo_root.join("sqlite/test");
    let source_roots = vec![
        sqlite_test_dir.clone(),
        repo_root.join("sqlite/ext/fts5/test"),
        repo_root.join("sqlite/ext/rtree"),
    ];

    let script_path = if Path::new(script_arg).is_absolute() {
        PathBuf::from(script_arg)
    } else {
        cwd.join(script_arg)
    };
    let script_stem = script_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "testfixture".to_string());

    let script_content = match fs::read_to_string(&script_path) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "cannot read {script_arg}: {e}");
            std::process::exit(1);
        }
    };

    let host = SqliteTestHost::new_shared_with_roots(&script_stem, sqlite_test_dir, source_roots);
    let mut interp = TclInterp::new(cwd, Some(host.clone()));
    interp.subprocess_mode = true;
    interp.script_path = script_arg.clone();
    let _ = interp.set_var("argv0", script_arg.clone());
    let _ = interp.set_var("argv", format_list(extra_args));
    let _ = interp.set_var("argc", extra_args.len().to_string());

    let exit_code = match interp.eval(&script_content) {
        Ok(_) | Err(TclControl::FinishTest) | Err(TclControl::Return { .. }) => 0,
        Err(ctrl) => {
            let _ = writeln!(std::io::stderr(), "{}", ctrl.err_msg());
            1
        }
    };

    drop(interp);
    sqliters_tcltest::harness::close_all_host_connections(&host);
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(exit_code);
}
