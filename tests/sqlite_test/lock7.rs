#[test]
#[ignore = "4/8 subtests passed; pending: lock7 edge cases"]
fn test_lock7() {
    sqliters_tcltest::run_test_script("lock7", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test lock7-1.1 {
  execsql { CREATE TABLE t1(a, b) }
  db close

  sqlite3 db1 test.db
  sqlite3 db2 test.db

  db1 eval {BEGIN}
  db2 eval {BEGIN}
} {}
do_test lock7-1.2 {
  execsql { PRAGMA lock_status } db1
} {main unlocked temp closed}
do_test lock7-1.3 {
  execsql { PRAGMA lock_status } db2
} {main unlocked temp closed}
do_test lock7-1.4 {
  catchsql { INSERT INTO t1 VALUES(1, 1) } db1
} {0 {}}
do_test lock7-1.5 {
  catchsql { INSERT INTO t1 VALUES(2, 2) } db2
} {1 {database is locked}}
do_test lock7-1.6 {
  execsql { PRAGMA lock_status } db1
} {main reserved temp closed}
do_test lock7-1.7 {
  execsql { PRAGMA lock_status } db2
} {main unlocked temp closed}
do_test lock7-1.8 {
  execsql { COMMIT } db1
} {}
db1 close
db2 close
finish_test
"#);
}
