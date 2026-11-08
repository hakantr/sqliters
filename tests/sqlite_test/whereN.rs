#[test]
#[ignore = "0/2 subtests passed; pending: ANALYZE/sqlite_stat1, EQP/optimizer"]
fn test_whereN() {
    sqliters_tcltest::run_test_script(
        "whereN",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix whereN
do_execsql_test 1.0 {
  CREATE TABLE datasource(dsid INT, name TEXT);
  INSERT INTO datasource VALUES(1,'ds-one'),(2,'ds-two'),(3,'ds-three');
  CREATE INDEX ds1 ON datasource(name, dsid);

  CREATE TABLE rule(rid INT, team_id INT, dsid INT);
  WITH RECURSIVE c(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM c WHERE n<9)
    INSERT INTO rule(rid,team_id,dsid) SELECT n, 1, 1 FROM c;
  WITH RECURSIVE c(n) AS (VALUES(10) UNION ALL SELECT n+1 FROM c WHERE n<24)
    INSERT INTO rule(rid,team_id,dsid) SELECT n, 2, 2 FROM c;
  CREATE INDEX rule2 ON rule(dsid, rid);

  CREATE TABLE violation(vid INT, rid INT, vx BLOB);
  CREATE INDEX v1 ON violation(rid, vid);
  CREATE INDEX v2 ON violation(vid);
  ANALYZE;
  DELETE FROM sqlite_stat1;
  DROP TABLE IF EXISTS sqlite_stat4;
  INSERT INTO sqlite_stat1 VALUES
    ('violation','v2','376661 1'),
    ('violation','v1','376661 28974 1'),
    ('rule','rule2','24 12 1'),
    ('datasource','ds1','3 1 1');
  ANALYZE sqlite_schema;
}
set DSNAME ds-two   ;#  Only a few rows.  Change to "ds-one" for many rows.
do_eqp_test 1.1 {
  SELECT count(*), length(group_concat(vx)) FROM (
    SELECT V.*
      FROM datasource DS, rule R, violation V
     WHERE V.rid=R.rid
       AND R.dsid=DS.dsid
       AND DS.name=$DSNAME
     ORDER BY V.vid desc
  );
} {
  QUERY PLAN
  |--CO-ROUTINE (subquery-xxxxxx)
  |  |--SEARCH DS USING COVERING INDEX ds1 (name=?)
  |  |--SEARCH R USING COVERING INDEX rule2 (dsid=?)
  |  |--SEARCH V USING INDEX v1 (rid=?)
  |  `--USE TEMP B-TREE FOR ORDER BY
  `--SCAN (subquery-xxxxxx)
}
finish_test
"#,
    );
}
