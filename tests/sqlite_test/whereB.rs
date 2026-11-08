#[test]
fn test_whereB() {
    sqliters_tcltest::run_test_script(
        "whereB",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test whereB-1.1 {
  db eval {
    CREATE TABLE t1(x,y);    -- affinity of t1.y is NONE
    INSERT INTO t1 VALUES(1,99);

    CREATE TABLE t2(a, b TEXT);  -- affinity of t2.b is TEXT
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,99);

    SELECT x, a, y=b FROM t1, t2 ORDER BY +x, +a;
  }
} {1 2 0}
do_test whereB-1.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {}
do_test whereB-1.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {}
do_test whereB-1.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-1.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {}
do_test whereB-1.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {}
do_test whereB-1.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-2.1 {
  db eval {
    DROP TABLE t1;
    DROP TABLE t2;

    CREATE TABLE t1(x, y TEXT);    -- affinity of t1.y is TEXT
    INSERT INTO t1 VALUES(1,99);

    CREATE TABLE t2(a, b BLOB);  -- affinity of t2.b is NONE
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,99);

    SELECT x, a, y=b FROM t1, t2 ORDER BY +x, +a;
  }
} {1 2 0}
do_test whereB-2.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {}
do_test whereB-2.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {}
do_test whereB-2.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-2.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {}
do_test whereB-2.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {}
do_test whereB-2.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-3.1 {
  db eval {
    DROP TABLE t1;
    DROP TABLE t2;

    CREATE TABLE t1(x, y BLOB);    -- affinity of t1.y is NONE
    INSERT INTO t1 VALUES(1,99);

    CREATE TABLE t2(a, b BLOB);  -- affinity of t2.b is NONE
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,'99');

    SELECT x, a, y=b FROM t1, t2;
  }
} {1 2 0}
do_test whereB-3.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {}
do_test whereB-3.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {}
do_test whereB-3.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-3.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {}
do_test whereB-3.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {}
do_test whereB-3.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-4.1 {
  db eval {
    DROP TABLE t1;
    DROP TABLE t2;

    CREATE TABLE t1(x, y BLOB);    -- affinity of t1.y is NONE
    INSERT INTO t1 VALUES(1,'99');

    CREATE TABLE t2(a, b NUMERIC);  -- affinity of t2.b is NUMERIC
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,99);

    SELECT x, a, y=b FROM t1, t2;
  }
} {1 2 1}
do_test whereB-4.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-4.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-4.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-4.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-4.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-4.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-5.1 {
  db eval {
    DROP TABLE t1;
    DROP TABLE t2;

    CREATE TABLE t1(x, y BLOB);    -- affinity of t1.y is NONE
    INSERT INTO t1 VALUES(1,'99');

    CREATE TABLE t2(a, b INT);  -- affinity of t2.b is INTEGER
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,99);

    SELECT x, a, y=b FROM t1, t2;
  }
} {1 2 1}
do_test whereB-5.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-5.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-5.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-5.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-5.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-5.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-6.1 {
  db eval {
    DROP TABLE t1;
    DROP TABLE t2;

    CREATE TABLE t1(x, y BLOB);    -- affinity of t1.y is NONE
    INSERT INTO t1 VALUES(1,'99');

    CREATE TABLE t2(a, b REAL);  -- affinity of t2.b is REAL
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,99.0);

    SELECT x, a, y=b FROM t1, t2;
  }
} {1 2 1}
do_test whereB-6.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-6.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-6.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-6.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-6.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-6.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-7.1 {
  db eval {
    DROP TABLE t1;
    DROP TABLE t2;

    CREATE TABLE t1(x, y NUMERIC);  -- affinity of t1.y is NUMERIC
    INSERT INTO t1 VALUES(1,99);

    CREATE TABLE t2(a, b BLOB);  -- affinity of t2.b is NONE
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,'99');

    SELECT x, a, y=b FROM t1, t2;
  }
} {1 2 1}
do_test whereB-7.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-7.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-7.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-7.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-7.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-7.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-8.1 {
  db eval {
    DROP TABLE t1;
    DROP TABLE t2;

    CREATE TABLE t1(x, y INT);  -- affinity of t1.y is INTEGER
    INSERT INTO t1 VALUES(1,99);

    CREATE TABLE t2(a, b BLOB);  -- affinity of t2.b is NONE
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,'99');

    SELECT x, a, y=b FROM t1, t2;
  }
} {1 2 1}
do_test whereB-8.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-8.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-8.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-8.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-8.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-8.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-9.1 {
  db eval {
    DROP TABLE t1;
    DROP TABLE t2;

    CREATE TABLE t1(x, y REAL);  -- affinity of t1.y is REAL
    INSERT INTO t1 VALUES(1,99.0);

    CREATE TABLE t2(a, b BLOB);  -- affinity of t2.b is NONE
    CREATE INDEX t2b ON t2(b);
    INSERT INTO t2 VALUES(2,'99');

    SELECT x, a, y=b FROM t1, t2;
  }
} {1 2 1}
do_test whereB-9.2 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-9.3 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-9.4 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
do_test whereB-9.100 {
  db eval {
    DROP INDEX t2b;
    SELECT x, a, y=b FROM t1, t2 WHERE y=b;
  }
} {1 2 1}
do_test whereB-9.101 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE b=y;
  }
} {1 2 1}
do_test whereB-9.102 {
  db eval {
    SELECT x, a, y=b FROM t1, t2 WHERE +y=+b;
  }
} {}
finish_test
"#,
    );
}
