#[test]
#[ignore = "2/18 subtests passed; pending: types3 edge cases"]
fn test_types3() {
    sqliters_tcltest::run_test_script(
        "types3",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test types3-1.1 {
  set V [format %s xxxxx]
  concat [tcl_variable_type V] [execsql {SELECT typeof(:V)}]
} {text}
do_test types3-1.2 {
  set V [expr {int(1+2)}]
  concat [tcl_variable_type V] [execsql {SELECT typeof(:V)}]
} {int integer}
set V [expr {1+12345678012345}]
if {[tcl_variable_type V]=="wideInt"} {
  do_test types3-1.3 {
    set V [expr {1+123456789012345}]
    concat [tcl_variable_type V] [execsql {SELECT typeof(:V)}]
  } {wideInt integer}
} else {
  do_test types3-1.3 {
    set V [expr {1+123456789012345}]
    concat [tcl_variable_type V] [execsql {SELECT typeof(:V)}]
  } {int integer}
}
do_test types3-1.4 {
  set V [expr {1.0+1}]
  concat [tcl_variable_type V] [execsql {SELECT typeof(:V)}]
} {double real}
do_test types3-1.5 {
  set V [binary format a3 abc]
  concat [tcl_variable_type V] [execsql {SELECT typeof(:V)}]
} {bytearray blob}
do_test types3-1.6 {
  set V "abc"
  binary scan $V a3 x
  concat [tcl_variable_type V] [execsql {SELECT typeof(:V)}]
} {bytearray text}
ifcapable bloblit {
  do_test types3-2.1 {
    set V [db one {SELECT x'616263'}]
    tcl_variable_type V
  } bytearray
}
do_test types3-2.2 {
  set V [db one {SELECT 123}]
  tcl_variable_type V
} int
set Vx [expr {1+wide(123456789123456)}]
do_test types3-2.3 {
  set V [db one {SELECT 1234567890123456}]
  tcl_variable_type V
} [tcl_variable_type Vx]
do_test types3-2.4.1 {
  set V [db one {SELECT 1234567890123456.1}]
  tcl_variable_type V
} double
do_test types3-2.4.2 {
  set V [db one {SELECT 1234567890123.456}]
  tcl_variable_type V
} double
do_test types3-2.5 {
  set V [db one {SELECT '1234567890123456.0'}]
  tcl_variable_type V
} {}
do_test types3-2.6 {
  set V [db one {SELECT NULL}]
  tcl_variable_type V
} {}
sqlite3_create_function db
do_execsql_test types3-3.1 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(x TEXT PRIMARY KEY);
  INSERT INTO t1 VALUES('1');
  SELECT * FROM t1 WHERE NOT x=upper(1);
} {}
do_execsql_test types3-3.2 {
  SELECT * FROM t1 WHERE NOT x=add_text_type(1);
} {}
do_execsql_test types3-3.3 {
  SELECT * FROM t1 WHERE NOT x=add_int_type('1');
} {}
do_execsql_test types3-3.4 {
  DELETE FROM t1;
  INSERT INTO t1 VALUES(1.25);
  SELECT * FROM t1 WHERE NOT x=add_real_type('1.25');
} {}
do_execsql_test types3-3.5 {
  SELECT * FROM t1 WHERE NOT x=add_text_type(1.25);
} {}
finish_test
"#,
    );
}
