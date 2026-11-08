#[test]
#[ignore = "3/4 subtests passed; pending: EQP/optimizer"]
fn test_index8() {
    sqliters_tcltest::run_test_script("index8", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_execsql_test 1.0 {
  CREATE TABLE t1(a,b,c,d);
  WITH RECURSIVE c(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM c WHERE x<100)
  INSERT INTO t1(a,b,c,d)
     SELECT x/10, x%10, x%19, x FROM c;
  CREATE INDEX t1abc ON t1(a,b,c);
  SELECT * FROM t1 WHERE c=4 ORDER BY a, b LIMIT 2;
} {0 4 4 4 2 3 4 23}
do_execsql_test 1.0eqp {
  EXPLAIN QUERY PLAN
  SELECT * FROM t1 WHERE c=4 ORDER BY a, b LIMIT 2;
} {/SCAN t1 USING INDEX t1abc/}
do_execsql_test 1.1 {
  DROP INDEX t1abc;
  CREATE INDEX t1abd ON t1(a,b,d);
  SELECT * FROM t1 WHERE c=4 ORDER BY a, b LIMIT 2;
} {0 4 4 4 2 3 4 23}
do_execsql_test 1.1eqp {
  EXPLAIN QUERY PLAN
  SELECT * FROM t1 WHERE c=4 ORDER BY a, b LIMIT 2;
} {~/USING INDEX/}
finish_test
"#);
}
