//! Integration smoke tests for the `sqliters` CLI binary.

use std::fs;
use std::process::Command;

#[test]
fn test_cli_binary_json_csv_dump_and_sqlite3_compat() {
    let db_path = std::env::temp_dir().join(format!("sqliters_cli_test_{}.db", std::process::id()));
    let _ = fs::remove_file(&db_path);
    let _ = fs::remove_file(format!("{}-journal", db_path.display()));

    let bin = env!("CARGO_BIN_EXE_sqliters");

    let init = Command::new(bin)
        .arg(&db_path)
        .arg("CREATE TABLE cities (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO cities VALUES (1, 'İstanbul'), (2, 'Ankara');")
        .output()
        .expect("run sqliters cli");
    assert!(
        init.status.success(),
        "cli stderr: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let json_out = Command::new(bin)
        .arg("-json")
        .arg(&db_path)
        .arg("SELECT id, name FROM cities ORDER BY id;")
        .output()
        .expect("run sqliters cli -json");
    assert!(json_out.status.success());
    // C SQLite 3.54.1 (`sqlite3 -json`): one row per line, rows separated by "},\n{".
    assert_eq!(
        String::from_utf8_lossy(&json_out.stdout),
        "[{\"id\":1,\"name\":\"İstanbul\"},\n{\"id\":2,\"name\":\"Ankara\"}]\n"
    );

    let csv_out = Command::new(bin)
        .arg("-csv")
        .arg("-header")
        .arg(&db_path)
        .arg("SELECT id, name FROM cities ORDER BY id;")
        .output()
        .expect("run sqliters cli -csv");
    assert!(csv_out.status.success());
    // C SQLite 3.54.1 (`sqlite3 -csv -header`): rows end with CR LF and a text field that
    // holds a non-ASCII byte is quoted.
    assert_eq!(
        String::from_utf8_lossy(&csv_out.stdout),
        "id,name\r\n1,\"İstanbul\"\r\n2,Ankara\r\n"
    );

    let c_check = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg(&db_path)
        .arg("PRAGMA integrity_check;")
        .output()
        .expect("run /usr/bin/sqlite3");
    assert!(c_check.status.success());
    assert_eq!(String::from_utf8_lossy(&c_check.stdout).trim(), "ok");

    let _ = fs::remove_file(&db_path);
    let _ = fs::remove_file(format!("{}-journal", db_path.display()));
}
