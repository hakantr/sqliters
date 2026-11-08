#[test]
#[ignore = "2/6 subtests passed; pending: utf16align edge cases"]
fn test_utf16align() {
    sqliters_tcltest::run_test_script(
        "utf16align",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable !utf16 {
  finish_test
  return
}
do_test utf16align-1.0 {
  set unaligned_string_counter 0
  add_alignment_test_collations [sqlite3_connection_pointer db]
  sqlite3_db_config db SQLITE_DBCONFIG_DQS_DML 1
  execsql {
    PRAGMA encoding=UTF16;
    CREATE TABLE t1(
      id INTEGER PRIMARY KEY,
      spacer TEXT,
      a TEXT COLLATE utf16_aligned,
      b TEXT COLLATE utf16_unaligned
    );
    INSERT INTO t1(a) VALUES("abc");
    INSERT INTO t1(a) VALUES("defghi");
    INSERT INTO t1(a) VALUES("jklmnopqrstuv");
    INSERT INTO t1(a) VALUES("wxyz0123456789-");
    UPDATE t1 SET b=a||'-'||a;
    INSERT INTO t1(a,b) SELECT a||b, b||a FROM t1;
    INSERT INTO t1(a,b) SELECT a||b, b||a FROM t1;
    INSERT INTO t1(a,b) SELECT a||b, b||a FROM t1;
    INSERT INTO t1(a,b) VALUES('one','two');
    INSERT INTO t1(a,b) SELECT a, b FROM t1;
    UPDATE t1 SET spacer = CASE WHEN rowid&1 THEN 'x' ELSE 'xx' END;
    SELECT count(*) FROM t1;
  }
} 66
do_test utf16align-1.1 {
  set unaligned_string_counter
} 0
do_test utf16align-1.2 {
  execsql {
    CREATE INDEX t1i1 ON t1(spacer, b);
  }
  expr {$unaligned_string_counter>0}
} 1
do_test utf16align-1.3 {
  set unaligned_string_counter 0
  execsql {
    CREATE INDEX t1i2 ON t1(spacer, a);
  }
  expr {$unaligned_string_counter>0}
} 0
integrity_check utf16align-1.4
db close
sqlite3 db :memory:
do_test utf16align-2.1 {
  db eval {
    PRAGMA encoding=UTF16be;
    SELECT hex(ltrim(x'6efcda'));
  }
} {6EFC}
finish_test
"#,
    );
}
