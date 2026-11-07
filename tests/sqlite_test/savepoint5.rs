#[test]
fn test_savepoint5() {
    sqliters_tcltest::run_test_script("savepoint5", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test savepoint5-1.1 {
  db eval {
    SAVEPOINT sp1;
    CREATE TABLE t1(x);
    INSERT INTO t1 VALUES(1);
    SELECT count(*) FROM sqlite_master;
    SELECT * FROM t1;
  }
} {1 1}
do_test savepoint5-1.2 {
  db eval {
    ROLLBACK TO sp1;
    SELECT count(*) FROM sqlite_master;
  }
} {0}
do_test savepoint5-1.3 {
  db eval {
    CREATE TABLE t1(x);
    INSERT INTO t1 VALUES(1);
    SELECT count(*) FROM sqlite_master;
    SELECT * FROM t1;
  }
} {1 1}
finish_test
"#);
}
