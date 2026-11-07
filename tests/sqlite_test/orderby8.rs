#[test]
fn test_orderby8() {
    sqliters_tcltest::run_test_script("orderby8", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::testprefix orderby8
do_test 1.0 {
  db eval {
    CREATE TABLE t1(x);
    INSERT INTO t1(x) VALUES(1),(5),(9),(7),(3),(2),(4),(6),(8);
  }
  set ::result_set "x"
} {x}
for {set i 1} {$i<200} {incr i} {
  append ::result_set ", x+$i"
  do_test 1.$i {
    set res {}
    db eval "SELECT $::result_set FROM t1 ORDER BY x LIMIT -1" {
      lappend res $x
    }
    set res
  } {1 2 3 4 5 6 7 8 9}
}
finish_test
"#);
}
