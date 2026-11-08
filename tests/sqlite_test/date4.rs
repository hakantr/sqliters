#[test]
fn test_date4() {
    sqliters_tcltest::run_test_script(
        "date4",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable {!datetime} {
  finish_test
  return
}
if {$tcl_platform(os)=="Linux"} {
  if {"" eq [strftime {%P} 1]} {
    set FMT {%d,%e,%F,%H,%I,%j,%m,%M,%u,%w,%W,%Y,%%,%p,%U,%V,%G,%g}
  } else {
    set FMT {%d,%e,%F,%H,%k,%I,%l,%j,%m,%M,%u,%w,%W,%Y,%%,%P,%p,%U,%V,%G,%g}
  }
} else {
  set FMT {%d,%e,%F,%H,%I,%j,%p,%R,%u,%w,%W,%%}
}
for {set i 0} {$i<=24858} {incr i} {
  set TS [expr {$i*86390}]
  do_execsql_test date4-$i {
    SELECT strftime($::FMT,$::TS,'unixepoch');
  } [list [strftime $FMT $TS]]
}
finish_test
"#,
    );
}
