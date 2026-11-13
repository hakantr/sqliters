//! Integration tests for `sqliters` CLI compatibility with SQLite's `shell.c` batch, piped,
//! `.read`, `.import`, `.bail`, `.print`, and multiline CRLF SQL behavior.
#![forbid(unsafe_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static CLI_TEST_SEQ: AtomicU64 = AtomicU64::new(1);

struct TempDirGuard {
    dir: PathBuf,
}

impl TempDirGuard {
    fn new(tag: &str) -> Self {
        let id = CLI_TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sqliters-cli-test-{tag}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_sqliters")
}

#[test]
fn test_multiline_sql_preserves_embedded_crlf_and_comments() {
    let tmp = TempDirGuard::new("crlf");
    let db = tmp.path("crlf.db");

    // Input script uses CRLF line endings and embeds both CRLF and a line starting with `.`
    // inside a multiline SQL string literal, plus standalone `--` and `/*...*/` comments before dot commands.
    let stdin_payload = concat!(
        "-- leading comment before dot command\r\n",
        "/* multiline\r\n",
        "   block comment */\r\n",
        ".bail on\r\n",
        ".mode list\r\n",
        ".headers off\r\n",
        "CREATE TABLE t(id INT PRIMARY KEY, body TEXT);\r\n",
        "INSERT INTO t VALUES (1, 'first\r\n",
        ".not_a_dot_cmd\r\n",
        "third');\r\n",
        "SELECT lower(hex(body)) FROM t WHERE id = 1;\r\n",
    );

    let mut child = Command::new(bin_path())
        .arg(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());

    // C SQLite 3.54.1: the shell reads its input line by line and drops a CR in front of
    // the LF of every line (`local_getline`), also inside a multi-line string literal.
    let expected_hex: String = b"first\n.not_a_dot_cmd\nthird"
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), expected_hex);
}

#[test]
fn test_rfc4180_import_nested_read_quoted_args_and_auto_create() {
    let tmp = TempDirGuard::new("import");
    let db = tmp.path("import.db");
    let csv_file = tmp.path("rows with spaces.csv");
    let inner_script = tmp.path("inner setup.sql");
    let outer_script = tmp.path("outer driver.sql");

    let csv_bytes = b"id,name,note,tail\r\n\
1,\"a\"\"quote\",plain,end1\r\n\
2,\"line\nbreak\",\"CR\r\nLF\",end2\r\n\
3,,\\N,\r\n\
4,\xc4\xb0stanbul \xe6\x9d\xb1\xe4\xba\xac,\"comma,val\",ok\r\n";
    fs::write(&csv_file, csv_bytes).unwrap();

    fs::write(
        &inner_script,
        "CREATE TABLE stage(id TEXT, name TEXT, note TEXT, tail TEXT);\n.print INNER_OK\n",
    )
    .unwrap();

    let outer_content = format!(
        ".bail on\n\
         .mode list\n\
         .headers off\n\
         .read \"{}\"\n\
         .import --csv --skip 1 \"{}\" stage\n\
         .import \"{}\" auto_tbl --csv\n\
         SELECT id || '|' || lower(hex(name)) || '|' || lower(hex(note)) || '|' || lower(hex(tail)) FROM stage ORDER BY CAST(id AS INT);\n\
         SELECT count(*) FROM auto_tbl;\n",
        inner_script.display(),
        csv_file.display(),
        csv_file.display(),
    );
    fs::write(&outer_script, outer_content).unwrap();

    let mut child = Command::new(bin_path())
        .arg(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let cmd = format!(".read '{}'\n", outer_script.display());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(cmd.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());

    let lines: Vec<&str> = std::str::from_utf8(&out.stdout).unwrap().lines().collect();
    assert_eq!(
        lines,
        vec![
            "INNER_OK",
            "1|612271756f7465|706c61696e|656e6431",
            "2|6c696e650a627265616b|43520d0a4c46|656e6432",
            "3||5c4e|",
            "4|c4b07374616e62756c20e69db1e4baac|636f6d6d612c76616c|6f6b",
            "4",
        ]
    );
}

#[test]
fn test_persistent_pipe_immediate_flush_and_bail() {
    let tmp = TempDirGuard::new("pipe");
    let db = tmp.path("pipe.db");

    let mut child = Command::new(bin_path())
        .arg(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    stdin
        .write_all(
            b".bail on\n.mode list\n.headers off\nPRAGMA journal_mode=WAL;\n.print SQLTEST_READY\n",
        )
        .unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    let mut preflight = Vec::new();
    loop {
        line.clear();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "EOF before SQLTEST_READY");
        let trimmed = line.trim().to_owned();
        if trimmed == "SQLTEST_READY" {
            break;
        }
        preflight.push(trimmed);
    }
    assert_eq!(preflight, vec!["wal"]);

    stdin
        .write_all(
            b"CREATE TABLE items(x INT);\nINSERT INTO items VALUES (10),(20),(30);\nSELECT sum(x) FROM items;\n.print SQLTEST_DONE\n",
        )
        .unwrap();
    stdin.flush().unwrap();

    let mut body = Vec::new();
    loop {
        line.clear();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "EOF before SQLTEST_DONE");
        let trimmed = line.trim().to_owned();
        if trimmed == "SQLTEST_DONE" {
            break;
        }
        body.push(trimmed);
    }
    assert_eq!(body, vec!["60"]);

    stdin.write_all(b".quit\n").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let status = child.wait().unwrap();
    assert!(status.success());

    // Also verify `.bail on` immediately aborts with non-zero exit code and does not execute subsequent lines.
    let mut bad = Command::new(bin_path())
        .arg(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    bad.stdin
        .take()
        .unwrap()
        .write_all(b".bail on\nSELECT * FROM nonexistent_table;\n.print SHOULD_NOT_RUN\n")
        .unwrap();
    let bad_out = bad.wait_with_output().unwrap();
    assert!(!bad_out.status.success());
    assert!(!bad_out.stderr.is_empty());
    assert!(
        !String::from_utf8_lossy(&bad_out.stdout).contains("SHOULD_NOT_RUN"),
        "bail did not stop execution"
    );
}
