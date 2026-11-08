#[test]
fn test_affinity2() {
    sqliters_tcltest::run_test_script(
        "affinity2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix affinity2
do_execsql_test affinity2-100 {
  CREATE TABLE t1(
    xi INTEGER,
    xr REAL,
    xb BLOB,
    xn NUMERIC,
    xt TEXT
  );
  INSERT INTO t1(rowid,xi,xr,xb,xn,xt) VALUES(1,1,1,1,1,1);
  INSERT INTO t1(rowid,xi,xr,xb,xn,xt) VALUES(2,'2','2','2','2','2');
  INSERT INTO t1(rowid,xi,xr,xb,xn,xt) VALUES(3,'03','03','03','03','03');

} {}
do_execsql_test affinity2-110 {
  SELECT xi, typeof(xi) FROM t1 ORDER BY rowid;
} {1 integer 2 integer 3 integer}
do_execsql_test affinity2-120 {
  SELECT xr, typeof(xr) FROM t1 ORDER BY rowid;
} {1.0 real 2.0 real 3.0 real}
do_execsql_test affinity2-130 {
  SELECT xb, typeof(xb) FROM t1 ORDER BY rowid;
} {1 integer 2 text 03 text}
do_execsql_test affinity2-140 {
  SELECT xn, typeof(xn) FROM t1 ORDER BY rowid;
} {1 integer 2 integer 3 integer}
do_execsql_test affinity2-150 {
  SELECT xt, typeof(xt) FROM t1 ORDER BY rowid;
} {1 text 2 text 03 text}
do_execsql_test affinity2-200 {
  SELECT rowid, xi==xt, xi==xb, xi==+xt FROM t1 ORDER BY rowid;
} {1 1 1 1 2 1 1 1 3 1 1 1}
do_execsql_test affinity2-210 {
  SELECT rowid, xr==xt, xr==xb, xr==+xt FROM t1 ORDER BY rowid;
} {1 1 1 1 2 1 1 1 3 1 1 1}
do_execsql_test affinity2-220 {
  SELECT rowid, xn==xt, xn==xb, xn==+xt FROM t1 ORDER BY rowid;
} {1 1 1 1 2 1 1 1 3 1 1 1}
do_execsql_test affinity2-300 {
  SELECT rowid, xt==+xi, xt==xi, xt==xb FROM t1 ORDER BY rowid;
} {1 1 1 0 2 1 1 1 3 0 1 1}
do_execsql_test 400 {
  CREATE TABLE ttt(c0, c1);
  CREATE INDEX ii ON ttt(CAST(c0 AS NUMERIC)); 
  INSERT INTO ttt VALUES('abc', '-1');
}
do_execsql_test 410 {
  SELECT * FROM ttt WHERE CAST(c0 AS NUMERIC) > c1 GROUP BY rowid; 
} {abc -1}
do_execsql_test 420 {
  SELECT * FROM ttt INDEXED BY ii WHERE CAST(c0 AS NUMERIC) > c1 GROUP BY rowid;
} {abc -1}
do_execsql_test 430 {
  CREATE TABLE t3(a, b, c INTEGER);
  CREATE INDEX t3ac ON t3(a, c-1);
  INSERT INTO t3 VALUES(1, 1, 1);
  INSERT INTO t3 VALUES(2, 1, 0);
  INSERT INTO t3 VALUES(3, 1, 1);
  INSERT INTO t3 VALUES(4, 1, 0);
  INSERT INTO t3 VALUES(5, 1, 1);
}
do_execsql_test 440 {
  SELECT * FROM t3 WHERE c='0' ORDER BY a;
} {2 1 0 4 1 0}
do_execsql_test 500 {
  DROP TABLE IF EXISTS t0;
  CREATE TABLE t0(c0 TEXT UNIQUE, c1);
  INSERT INTO t0(c0) VALUES (-1);
  SELECT quote(- x'ce'), quote(t0.c0), quote(- x'ce' >= t0.c0) FROM t0;
} {0 '-1' 1}
do_execsql_test 501 {
  SELECT * FROM t0 WHERE - x'ce' >= t0.c0;
} {-1 {}}
do_execsql_test 502 {
  SELECT quote(+-+x'ce'), quote(t0.c0), quote(+-+x'ce' >= t0.c0) FROM t0;
} {0 '-1' 1}
do_execsql_test 503 {
  SELECT * FROM t0 WHERE +-+x'ce' >= t0.c0;
} {-1 {}}
do_execsql_test 504 {
  SELECT quote(- 'ce'), quote(t0.c0), quote(- 'ce' >= t0.c0) FROM t0;
} {0 '-1' 1}
do_execsql_test 505 {
  SELECT * FROM t0 WHERE - 'ce' >= t0.c0;
} {-1 {}}
do_execsql_test 506 {
  SELECT quote(+-+'ce'), quote(t0.c0), quote(+-+'ce' >= t0.c0) FROM t0;
} {0 '-1' 1}
do_execsql_test 507 {
  SELECT * FROM t0 WHERE +-+'ce' >= t0.c0;
} {-1 {}}
if {![info exists ::G(valgrind)]} {
  do_execsql_test 600 {
    DROP TABLE IF EXISTS t0;
    CREATE TABLE t0(c0 REAL UNIQUE);
    INSERT INTO t0(c0) VALUES (3175546974276630385);
    SELECT 3175546974276630385 < c0 FROM t0;
  } {1}
  do_execsql_test 601 {
    SELECT 1 FROM t0 WHERE 3175546974276630385 < c0;
  } {1}
}
finish_test
"#,
    );
}
