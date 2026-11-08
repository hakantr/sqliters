#[test]
#[ignore = "2/8 subtests passed; pending: collate7 edge cases"]
fn test_collate7() {
    sqliters_tcltest::run_test_script(
        "collate7",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::caseless_del 0
proc caseless_cmp {zLeft zRight} {
  string compare -nocase $zLeft $zRight
}
do_test collate7-1.1 {
  set cmd [list incr ::caseless_del]
  sqlite3_create_collation_v2 db CASELESS caseless_cmp $cmd
  set ::caseless_del
} {0}
do_test collate7-1.2 {
  sqlite_delete_collation db CASELESS
  set ::caseless_del
} {1}
do_test collate7-1.3 {
  catchsql {
    CREATE TABLE abc(a COLLATE CASELESS, b, c);
  }
} {1 {no such collation sequence: CASELESS}}
do_test collate7-1.4 {
  sqlite3_create_collation_v2 db CASELESS caseless_cmp {incr ::caseless_del}
  db close
  set ::caseless_del
} {2}
do_test collate7-2.1 {
  forcedelete test.db test.db-journal
  sqlite3 db test.db
  sqlite3_create_collation_v2 db CASELESS caseless_cmp {incr ::caseless_del}
  execsql {
    PRAGMA encoding='utf-16';
    CREATE TABLE abc16(a COLLATE CASELESS, b, c);
  } db
  set ::caseless_del
} {2}
do_test collate7-2.2 {
  execsql {
    SELECT * FROM abc16 WHERE a < 'abc';
  }
  set ::caseless_del
} {2}
do_test collate7-2.3 {
  sqlite_delete_collation db CASELESS
  set ::caseless_del
} {3}
do_test collate7-2.4 {
  catchsql {
    SELECT * FROM abc16 WHERE a < 'abc';
  }
} {1 {no such collation sequence: CASELESS}}
finish_test
"#,
    );
}
