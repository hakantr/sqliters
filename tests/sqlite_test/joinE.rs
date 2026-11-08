#[test]
fn test_joinE() {
    sqliters_tcltest::run_test_script(
        "joinE",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
db nullvalue -
db eval {
  CREATE TABLE t1(a INT);
  INSERT INTO t1 VALUES(1),(NULL);
  CREATE TABLE t2(b INT);
  INSERT INTO t2 VALUES(2),(NULL);
}
do_execsql_test joinE-1 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON true
  ORDER BY coalesce(a,b,3);
} {
  1 2
  1 -
  - 2
  - -
}
do_execsql_test joinE-2 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON true WHERE a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  - 2
  - -
}
do_execsql_test joinE-3 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  - 2
  - -
}
do_execsql_test joinE-4 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON true WHERE b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
  - -
}
do_execsql_test joinE-5 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
  - -
}
do_execsql_test joinE-6 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON true
  ORDER BY coalesce(a,b,3);
} {
  1 2
  1 -
  - 2
  - -
}
do_execsql_test joinE-7 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON true WHERE a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  - 2
  - -
}
do_execsql_test joinE-8 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
  - 2
  - -
}
do_execsql_test joinE-9 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON true WHERE b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
  - -
}
do_execsql_test joinE-10 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
  - -
}
do_execsql_test joinE-11 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON true
  ORDER BY coalesce(a,b,3);
} {
  1 2
  1 -
  - 2
  - -
}
do_execsql_test joinE-12 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON true WHERE a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  - 2
  - -
}
do_execsql_test joinE-13 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  - 2
  - -
}
do_execsql_test joinE-14 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON true WHERE b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
  - -
}
do_execsql_test joinE-15 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
  - 2
  - -
}
do_execsql_test joinE-16 {
  SELECT a, b
  FROM t1 FULL JOIN t2 ON true
  ORDER BY coalesce(a,b,3);
} {
  1 2
  1 -
  - 2
  - -
}
do_execsql_test joinE-17 {
  SELECT a, b
  FROM t1 FULL JOIN t2 ON true WHERE a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  - 2
  - -
}
do_execsql_test joinE-19 {
  SELECT a, b
  FROM t1 FULL JOIN t2 ON true WHERE b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
  - -
}
db eval {
  DELETE FROM t1;
  INSERT INTO t1 VALUES(1);
  DELETE FROM t2;
  INSERT INTO t2 VALUES(NULL);
}
do_execsql_test joinE-21 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON true
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-22 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON true WHERE a IS NULL
  ORDER BY coalesce(a,b,3);
} {
}
do_execsql_test joinE-23 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON a IS NULL
  ORDER BY coalesce(a,b,3);
} {
}
do_execsql_test joinE-24 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON true WHERE b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-25 {
  SELECT a, b
  FROM t1 INNER JOIN t2 ON b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-26 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON true
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-27 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON true WHERE a IS NULL
  ORDER BY coalesce(a,b,3);
} {
}
do_execsql_test joinE-28 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-29 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON true WHERE b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-30 {
  SELECT a, b
  FROM t1 LEFT JOIN t2 ON b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-31 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON true
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-32 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON true WHERE a IS NULL
  ORDER BY coalesce(a,b,3);
} {
}
do_execsql_test joinE-33 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON a IS NULL
  ORDER BY coalesce(a,b,3);
} {
  - -
}
do_execsql_test joinE-34 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON true WHERE b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-35 {
  SELECT a, b
  FROM t1 RIGHT JOIN t2 ON b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-36 {
  SELECT a, b
  FROM t1 FULL JOIN t2 ON true
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
do_execsql_test joinE-37 {
  SELECT a, b
  FROM t1 FULL JOIN t2 ON true WHERE a IS NULL
  ORDER BY coalesce(a,b,3);
} {
}
do_execsql_test joinE-39 {
  SELECT a, b
  FROM t1 FULL JOIN t2 ON true WHERE b IS NULL
  ORDER BY coalesce(a,b,3);
} {
  1 -
}
finish_test
"#,
    );
}
