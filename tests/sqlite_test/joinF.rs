#[test]
fn test_joinF() {
    sqliters_tcltest::run_test_script(
        "joinF",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
db nullvalue -
db eval {
  CREATE TABLE t1(x INT);
  CREATE TABLE t2(y INT);
  CREATE TABLE t3(z INT);
  CREATE TABLE t4(w INT);
  INSERT INTO t1 VALUES(10);
  INSERT INTO t3 VALUES(20),(30);
  INSERT INTO t4 VALUES(50);
}
do_execsql_test joinF-1 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-2 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-3 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-4 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-5 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - - 50
}
do_execsql_test joinF-6 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-7 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-8 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-9 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-10 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-11 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - - 50
}
do_execsql_test joinF-12 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-13 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-14 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-15 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-16 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-17 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-18 {
  SELECT *
  FROM t1 INNER JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-19 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-20 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-21 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-22 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-23 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - - 50
}
do_execsql_test joinF-24 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-25 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  10 - - 50
}
do_execsql_test joinF-26 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-27 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  10 - - 50
}
do_execsql_test joinF-28 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-29 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  10 - - 50
}
do_execsql_test joinF-30 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-31 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-32 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-33 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-34 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-35 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-36 {
  SELECT *
  FROM t1 LEFT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-37 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-38 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-39 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-40 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-41 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - - 50
}
do_execsql_test joinF-42 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  INNER JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-43 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-44 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-45 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-46 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-47 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - - 50
}
do_execsql_test joinF-48 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  LEFT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
}
do_execsql_test joinF-49 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-50 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  INNER JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-51 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-52 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  LEFT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-53 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
do_execsql_test joinF-54 {
  SELECT *
  FROM t1 RIGHT JOIN t2 ON true
  RIGHT JOIN t3 ON t2.y IS NOT NULL
  RIGHT JOIN t4 ON true
  WHERE (t3.z!=400 AND t3.z!=500 AND t3.z!=600)
  ORDER BY coalesce(t1.x,t2.y,t3.z,t4.w,0);
} {
  - - 20 50
  - - 30 50
}
finish_test
"#,
    );
}
