#[test]
fn test_in2() {
    sqliters_tcltest::run_test_script(
        "in2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test in2-1 {
  execsql {
    CREATE TABLE a(i INTEGER PRIMARY KEY, a);
  }
} {}
set ::N 2000
do_test in2-2 {
  db transaction {
    for {set ::ii 0} {$::ii < $::N} {incr ::ii} {
      execsql {INSERT INTO a VALUES($::ii, $::ii)}
    }
    execsql {INSERT INTO a VALUES(4000, '')}

    for {set ::ii 0} {$::ii < $::N} {incr ::ii} {
      set ::t [format "x%04d" $ii]
      execsql {INSERT INTO a VALUES(NULL, $::t)}
    }
  }
} {}
for {set ::ii 3} {$::ii < $::N} {incr ::ii} {
  do_test in2-$::ii {
    execsql {
      SELECT 1 IN (SELECT a FROM a WHERE (i < $::ii) OR (i >= $::N))
    }
  } {1}
}
finish_test
"#,
    );
}
