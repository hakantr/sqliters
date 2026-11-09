#[test]
#[ignore = "pending Phase 7: pragma_function_list/pragma_module_list/pragma_pragma_list virtual tables, UDFs, and fts5=1"]
fn test_pragma5() {
    sqliters_tcltest::run_test_script(
        "pragma5",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix pragma5
if { [catch {db one "SELECT count(*) FROM pragma_function_list"}] } {
  finish_test
  return
}
db function external external
do_execsql_test 1.0 {
  PRAGMA table_info(pragma_function_list)
} {
  0 name {} 0 {} 0 
  1 builtin {} 0 {} 0
  2 type {} 0 {} 0
  3 enc {} 0 {} 0
  4 narg {} 0 {} 0
  5 flags {} 0 {} 0
}
do_execsql_test 1.1 {
  SELECT DISTINCT name, builtin
    FROM pragma_function_list WHERE name='upper' AND builtin
} {upper 1}
do_execsql_test 1.2 {
  SELECT DISTINCT name, builtin
    FROM pragma_function_list WHERE name LIKE 'exter%';
} {external 0}
ifcapable fts5 {
  do_execsql_test 2.0 {
    PRAGMA table_info(pragma_module_list)
  } {
    0 name {} 0 {} 0 
  }
  do_execsql_test 2.1 {
    SELECT * FROM pragma_module_list WHERE name='fts5'
  } {fts5}
}
do_execsql_test 3.0 {
  PRAGMA table_info(pragma_pragma_list)
} {
  0 name {} 0 {} 0 
}
do_execsql_test 3.1 {
  SELECT * FROM pragma_pragma_list WHERE name='pragma_list'
} {pragma_list}
finish_test
"#,
    );
}
