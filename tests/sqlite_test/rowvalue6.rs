#[test]
fn test_rowvalue6() {
    sqliters_tcltest::run_test_script(
        "rowvalue6",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::testprefix rowvalue6
do_execsql_test 1.1 {
  CREATE TABLE t1(a,b,c);
  CREATE INDEX t1x1 ON t1(a,b);
  INSERT INTO t1 VALUES(1,NULL,200);

  CREATE TABLE t2(x,y,z);
  INSERT INTO t2 VALUES(1,NULL,55);

  SELECT c FROM t1 WHERE (a,b) IN (SELECT x,y FROM t2 WHERE z==55);
} {}
do_execsql_test 1.2 {
  INSERT INTO t1 VALUES(2,3,400);
  INSERT INTO t2 VALUES(2,3,55);  

  SELECT c FROM t1 WHERE (a,b) IN (SELECT x,y FROM t2 WHERE z==55);
} {400}
finish_test
"#,
    );
}
