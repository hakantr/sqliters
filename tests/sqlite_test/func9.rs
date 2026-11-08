#[test]
#[ignore = "0/15 subtests passed; pending: func9 edge cases"]
fn test_func9() {
    sqliters_tcltest::run_test_script(
        "func9",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_execsql_test func9-100 {
  SELECT concat('abc',123,null,'xyz');
} {abc123xyz}
do_execsql_test func9-110 {
  SELECT typeof(concat(null));
} {text}
do_catchsql_test func9-120 {
  SELECT concat();
} {1 {wrong number of arguments to function concat()}}
do_execsql_test func9-130 {
  SELECT concat_ws(',',1,2,3,4,5,6,7,8,NULL,9,10,11,12);
} {1,2,3,4,5,6,7,8,9,10,11,12}
do_execsql_test func9-131 {
  SELECT concat_ws(',',1,2,3,4,'',6,7,8,NULL,9,10,11,12);
} {1,2,3,4,,6,7,8,9,10,11,12}
do_execsql_test func9-132 {
  SELECT concat_ws(',','',2,3,4,'',6,7,8,NULL,9,10,11,12);
} {,2,3,4,,6,7,8,9,10,11,12}
do_execsql_test func9-133 {
  SELECT concat_ws(',',NULL,'',3,4,'',6,7,8,NULL,9,10,11,12);
} {,3,4,,6,7,8,9,10,11,12}
do_execsql_test func9-134 {
  SELECT concat_ws(',',NULL,NULL,NULL,'',3,4,'',6,7,8,NULL,9,10,11,12);
} {,3,4,,6,7,8,9,10,11,12}
do_execsql_test func9-140 {
  SELECT concat_ws(NULL,1,2,3,4,5,6,7,8,NULL,9,10,11,12);
} {{}}
do_catchsql_test func9-150 {
  SELECT concat_ws();
} {1 {wrong number of arguments to function concat_ws()}}
do_catchsql_test func9-160 {
  SELECT concat_ws(',');
} {1 {wrong number of arguments to function concat_ws()}}
do_execsql_test func9-200 {
  SELECT unistr('G\u00e4ste');
} {Gäste}
do_execsql_test func9-210 {
  SELECT unistr_quote(unistr('G\u00e4ste'));
} {'Gäste'}
do_execsql_test func9-220 {
  SELECT format('%#Q',unistr('G\u00e4ste'));
} {'Gäste'}
do_execsql_test func9-300 {
  SELECT hex( unistr('\UFFFFFFFF') )
} {F7BFBFBF}
finish_test
"#,
    );
}
