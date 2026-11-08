#[test]
fn test_unique2() {
    sqliters_tcltest::run_test_script(
        "unique2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach {id sql} {
   1 {CREATE TABLE t1(x TEXT PRIMARY KEY, y NOT NULL) WITHOUT ROWID}
   2 {CREATE TABLE t1(x TEXT PRIMARY KEY, y NOT NULL)}
   3 {CREATE TABLE t1(x TEXT PRIMARY KEY, y) WITHOUT ROWID}
   4 {CREATE TABLE t1(x TEXT PRIMARY KEY, y)}
} {
  do_test $id.1 {
    db eval {DROP TABLE IF EXISTS t1}
    db eval $sql
    db eval {INSERT INTO t1(x,y) VALUES(1,1),(2,2),(3,2),(4,3)}
  } {}
  do_test $id.2 {
    catchsql {CREATE UNIQUE INDEX t1y ON t1(y)}
  } {1 {UNIQUE constraint failed: t1.y}}
}
foreach {id sql} {
   5 {CREATE TABLE t1(w,x,y NOT NULL,z NOT NULL,PRIMARY KEY(w,x)) WITHOUT ROWID}
   6 {CREATE TABLE t1(w,x,y NOT NULL,z NOT NULL,PRIMARY KEY(w,x))}
   7 {CREATE TABLE t1(w,x,y NOT NULL,z,PRIMARY KEY(w,x)) WITHOUT ROWID}
   8 {CREATE TABLE t1(w,x,y NOT NULL,z,PRIMARY KEY(w,x))}
   9 {CREATE TABLE t1(w,x,y,z NOT NULL,PRIMARY KEY(w,x)) WITHOUT ROWID}
  10 {CREATE TABLE t1(w,x,y,z NOT NULL,PRIMARY KEY(w,x))}
  11 {CREATE TABLE t1(w,x,y,z,PRIMARY KEY(w,x)) WITHOUT ROWID}
  12 {CREATE TABLE t1(w,x,y,z,PRIMARY KEY(w,x))}
} {
  do_test $id.1 {
    db eval {DROP TABLE IF EXISTS t1}
    db eval $sql
    db eval {INSERT INTO t1(w,x,y,z) VALUES(1,2,3,4),(2,3,3,4)}
  } {}
  do_test $id.2 {
    catchsql {CREATE UNIQUE INDEX t1yz ON t1(y,z)}
  } {1 {UNIQUE constraint failed: t1.y, t1.z}}
}
do_catchsql_test 13.1 {
  CREATE TABLE err1(a,b,c,UNIQUE(rowid));
} {1 {no such column: rowid}}
do_catchsql_test 13.2 {
  CREATE TABLE err1(a,b,c,PRIMARY KEY(rowid));
} {1 {no such column: rowid}}
finish_test
"#,
    );
}
