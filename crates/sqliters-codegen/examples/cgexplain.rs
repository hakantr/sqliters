//! EXPLAIN through the ported code generator only (no engine). Reads statements from stdin,
//! one per line (`\n` inside a statement written as backslash-n; `--setup:` lines ignored),
//! and prints for each the same text as the C oracle `target/perf-work/v1/cdump/cexplain`:
//!
//! ```text
//! === <lineno>
//! addr|opcode|p1|p2|p3|p4|p5|comment
//! ```
//!
//! or `ERR <offset> <message>`, or `UNSUPPORTED <first missing piece>`.
use std::io::{BufRead, Write};

use sqliters_codegen::{Db, NoHost, PrepareResult, explain_bytes, prepare};

fn main() {
    // `--tree`: print the parse-tree dumps (see `sqliters_codegen::treedump`) instead of the listing
    let tree = std::env::args().any(|a| a == "--tree");
    let stdin = std::io::stdin();
    let out = std::io::stdout();
    let mut out = out.lock();
    for (i, line) in stdin.lock().lines().enumerate() {
        let Ok(line) = line else { break };
        let sql = line.replace("\\n", "\n");
        if sql.starts_with("--setup:") {
            continue;
        }
        let _ = writeln!(out, "=== {}", i + 1);
        if tree {
            let (r, _, dump) = sqliters_codegen::prepare_with_tree_dump(Db::new(&NoHost), &sql, true);
            for l in dump {
                let _ = writeln!(out, "{l}");
            }
            if let PrepareResult::Unsupported(why) = r {
                let _ = writeln!(out, "UNSUPPORTED {why}");
            }
            continue;
        }
        match prepare(Db::new(&NoHost), &sql) {
            PrepareResult::Program(prog) => {
                for l in explain_bytes(&prog) {
                    let _ = out.write_all(&l);
                    let _ = out.write_all(b"\n");
                }
            }
            PrepareResult::Error { message, offset, .. } => {
                let _ = writeln!(out, "ERR {offset} {message}");
            }
            PrepareResult::Unsupported(why) => {
                let _ = writeln!(out, "UNSUPPORTED {why}");
            }
        }
    }
}
