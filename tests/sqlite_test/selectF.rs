#[test]
fn test_selectF() {
    sqliters_tcltest::run_test_script(
        "selectF",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix selectF
do_execsql_test 1 {
  BEGIN TRANSACTION;
  CREATE TABLE t1(a, b, c);
  INSERT INTO "t1" VALUES(1,'one','I');
  CREATE TABLE t2(d, e, f);
  INSERT INTO "t2" VALUES(5,'ten','XX');
  INSERT INTO "t2" VALUES(6,NULL,NULL);

  CREATE INDEX i1 ON t1(b, a);
  COMMIT;
}
do_execsql_test 2 {
  SELECT * FROM t2
  UNION ALL 
  SELECT * FROM t1 WHERE a<5 
  ORDER BY 2, 1
} {6 {} {} 1 one I 5 ten XX}
finish_test
"#,
    );
}
