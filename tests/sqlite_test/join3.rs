#[test]
fn test_join3() {
    sqliters_tcltest::run_test_script(
        "join3",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
catch {unset ::result}
set result {}
for {set N 1} {$N<=$bitmask_size} {incr N} {
  lappend result $N
  do_test join3-1.$N {
    execsql "CREATE TABLE t${N}(x);"
    execsql "INSERT INTO t$N VALUES($N)"
    set sql "SELECT * FROM t1"
    for {set i 2} {$i<=$N} {incr i} {append sql ", t$i"}
    execsql $sql
  } $result
}
set result {}
for {set N 1} {$N<=$bitmask_size} {incr N} {
  lappend result $N
  do_test join3-2.$N {
    set sql "SELECT * FROM t1"
    for {set i 2} {$i<=$N} {incr i} {append sql ", t$i"}
    set sep WHERE
    for {set i 1} {$i<$N} {incr i} {
      append sql " $sep t[expr {$i+1}].x==t$i.x+1"
      set sep AND
    }
    execsql $sql
  } $result
}
do_test join3-3.1 {
  set sql "SELECT * FROM t1 AS t0, t1"
  for {set i 2} {$i<=$bitmask_size} {incr i} {append sql ", t$i"}
  catchsql $sql
} [list 1 "at most $bitmask_size tables in a join"]
finish_test
"#,
    );
}
