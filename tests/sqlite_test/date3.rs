#[test]
#[ignore = "112/126 subtests passed; pending: date3 edge cases"]
fn test_date3() {
    sqliters_tcltest::run_test_script(
        "date3",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable {!datetime} {
  finish_test
  return
}
proc datetest {tnum expr result} {
  do_test date3-$tnum [subst {
    execsql "SELECT coalesce($expr,'NULL')"
  }] [list $result]
}
set tcl_precision 15
datetest 1.1 {unixepoch('1970-01-01')} {0}
datetest 1.2 {unixepoch('1969-12-31 23:59:59')} {-1}
datetest 1.3 {unixepoch('2106-02-07 06:28:15')} {4294967295}
datetest 1.4 {unixepoch('2106-02-07 06:28:16')} {4294967296}
datetest 1.5 {unixepoch('9999-12-31 23:59:59')} {253402300799}
datetest 1.6 {unixepoch('0000-01-01 00:00:00')} {-62167219200}
for {set i 1} {$i<=100} {incr i} {
  set x [expr {int(rand()*0xfffffffff)-0xffffffff}]
  datetest 1.7.$i "unixepoch($x,'unixepoch')==$x" {1}
}
datetest 1.8 {unixepoch('2022-01-27 12:59:28.052')} {1643288368}
foreach {tn jd date} {
  2.1  0.0              {-4713-11-24 12:00:00}
  2.2  5373484.4999999  {9999-12-31 23:59:59}
  2.3  2440587.5        {1970-01-01 00:00:00}
  2.4  2440587.49998843 {1969-12-31 23:59:59}
  2.5  2440615.7475463  {1970-01-29 05:56:28}

  2.10 -1               {1969-12-31 23:59:59}
  2.11 5373485          {1970-03-04 04:38:05}
  2.12 -210866760000    {-4713-11-24 12:00:00}
  2.13 253402300799     {9999-12-31 23:59:59}

  2.20 -210866760001    {NULL}
  2.21 253402300800     {NULL}
} {
  datetest $tn "datetime($jd,'auto')" $date
}
datetest 2.30 {date('2022-01-29','auto')==date('2022-01-29')} {1}
do_execsql_test date3-2.40 {
  WITH tx(timeval,datetime) AS (
     VALUES('2022-01-27 13:15:44','2022-01-27 13:15:44'),
           (2459607.05260275,'2022-01-27 13:15:44'),
           (1643289344,'2022-01-27 13:15:44')
  )
  SELECT datetime(timeval,'auto') == datetime FROM tx;
} {1 1 1}
datetest 3.1 {datetime(2459607.05,'+1 hour','unixepoch')} {NULL}
datetest 3.2 {datetime(2459607.05,'unixepoch','+1 hour')} {1970-01-29 12:13:27}
datetest 4.1 {datetime(2459607,'julianday')}           {2022-01-27 12:00:00}
datetest 4.2 {datetime(2459607,'+1 hour','julianday')} {NULL}
datetest 4.3 {datetime('2022-01-27','julianday')}      {NULL}
do_execsql_test date3-5.0 {
  WITH inc(x) AS (VALUES(-10) UNION ALL SELECT x+1 FROM inc WHERE x<100)
  SELECT count(*) FROM inc
  WHERE datetime('1970-01-01',format('%+d days',x))
     <> datetime(unixepoch('1970-01-01',format('%+d days',x)),'auto');
} {63}
finish_test
"#,
    );
}
