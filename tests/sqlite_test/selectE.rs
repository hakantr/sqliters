#[test]
fn test_selectE() {
    sqliters_tcltest::run_test_script("selectE", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test selectE-1.0 {
  db eval {
    CREATE TABLE t1(a);
    INSERT INTO t1 VALUES('abc'),('def'),('ghi');
    CREATE TABLE t2(a);
    INSERT INTO t2 VALUES('DEF'),('abc');
    CREATE TABLE t3(a);
    INSERT INTO t3 VALUES('def'),('jkl');

    SELECT a FROM t1 EXCEPT SELECT a FROM t2
     ORDER BY a COLLATE nocase;
  }
} {def ghi}
do_test selectE-1.1 {
  db eval {
    SELECT a FROM t2 EXCEPT SELECT a FROM t3
     ORDER BY a COLLATE nocase;
  }
} {abc DEF}
do_test selectE-1.2 {
  db eval {
    SELECT a FROM t2 EXCEPT SELECT a FROM t3
     ORDER BY a COLLATE binary;
  }
} {DEF abc}
do_test selectE-1.3 {
  db eval {
    SELECT a FROM t2 EXCEPT SELECT a FROM t3
     ORDER BY a;
  }
} {DEF abc}
do_test selectE-2.1 {
  db eval {
    DELETE FROM t2;
    DELETE FROM t3;
    INSERT INTO t2 VALUES('ABC'),('def'),('GHI'),('jkl');
    INSERT INTO t3 SELECT lower(a) FROM t2;
    SELECT a COLLATE nocase FROM t2 EXCEPT SELECT a FROM t3
     ORDER BY 1
  }
} {}
do_test selectE-2.2 {
  db eval {
    SELECT a COLLATE nocase FROM t2 EXCEPT SELECT a FROM t3
     ORDER BY 1 COLLATE binary
  }
} {}
do_catchsql_test selectE-3.1 {
  SELECT 1 EXCEPT SELECT 2 ORDER BY 1 COLLATE nocase EXCEPT SELECT 3;
} {1 {ORDER BY clause should come after EXCEPT not before}}
finish_test
"#);
}
