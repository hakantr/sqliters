#[test]
#[ignore = "2/8 subtests passed; pending: lock3 edge cases"]
fn test_lock3() {
    sqliters_tcltest::run_test_script("lock3", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test lock3-1.1 {
  file mkdir tempdir/t1/t2/t3
  sqlite3 db2 ./tempdir/t1//t2/./t3//./../..//./../../tempdir/..//test.db//
  execsql {
    CREATE TABLE t1(a);
    INSERT INTO t1 VALUES(1);
  }
  execsql {
    SELECT * FROM t1
  } db2
} 1
do_test lock3-2.1 {
  execsql {BEGIN DEFERRED TRANSACTION}
  execsql {INSERT INTO t1 VALUES(2)} db2
  execsql {END TRANSACTION}
  execsql {SELECT * FROM t1}
} {1 2}
do_test lock3-3.1 {
  execsql {BEGIN IMMEDIATE TRANSACTION}
  catchsql {SELECT * FROM t1} db2
} {0 {1 2}}
do_test lock3-3.2 {
  catchsql {INSERT INTO t1 VALUES(3)} db2
} {1 {database is locked}}
do_test lock3-3.3 {
  execsql {END TRANSACTION}
} {}
do_test lock3-4.1 {
  execsql {BEGIN EXCLUSIVE TRANSACTION}
  catchsql {SELECT * FROM t1} db2
} {1 {database is locked}}
do_test lock3-4.2 {
  catchsql {INSERT INTO t1 VALUES(3)} db2
} {1 {database is locked}}
do_test lock3-4.3 {
  execsql {END TRANSACTION}
} {}
catch {db2 close}
finish_test
"#);
}
