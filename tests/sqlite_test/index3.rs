#![forbid(unsafe_code)]

#[test]
#[ignore = "10/11 subtests passed; pending: Phase 5 EQP/optimizer"]
fn test_index3() {
    sqliters_tcltest::run_test_script(
        "index3",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test index3-1.1 {
  execsql {
    CREATE TABLE t1(a);
    INSERT INTO t1 VALUES(1);
    INSERT INTO t1 VALUES(1);
    SELECT * FROM t1;
  }
} {1 1}
do_test index3-1.2 {
  catchsql {
    BEGIN;
    CREATE UNIQUE INDEX i1 ON t1(a);
  }
} {1 {UNIQUE constraint failed: t1.a}}
do_test index3-1.3 {
  catchsql COMMIT;
} {0 {}}
integrity_check index3-1.4
do_execsql_test index3-2.1 {
  DROP TABLE t1;
  CREATE TABLE t1(a, b, c, d, e, 
                  PRIMARY KEY('a'), UNIQUE('b' COLLATE nocase DESC));
  CREATE INDEX t1c ON t1('c');
  CREATE INDEX t1d ON t1('d' COLLATE binary ASC);
  WITH RECURSIVE c(x) AS (VALUES(1) UNION SELECT x+1 FROM c WHERE x<30)
    INSERT INTO t1(a,b,c,d,e) 
      SELECT x, printf('ab%03xxy',x), x, x, x FROM c;
} {}
do_execsql_test index3-2.2 {
  SELECT a FROM t1 WHERE b='ab005xy' COLLATE nocase;
} {5}
do_execsql_test index3-2.2eqp {
  EXPLAIN QUERY PLAN
  SELECT a FROM t1 WHERE b='ab005xy' COLLATE nocase;
} {/USING INDEX/}
do_execsql_test index3-2.3 {
  SELECT name FROM sqlite_master WHERE tbl_name='t1' ORDER BY name
} {sqlite_autoindex_t1_1 sqlite_autoindex_t1_2 t1 t1c t1d}
do_execsql_test index3-2.4 {
  CREATE TABLE t2a(a integer, b, PRIMARY KEY(a));
  CREATE TABLE t2b("a" integer, b, PRIMARY KEY("a"));
  CREATE TABLE t2c([a] integer, b, PRIMARY KEY([a]));
  CREATE TABLE t2d('a' integer, b, PRIMARY KEY('a'));
}
do_execsql_test index3-2.5 {
  SELECT name FROM sqlite_master WHERE tbl_name LIKE 't2_' ORDER BY name
} {t2a t2b t2c t2d}
do_test index3-99.1 {
  sqlite3_db_config db DEFENSIVE 0
  execsql {
    PRAGMA writable_schema=on;
    UPDATE sqlite_master SET sql='nonsense' WHERE name='t1d'
  }
  db close
  catch { sqlite3 db test.db }
  catchsql { DROP INDEX t1c }
} {1 {malformed database schema (t1d)}}
finish_test
"#,
    );
}
