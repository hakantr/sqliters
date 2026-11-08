#[test]
#[ignore = "0/6 subtests passed; pending: VIEW, TRIGGER, ATTACH"]
fn test_with4() {
    sqliters_tcltest::run_test_script("with4", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::testprefix with4
ifcapable {!cte} {
  finish_test
  return
}
do_execsql_test 100 {
  ATTACH ':memory:' AS aux;
  CREATE TABLE main.t1(a,b);
  CREATE TABLE aux.t2(x,y);
  INSERT INTO t1 VALUES(1,2);
  INSERT INTO t2 VALUES(3,4);
} {}
do_catchsql_test 110 {
  CREATE VIEW v1 AS SELECT * FROM t1, aux.t2;
} {1 {view v1 cannot reference objects in database aux}}
do_catchsql_test 120 {
  CREATE VIEW v2 AS WITH v(m,n) AS (SELECT x,y FROM aux.t2) SELECT * FROM t1, v;
} {1 {view v2 cannot reference objects in database aux}}
do_catchsql_test 130 {
  CREATE VIEW v2 AS WITH v(m,n) AS (SELECT 5,?2) SELECT * FROM t1, v;
} {1 {parameters are not allowed in views}}
do_catchsql_test 200 {
  CREATE TRIGGER r1 AFTER INSERT ON t1 BEGIN
     WITH v(m,n) AS (SELECT x,y FROM aux.t2) SELECT * FROM t1, v;
  END;
} {1 {trigger r1 cannot reference objects in database aux}}
do_catchsql_test 210 {
  CREATE TRIGGER r1 AFTER INSERT ON t1 BEGIN
     WITH v(m,n) AS (SELECT 5,?2) SELECT * FROM t1, v;
  END;
} {1 {trigger cannot use variables}}
finish_test
"#);
}
