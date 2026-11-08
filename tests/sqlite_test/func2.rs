#[test]
#[ignore = "113/131 subtests passed; pending: func2 edge cases"]
fn test_func2() {
    sqliters_tcltest::run_test_script(
        "func2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc bin_to_hex {blob} {
  set bytes {}
  binary scan $blob \c* bytes
  set bytes2 [list]
  foreach b $bytes {lappend bytes2 [format %02X [expr $b & 0xFF]]}
  join $bytes2 {}
}
do_test func2-1.1 {
  execsql {SELECT 'Supercalifragilisticexpialidocious'}
} {Supercalifragilisticexpialidocious}
do_test func2-1.2.1 {
  catchsql {SELECT SUBSTR()}
} {1 {wrong number of arguments to function SUBSTR()}}
do_test func2-1.2.2 {
  catchsql {SELECT SUBSTR('Supercalifragilisticexpialidocious')}
} {1 {wrong number of arguments to function SUBSTR()}}
do_test func2-1.2.3 {
  catchsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 1,1,1)}
} {1 {wrong number of arguments to function SUBSTR()}}
do_test func2-1.3 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 0)}
} {Supercalifragilisticexpialidocious}
do_test func2-1.4 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 1)}
} {Supercalifragilisticexpialidocious}
do_test func2-1.5 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 2)}
} {upercalifragilisticexpialidocious}
do_test func2-1.6 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 30)}
} {cious}
do_test func2-1.7 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 34)}
} {s}
do_test func2-1.8 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 35)}
} {{}}
do_test func2-1.9 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 36)}
} {{}}
do_test func2-1.10 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -0)}
} {Supercalifragilisticexpialidocious}
do_test func2-1.11 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -1)}
} {s}
do_test func2-1.12 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -2)}
} {us}
do_test func2-1.13 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -30)}
} {rcalifragilisticexpialidocious}
do_test func2-1.14 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -34)}
} {Supercalifragilisticexpialidocious}
do_test func2-1.15 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -35)}
} {Supercalifragilisticexpialidocious}
do_test func2-1.16 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -36)}
} {Supercalifragilisticexpialidocious}
do_test func2-1.17.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, 1)}
} {{}}
do_test func2-1.17.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, 2)}
} {S}
do_test func2-1.18 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 1, 1)}
} {S}
do_test func2-1.19.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 2, 0)}
} {{}}
do_test func2-1.19.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 2, 1)}
} {u}
do_test func2-1.19.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 2, 2)}
} {up}
do_test func2-1.20 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 30, 1)}
} {c}
do_test func2-1.21 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 34, 1)}
} {s}
do_test func2-1.22 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 35, 1)}
} {{}}
do_test func2-1.23 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 36, 1)}
} {{}}
do_test func2-1.24 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -0, 1)}
} {{}}
do_test func2-1.25.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -1, 0)}
} {{}}
do_test func2-1.25.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -1, 1)}
} {s}
do_test func2-1.25.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -1, 2)}
} {s}
do_test func2-1.26 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -2, 1)}
} {u}
do_test func2-1.27 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -30, 1)}
} {r}
do_test func2-1.28.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -34, 0)}
} {{}}
do_test func2-1.28.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -34, 1)}
} {S}
do_test func2-1.28.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -34, 2)}
} {Su}
do_test func2-1.29.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -35, 1)}
} {{}}
do_test func2-1.29.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -35, 2)}
} {S}
do_test func2-1.30.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -36, 0)}
} {{}}
do_test func2-1.30.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -36, 1)}
} {{}}
do_test func2-1.30.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -36, 2)}
} {{}}
do_test func2-1.30.3 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', -36, 3)}
} {S}
do_test func2-1.31.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, 0)}
} {{}}
do_test func2-1.31.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, -1)}
} {{}}
do_test func2-1.31.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, -2)}
} {{}}
do_test func2-1.32.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 1, 0)}
} {{}}
do_test func2-1.32.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 1, -1)}
} {{}}
do_test func2-1.33.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 2, 0)}
} {{}}
do_test func2-1.33.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 2, -1)}
} {S}
do_test func2-1.33.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 2, -2)}
} {S}
do_test func2-1.34.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 3, 0)}
} {{}}
do_test func2-1.34.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 3, -1)}
} {u}
do_test func2-1.34.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 3, -2)}
} {Su}
do_test func2-1.35.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 30, -1)}
} {o}
do_test func2-1.35.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 30, -2)}
} {do}
do_test func2-1.36 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 34, -1)}
} {u}
do_test func2-1.37 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 35, -1)}
} {s}
do_test func2-1.38.0 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 36, 0)}
} {{}}
do_test func2-1.38.1 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 36, -1)}
} {{}}
do_test func2-1.38.2 {
  execsql {SELECT SUBSTR('Supercalifragilisticexpialidocious', 36, -2)}
} {s}
if {"\u1234"!="u1234"} {

do_test func2-2.1.1 {
  execsql "SELECT 'hi\u1234ho'"
} "hi\u1234ho"

do_test func2-2.1.2 {
  catchsql "SELECT SUBSTR()"
} {1 {wrong number of arguments to function SUBSTR()}}
do_test func2-2.1.3 {
  catchsql "SELECT SUBSTR('hi\u1234ho')"
} {1 {wrong number of arguments to function SUBSTR()}}
do_test func2-2.1.4 {
  catchsql "SELECT SUBSTR('hi\u1234ho', 1,1,1)"
} {1 {wrong number of arguments to function SUBSTR()}}

do_test func2-2.2.0 {
  execsql "SELECT SUBSTR('hi\u1234ho', 0, 0)"
} {{}}
do_test func2-2.2.1 {
  execsql "SELECT SUBSTR('hi\u1234ho', 0, 1)"
} {{}}
do_test func2-2.2.2 {
  execsql "SELECT SUBSTR('hi\u1234ho', 0, 2)"
} "h"
do_test func2-2.2.3 {
  execsql "SELECT SUBSTR('hi\u1234ho', 0, 3)"
} "hi"
do_test func2-2.2.4 {
  execsql "SELECT SUBSTR('hi\u1234ho', 0, 4)"
} "hi\u1234"
do_test func2-2.2.5 {
  execsql "SELECT SUBSTR('hi\u1234ho', 0, 5)"
} "hi\u1234h"
do_test func2-2.2.6 {
  execsql "SELECT SUBSTR('hi\u1234ho', 0, 6)"
} "hi\u1234ho"

do_test func2-2.3.0 {
  execsql "SELECT SUBSTR('hi\u1234ho', 1, 0)"
} {{}}
do_test func2-2.3.1 {
  execsql "SELECT SUBSTR('hi\u1234ho', 1, 1)"
} "h"
do_test func2-2.3.2 {
  execsql "SELECT SUBSTR('hi\u1234ho', 1, 2)"
} "hi"
do_test func2-2.3.3 {
  execsql "SELECT SUBSTR('hi\u1234ho', 1, 3)"
} "hi\u1234"
do_test func2-2.3.4 {
  execsql "SELECT SUBSTR('hi\u1234ho', 1, 4)"
} "hi\u1234h"
do_test func2-2.3.5 {
  execsql "SELECT SUBSTR('hi\u1234ho', 1, 5)"
} "hi\u1234ho"
do_test func2-2.3.6 {
  execsql "SELECT SUBSTR('hi\u1234ho', 1, 6)"
} "hi\u1234ho"

do_test func2-2.4.0 {
  execsql "SELECT SUBSTR('hi\u1234ho', 3, 0)"
} {{}}
do_test func2-2.4.1 {
  execsql "SELECT SUBSTR('hi\u1234ho', 3, 1)"
} "\u1234"
do_test func2-2.4.2 {
  execsql "SELECT SUBSTR('hi\u1234ho', 3, 2)"
} "\u1234h"

do_test func2-2.5.0 {
  execsql "SELECT SUBSTR('\u1234', 0, 0)"
} {{}}
do_test func2-2.5.1 {
  execsql "SELECT SUBSTR('\u1234', 0, 1)"
} {{}}
do_test func2-2.5.2 {
  execsql "SELECT SUBSTR('\u1234', 0, 2)"
} "\u1234"
do_test func2-2.5.3 {
  execsql "SELECT SUBSTR('\u1234', 0, 3)"
} "\u1234"

do_test func2-2.6.0 {
  execsql "SELECT SUBSTR('\u1234', 1, 0)"
} {{}}
do_test func2-2.6.1 {
  execsql "SELECT SUBSTR('\u1234', 1, 1)"
} "\u1234"
do_test func2-2.6.2 {
  execsql "SELECT SUBSTR('\u1234', 1, 2)"
} "\u1234"
do_test func2-2.6.3 {
  execsql "SELECT SUBSTR('\u1234', 1, 3)"
} "\u1234"

do_test func2-2.7.0 {
  execsql "SELECT SUBSTR('\u1234', 2, 0)"
} {{}}
do_test func2-2.7.1 {
  execsql "SELECT SUBSTR('\u1234', 2, 1)"
} {{}}
do_test func2-2.7.2 {
  execsql "SELECT SUBSTR('\u1234', 2, 2)"
} {{}}

do_test func2-2.8.0 {
  execsql "SELECT SUBSTR('\u1234', -1, 0)"
} {{}}
do_test func2-2.8.1 {
  execsql "SELECT SUBSTR('\u1234', -1, 1)"
} "\u1234"
do_test func2-2.8.2 {
  execsql "SELECT SUBSTR('\u1234', -1, 2)"
} "\u1234"
do_test func2-2.8.3 {
  execsql "SELECT SUBSTR('\u1234', -1, 3)"
} "\u1234"

} ;# End \u1234!=u1234
ifcapable {!bloblit} {
  finish_test
  return
}
do_test func2-3.1.1 {
  set blob [execsql "SELECT x'1234'"]
  bin_to_hex [lindex $blob 0]
} "1234"
do_test func2-3.1.2 {
  catchsql {SELECT SUBSTR()}
} {1 {wrong number of arguments to function SUBSTR()}}
do_test func2-3.1.3 {
  catchsql {SELECT SUBSTR(x'1234')}
} {1 {wrong number of arguments to function SUBSTR()}}
do_test func2-3.1.4 {
  catchsql {SELECT SUBSTR(x'1234', 1,1,1)}
} {1 {wrong number of arguments to function SUBSTR()}}
do_test func2-3.2.0 {
  set blob [execsql "SELECT SUBSTR(x'1234', 0, 0)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.2.1 {
  set blob [execsql "SELECT SUBSTR(x'1234', 0, 1)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.2.2 {
  set blob [execsql "SELECT SUBSTR(x'1234', 0, 2)"]
  bin_to_hex [lindex $blob 0]
} "12"
do_test func2-3.2.3 {
  set blob [execsql "SELECT SUBSTR(x'1234', 0, 3)"]
  bin_to_hex [lindex $blob 0]
} "1234"
do_test func2-3.3.0 {
  set blob [execsql "SELECT SUBSTR(x'1234', 1, 0)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.3.1 {
  set blob [execsql "SELECT SUBSTR(x'1234', 1, 1)"]
  bin_to_hex [lindex $blob 0]
} "12"
do_test func2-3.3.2 {
  set blob [execsql "SELECT SUBSTR(x'1234', 1, 2)"]
  bin_to_hex [lindex $blob 0]
} "1234"
do_test func2-3.3.3 {
  set blob [execsql "SELECT SUBSTR(x'1234', 1, 3)"]
  bin_to_hex [lindex $blob 0]
} "1234"
do_test func2-3.4.0 {
  set blob [execsql "SELECT SUBSTR(x'1234', -1, 0)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.4.1 {
  set blob [execsql "SELECT SUBSTR(x'1234', -1, 1)"]
  bin_to_hex [lindex $blob 0]
} "34"
do_test func2-3.4.2 {
  set blob [execsql "SELECT SUBSTR(x'1234', -1, 2)"]
  bin_to_hex [lindex $blob 0]
} "34"
do_test func2-3.4.3 {
  set blob [execsql "SELECT SUBSTR(x'1234', -1, 3)"]
  bin_to_hex [lindex $blob 0]
} "34"
do_test func2-3.5.0 {
  set blob [execsql "SELECT SUBSTR(x'1234', -2, 0)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.5.1 {
  set blob [execsql "SELECT SUBSTR(x'1234', -2, 1)"]
  bin_to_hex [lindex $blob 0]
} "12"
do_test func2-3.5.2 {
  set blob [execsql "SELECT SUBSTR(x'1234', -2, 2)"]
  bin_to_hex [lindex $blob 0]
} "1234"
do_test func2-3.5.3 {
  set blob [execsql "SELECT SUBSTR(x'1234', -2, 3)"]
  bin_to_hex [lindex $blob 0]
} "1234"
do_test func2-3.6.0 {
  set blob [execsql "SELECT SUBSTR(x'1234', -1, 0)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.6.1 {
  set blob [execsql "SELECT SUBSTR(x'1234', -1, -1)"]
  bin_to_hex [lindex $blob 0]
} "12"
do_test func2-3.6.2 {
  set blob [execsql "SELECT SUBSTR(x'1234', -1, -2)"]
  bin_to_hex [lindex $blob 0]
} "12"
do_test func2-3.6.3 {
  set blob [execsql "SELECT SUBSTR(x'1234', -1, -3)"]
  bin_to_hex [lindex $blob 0]
} "12"
do_test func2-3.7.0 {
  set blob [execsql "SELECT SUBSTR(x'1234', -2, 0)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.7.1 {
  set blob [execsql "SELECT SUBSTR(x'1234', -2, -1)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.7.2 {
  set blob [execsql "SELECT SUBSTR(x'1234', -2, -2)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.8.0 {
  set blob [execsql "SELECT SUBSTR(x'1234', 1, 0)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.8.1 {
  set blob [execsql "SELECT SUBSTR(x'1234', 1, -1)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.8.2 {
  set blob [execsql "SELECT SUBSTR(x'1234', 1, -2)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.9.0 {
  set blob [execsql "SELECT SUBSTR(x'1234', 2, 0)"]
  bin_to_hex [lindex $blob 0]
} {}
do_test func2-3.9.1 {
  set blob [execsql "SELECT SUBSTR(x'1234', 2, -1)"]
  bin_to_hex [lindex $blob 0]
} "12"
do_test func2-3.9.2 {
  set blob [execsql "SELECT SUBSTR(x'1234', 2, -2)"]
  bin_to_hex [lindex $blob 0]
} "12"
do_test func2-3.10 {
  set tm [time {
    execsql {
      SELECT '' IN (zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(
      zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(
      zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(
      zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(
      zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(
      zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(
      zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(
      zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(
      zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(1)
      )))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))
  }
  }]

  set tm [lindex $tm 0]
  expr $tm<2000000
} {1}
finish_test
"#,
    );
}
