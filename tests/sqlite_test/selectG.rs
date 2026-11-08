#[test]
fn test_selectG() {
    sqliters_tcltest::run_test_script(
        "selectG",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix selectG
do_test 100 {
  set sql "CREATE TABLE t1(x);\nINSERT INTO t1(x) VALUES"
  for {set i 1} {$i<100000} {incr i} {
    append sql "($i),"
  }
  append sql "($i);"
  set microsec [lindex [time {db eval $sql}] 0]
  db eval {
    SELECT count(x), sum(x), avg(x), $microsec<10000000 FROM t1;
  }
} {100000 5000050000 50000.5 1}
do_test 110 {
  set sql "SELECT (VALUES"
  for {set i 1} {$i<100000} {incr i} {
    append sql "($i),"
  }
  append sql "($i));"
  db eval $sql
} {1}
do_test 120 {
  set n [llength [split [db eval "explain $sql"] \n]]
  expr {$n<10}
} {1}
finish_test
"#,
    );
}
