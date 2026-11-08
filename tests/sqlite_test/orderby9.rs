#[test]
fn test_orderby9() {
    sqliters_tcltest::run_test_script("orderby9", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::testprefix orderby9
do_execsql_test setup {
  -- create a table with many entries
  CREATE TABLE t1(x);
  WITH RECURSIVE
     c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<100)
  INSERT INTO t1 SELECT x FROM c;
}
proc bigintcompare {a b} {
  set x [expr {$a-$b}]
  if {$x<0} {return -1}
  if {$x>0} {return +1}
  return 0
}
do_test 1.0 {
  set l1 {}
  db eval {SELECT random() AS y FROM t1 ORDER BY 1;} {lappend l1 $y}
  expr {$l1==[lsort -command bigintcompare $l1]}
} {1}
do_test 1.1 {
  set l1 {}
  db eval {SELECT random() AS y FROM t1 ORDER BY random();} {lappend l1 $y}
  expr {$l1==[lsort -command bigintcompare $l1]}
} {1}
do_test 1.2 {
  set l1 {}
  db eval {SELECT random() AS y FROM t1 ORDER BY +random();} {lappend l1 $y}
  expr {$l1==[lsort -command bigintcompare $l1]}
} {0}
finish_test
"#);
}
