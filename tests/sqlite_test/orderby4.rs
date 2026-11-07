#[test]
fn test_orderby4() {
    sqliters_tcltest::run_test_script("orderby4", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::testprefix orderby4
do_execsql_test 1.1 {
  CREATE TABLE t1(a, b, PRIMARY KEY(a,b));
  INSERT INTO t1 VALUES(1,1),(1,2);
  CREATE TABLE t2(x, y, PRIMARY KEY(x,y));
  INSERT INTO t2 VALUES(3,3),(4,4);
  SELECT a, x FROM t1, t2 ORDER BY 1, 2;
} {1 3 1 3 1 4 1 4}
do_execsql_test 1.2 {
  SELECT a, x FROM t1 CROSS JOIN t2 ORDER BY 1, 2;
} {1 3 1 3 1 4 1 4}
do_execsql_test 1.3 {
  SELECT a, x FROM t2 CROSS JOIN t1 ORDER BY 1, 2;
} {1 3 1 3 1 4 1 4}
do_execsql_test 2.1 {
  CREATE TABLE t3(a);
  INSERT INTO t3 VALUES(1),(1);
  CREATE INDEX t3a ON t3(a);
  CREATE TABLE t4(x);
  INSERT INTO t4 VALUES(3),(4);
  CREATE INDEX t4x ON t4(x);
  SELECT a, x FROM t3, t4 ORDER BY 1, 2;
} {1 3 1 3 1 4 1 4}
do_execsql_test 2.2 {
  SELECT a, x FROM t3 CROSS JOIN t4 ORDER BY 1, 2;
} {1 3 1 3 1 4 1 4}
do_execsql_test 2.3 {
  SELECT a, x FROM t4 CROSS JOIN t3 ORDER BY 1, 2;
} {1 3 1 3 1 4 1 4}
finish_test
"#);
}
