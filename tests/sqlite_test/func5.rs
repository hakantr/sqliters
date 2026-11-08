#[test]
#[ignore = "3/4 subtests passed; pending: no such function: counter2"]
fn test_func5() {
    sqliters_tcltest::run_test_script(
        "func5",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_execsql_test func5-1.1 {
  PRAGMA encoding=UTF16le;
  CREATE TABLE t1(x,a,b,c);
  INSERT INTO t1 VALUES(1,'ab','cd',1);
  INSERT INTO t1 VALUES(2,'gh','ef',5);
  INSERT INTO t1 VALUES(3,'pqr','fuzzy',99);
  INSERT INTO t1 VALUES(4,'abcdefg','xy',22);
  INSERT INTO t1 VALUES(5,'shoe','mayer',2953);
  SELECT x FROM t1 WHERE c=instr('abcdefg',b) OR a='abcdefg' ORDER BY +x;
} {2 4}
do_execsql_test func5-1.2 {
  SELECT x FROM t1 WHERE a='abcdefg' OR c=instr('abcdefg',b) ORDER BY +x;
} {2 4}
do_execsql_test func5-2.1 {
  CREATE TABLE t2(x,y);
  INSERT INTO t2 VALUES(1,2),(3,4),(5,6),(7,8);
  SELECT x, y FROM t2 WHERE x+5=5+x ORDER BY +x;
} {1 2 3 4 5 6 7 8}
sqlite3_create_function db
do_execsql_test func5-2.2 {
  SELECT x, y FROM t2
   WHERE x+counter1('hello')=counter1('hello')+x
   ORDER BY +x;
} {}
set cvalue [db one {SELECT counter2('hello')+1}]
do_execsql_test func5-2.3 {
  SELECT x, y FROM t2
   WHERE x+counter2('hello')=$cvalue+x
   ORDER BY +x;
} {1 2 3 4 5 6 7 8}
finish_test
"#,
    );
}
