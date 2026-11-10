#![forbid(unsafe_code)]

#[test]
fn test_conflict4() {
    sqliters_tcltest::run_test_script(
        "conflict4",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix conflict4
do_execsql_test 1.1 {
  CREATE TABLE t0(
    a INTEGER PRIMARY KEY ON CONFLICT REPLACE,
    b INT UNIQUE          ON CONFLICT FAIL,
    c INT UNIQUE          ON CONFLICT ABORT
  );
  INSERT INTO t0 VALUES (1, 10, 100),(2, 20, 200);
}
do_catchsql_test 1.2 {
  INSERT INTO t0 VALUES (1, 20, 300) ON CONFLICT(c) DO NOTHING;
} {1 {UNIQUE constraint failed: t0.b}}
do_execsql_test 1.3 {
  PRAGMA integrity_check;
} {ok}
do_execsql_test 1.4 {
  SELECT * FROM t0 ORDER BY a;
} {1 10 100 2 20 200}
do_execsql_test 1.5 {
  DROP TABLE t0;
  CREATE TABLE t0(
    a INTEGER PRIMARY KEY ON CONFLICT REPLACE,
    b INT UNIQUE          ON CONFLICT IGNORE,
    c INT UNIQUE          ON CONFLICT ABORT
  );
  INSERT INTO t0 VALUES (1, 10, 100),(2, 20, 200);
}
do_catchsql_test 1.6 {
  INSERT INTO t0 VALUES (1, 20, 300) ON CONFLICT(c) DO NOTHING;
} {0 {}}
do_execsql_test 1.7 {
  PRAGMA integrity_check;
} {ok}
do_execsql_test 1.8 {
  SELECT * FROM t0 ORDER BY a;
} {1 10 100 2 20 200}
do_execsql_test 1.9 {
  DROP TABLE t0;
  CREATE TABLE t0(
    a INTEGER PRIMARY KEY ON CONFLICT REPLACE,
    b INT UNIQUE          ON CONFLICT FAIL,
    c INT UNIQUE          ON CONFLICT ABORT
  );
  INSERT INTO t0 VALUES (1, 10, 100),(2, 20, 200);
}
do_catchsql_test 1.10 {
  INSERT INTO t0 VALUES (1, 20, 300) ON CONFLICT(c) DO NOTHING;
} {1 {UNIQUE constraint failed: t0.b}}
do_execsql_test 1.11 {
  PRAGMA integrity_check;
} {ok}
do_execsql_test 1.12 {
  SELECT * FROM t0 ORDER BY a;
} {1 10 100 2 20 200}
do_execsql_test 1.13 {
  DROP TABLE t0;
  CREATE TABLE t1(
    a INTEGER PRIMARY KEY,
    b UNIQUE ON CONFLICT FAIL,
    c UNIQUE
  );
  INSERT INTO t1 VALUES(1, 1, 1);
}
do_catchsql_test 1.14 {
  -- Inserts 0 rows, because hitting the constraint on (b) does ABORT:
  INSERT INTO t1 VALUES(3, 3, 3), (2, 1, 2) ON CONFLICT(c) DO NOTHING;
} {1 {UNIQUE constraint failed: t1.b}}
do_execsql_test 1.15 {
  SELECT * FROM t1 ORDER BY a;
} {1 1 1 3 3 3}
do_catchsql_test 1.16 {
  -- Inserts 1 row, because hitting the constraint on (b) does FAIL:
  DELETE FROM t1 WHERE a=3;
  INSERT INTO t1 VALUES(3, 3, 3), (2, 1, 2);
} {1 {UNIQUE constraint failed: t1.b}}
do_execsql_test 1.17 {
  SELECT * FROM t1 ORDER BY a;
} {1 1 1 3 3 3}
do_execsql_test 1.18 {
  PRAGMA integrity_check;
} ok
do_execsql_test 2.1 {
  DROP TABLE t1;
  PRAGMA recursive_triggers=ON;
  CREATE TABLE t2(
    a INTEGER PRIMARY KEY ON CONFLICT REPLACE,
    b INT UNIQUE          ON CONFLICT FAIL,
    c INT UNIQUE          ON CONFLICT ABORT
  );
  CREATE TRIGGER t_ad AFTER DELETE ON t2 BEGIN
    INSERT INTO t2 VALUES(99,30,999);
  END;
  INSERT INTO t2 VALUES(1,10,100),(2,20,200);
  SELECT * FROM t2 ORDER BY a;
} {1 10 100 2 20 200}
do_catchsql_test 2.2 {
  INSERT INTO t2 VALUES(4,40,400),(1,30,300),(5,50,500)
      ON CONFLICT(c) DO NOTHING;
} {1 {UNIQUE constraint failed: t2.b}}
do_execsql_test 2.3 {
  PRAGMA integrity_check;
} ok
do_execsql_test 2.4 {
  SELECT * FROM t2 ORDER BY a;
} {1 10 100 2 20 200}
reset_db
do_execsql_test 3.1 {
  PRAGMA recursive_triggers = ON;
  CREATE TABLE t1(
      a PRIMARY KEY ON CONFLICT REPLACE,
      b UNIQUE
  );
  INSERT INTO t1 VALUES(100, 'abcde');
  CREATE TRIGGER tr1 AFTER DELETE ON t1 BEGIN
    INSERT INTO t1 VALUES(200, 'HELLO');
  END;
}
do_catchsql_test 3.2 {
  INSERT INTO t1 VALUES(100, 'HELLO');
} {1 {UNIQUE constraint failed: t1.b}}
do_execsql_test 3.3 {
  PRAGMA integrity_check;
  SELECT * FROM t1 ORDER BY +a;
} {ok 100 abcde}
finish_test
"#,
    );
}
