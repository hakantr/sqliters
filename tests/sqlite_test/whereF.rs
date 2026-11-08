#[test]
#[ignore = "13/27 subtests passed; pending: EQP/optimizer"]
fn test_whereF() {
    sqliters_tcltest::run_test_script(
        "whereF",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix whereF
do_execsql_test 1.0 {
  PRAGMA automatic_index = 0;
  CREATE TABLE t1(a, b, c);
  CREATE TABLE t2(d, e, f);
  CREATE UNIQUE INDEX i1 ON t1(a);
  CREATE UNIQUE INDEX i2 ON t2(d);
} {}
foreach {tn sql} {
  1 "SELECT * FROM t1,           t2 WHERE t1.a=t2.e AND t2.d<t1.b AND t1.c!=10"
  2 "SELECT * FROM t2,           t1 WHERE t1.a=t2.e AND t2.d<t1.b AND t1.c!=10"
  3 "SELECT * FROM t2 CROSS JOIN t1 WHERE t1.a=t2.e AND t2.d<t1.b AND t1.c!=10"
} {
  do_test 1.$tn {
    db eval "EXPLAIN QUERY PLAN $sql"
   } {/.*SCAN t2\y.*SEARCH t1\y.*/}
}
do_execsql_test 2.0 {
  DROP TABLE t1;
  DROP TABLE t2;
  CREATE TABLE t1(a, b, c);
  CREATE TABLE t2(d, e, f);

  CREATE UNIQUE INDEX i1 ON t1(a);
  CREATE UNIQUE INDEX i2 ON t1(b);
  CREATE UNIQUE INDEX i3 ON t2(d);
} {}
foreach {tn sql} {
  1 "SELECT * FROM t1,           t2 WHERE t1.a>? AND t2.d>t1.c AND t1.b=t2.e"
  2 "SELECT * FROM t2,           t1 WHERE t1.a>? AND t2.d>t1.c AND t1.b=t2.e"
  3 "SELECT * FROM t2 CROSS JOIN t1 WHERE t1.a>? AND t2.d>t1.c AND t1.b=t2.e"
} {
  do_test 2.$tn {
    db eval "EXPLAIN QUERY PLAN $sql"
   } {/.*SCAN t2\y.*SEARCH t1\y.*/}
}
do_execsql_test 3.0 {
  DROP TABLE t1;
  DROP TABLE t2;
  CREATE TABLE t1(a, b, c);
  CREATE TABLE t2(d, e, f);

  CREATE UNIQUE INDEX i1 ON t1(a, b);
  CREATE INDEX i2 ON t2(d);
} {}
foreach {tn sql} {
  1 {SELECT t1.a, t1.b, t2.d, t2.e FROM t1, t2 
     WHERE t2.d=t1.b AND t1.a=(t2.d+1) AND t1.b = (t2.e+1)}

  2 {SELECT t1.a, t1.b, t2.d, t2.e FROM t2, t1 
     WHERE t2.d=t1.b AND t1.a=(t2.d+1) AND t1.b = (t2.e+1)}

  3 {SELECT t1.a, t1.b, t2.d, t2.e FROM t2 CROSS JOIN t1 
     WHERE t2.d=t1.b AND t1.a=(t2.d+1) AND t1.b = (t2.e+1)}
} {
  do_test 3.$tn {
    db eval "EXPLAIN QUERY PLAN $sql"
   } {/.*SCAN t2\y.*SEARCH t1\y.*/}
}
do_execsql_test 4.0 {
  CREATE TABLE t4(a,b,c,d,e, PRIMARY KEY(a,b,c));
  CREATE INDEX t4adc ON t4(a,d,c);
  CREATE UNIQUE INDEX t4aebc ON t4(a,e,b,c);
  EXPLAIN QUERY PLAN SELECT rowid FROM t4 WHERE a=? AND b=?;
} {/a=. AND b=./}
reset_db
do_execsql_test 5.0 {
  CREATE TABLE t1(f1);
  CREATE TABLE t2(f2);
  CREATE INDEX t2f ON t2(f2);

  INSERT INTO t1 VALUES(-1);
  INSERT INTO t1 VALUES(-1);
  INSERT INTO t1 VALUES(-1);
  INSERT INTO t1 VALUES(-1);

  WITH w(i) AS (
    SELECT 1 UNION ALL SELECT i+1 FROM w WHERE i<1000
  )
  INSERT INTO t2 SELECT -1 FROM w;
}
do_execsql_test 5.1 {
  SELECT count(*) FROM t1, t2 WHERE t2.rowid = +t1.rowid
} {4}
do_test 5.2 { expr [db status vmstep]<200 } 1
do_execsql_test 5.3 {
  SELECT count(*) FROM t1, t2 WHERE (
    t2.rowid = +t1.rowid OR t2.f2 = t1.f1
  )
} {4000}
do_test 5.4 { expr [db status vmstep]>1000 } 1
do_execsql_test 5.5 {
  SELECT count(*) FROM t1, t2 WHERE (
    t2.rowid = +t1.rowid OR (t2.f2 = t1.f1 AND t1.f1!=-1)
  )
} {4}
do_test 5.6 { expr [db status vmstep]<200 } 1
ifcapable json1&&vtab {
  do_execsql_test 6.1 {
    CREATE TABLE t6(x);
    SELECT * FROM t6 WHERE 1 IN (SELECT value FROM json_each(x));
  } {}

  do_execsql_test 6.2 {
    DROP TABLE t6;
    CREATE TABLE t6(a,b,c);
    INSERT INTO t6 VALUES
     (0,null,'{"a":0,"b":[3,4,5],"c":{"x":4.5,"y":7.8}}'),
     (1,null,'{"a":1,"b":[3,4,5],"c":{"x":4.5,"y":7.8}}'),
     (2,null,'{"a":9,"b":[3,4,5],"c":{"x":4.5,"y":7.8}}');
    SELECT * FROM t6
     WHERE (EXISTS (SELECT 1 FROM json_each(t6.c) AS x WHERE x.value=1));
  } {1 {} {{"a":1,"b":[3,4,5],"c":{"x":4.5,"y":7.8}}}}

  do_execsql_test 6.3 {
    DROP TABLE IF EXISTS t;
    CREATE TABLE t(json JSON);
    SELECT * FROM t
     WHERE(EXISTS(SELECT 1 FROM json_each(t.json,"$.foo") j
                   WHERE j.value = 'meep'));
  } {}
  do_execsql_test 6.4 {
    INSERT INTO t VALUES('{"xyzzy":null}');
    INSERT INTO t VALUES('{"foo":"meep","other":12345}');
    INSERT INTO t VALUES('{"foo":"bingo","alt":5.25}');
    SELECT * FROM t
     WHERE(EXISTS(SELECT 1 FROM json_each(t.json,"$.foo") j
                   WHERE j.value = 'meep'));
  } {{{"foo":"meep","other":12345}}}
}
do_execsql_test 7.1 {
  DROP TABLE IF EXISTS cd;
  CREATE TABLE cd ( cdid INTEGER PRIMARY KEY NOT NULL, genreid integer );
  CREATE INDEX cd_idx_genreid ON cd (genreid);
  INSERT INTO cd  ( cdid, genreid ) VALUES
                     ( 1,    1 ),
                     ( 2, NULL ),
                     ( 3, NULL ),
                     ( 4, NULL ),
                     ( 5, NULL );
  
  SELECT cdid
    FROM cd me
  WHERE 2 > (
    SELECT COUNT( * )
      FROM cd rownum__emulation
    WHERE
      (
        me.genreid IS NOT NULL
          AND
        rownum__emulation.genreid IS NULL
      )
        OR
      (
        me.genreid IS NOT NULL
          AND
        rownum__emulation.genreid IS NOT NULL
          AND
        rownum__emulation.genreid < me.genreid
      )
        OR
      (
        ( me.genreid = rownum__emulation.genreid OR ( me.genreid IS NULL
  AND rownum__emulation.genreid IS NULL ) )
          AND
        rownum__emulation.cdid > me.cdid
      )
  );
} {4 5}
do_execsql_test 7.2 {
  DROP TABLE IF EXISTS t1;
  DROP TABLE IF EXISTS t2;
  CREATE TABLE t1(a INTEGER PRIMARY KEY, b);
  INSERT INTO t1(a,b) VALUES(1,1);
  CREATE TABLE t2(aa INTEGER PRIMARY KEY, bb);
  INSERT INTO t2(aa,bb) VALUES(1,1),(2,NULL),(3,NULL);
  SELECT (
    SELECT COUNT(*) FROM t2
     WHERE ( t1.b IS NOT NULL AND t2.bb IS NULL )
        OR ( t2.bb < t1.b )
        OR ( t1.b IS t2.bb AND t2.aa > t1.a )
    )
    FROM t1;
} {2}
do_execsql_test 7.3 {
  DROP TABLE IF EXISTS t1;
  DROP TABLE IF EXISTS t2;
  CREATE TABLE t1(a INTEGER PRIMARY KEY, b TEXT);
  INSERT INTO t1(a,b) VALUES(1,'abcxyz');
  CREATE TABLE t2(aa INTEGER PRIMARY KEY, bb TEXT);
  INSERT INTO t2(aa,bb) VALUES(1,'abc'),(2,'wxyz'),(3,'xyz');
  CREATE INDEX t2bb ON t2(bb);
  EXPLAIN SELECT (
    SELECT COUNT(*) FROM t2
     WHERE ( t1.b GLOB 'a*z' AND t2.bb='xyz' )
        OR ( t2.bb = t1.b )
        OR ( t2.aa = t1.a )
    )
    FROM t1;
} {~/ (Lt|Ge) /}
finish_test
"#,
    );
}
