#[test]
fn test_expr2() {
    sqliters_tcltest::run_test_script(
        "expr2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix expr2
do_execsql_test 1.0 {
  CREATE TABLE t0(c0);
  INSERT INTO t0(c0) VALUES ('val');
}
do_execsql_test 1.1 {
  SELECT * FROM t0 WHERE (
      ( (0 IS NOT FALSE) OR NOT (0 IS FALSE OR (t0.c0 = 1)) ) IS 0
  )
} {val}
do_execsql_test 1.2.1 {
  SELECT 
      ( (0 IS NOT FALSE) OR NOT (0 IS FALSE OR (t0.c0 = 1)) ) IS 0
  FROM t0 
} {1}
do_execsql_test 1.2.2 {
  SELECT 
      ( (0 IS NOT FALSE) OR NOT (0 IS 0 OR (t0.c0 = 1)) ) IS 0
  FROM t0 
} {1}
do_execsql_test 1.3 {
  SELECT ( (0 IS NOT FALSE) OR NOT (0 IS FALSE OR (t0.c0 = 1)) ) FROM t0 
} {0}
do_execsql_test 1.4.1 {
  SELECT (0 IS NOT FALSE) FROM t0 
} {0}
do_execsql_test 1.4.2 {
  SELECT NOT (0 IS FALSE OR (t0.c0 = 1)) FROM t0 
} {0}
finish_test
"#,
    );
}
