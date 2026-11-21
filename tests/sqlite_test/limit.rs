#![forbid(unsafe_code)]

#[test]
fn test_limit() {
    sqliters_tcltest::run_test_script(
        "limit",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
execsql {
  CREATE TABLE t1(x int, y int);
  BEGIN;
}
for {set i 1} {$i<=32} {incr i} {
  for {set j 0} {(1<<$j)<$i} {incr j} {}
  execsql "INSERT INTO t1 VALUES([expr {32-$i}],[expr {10-$j}])"
}
execsql {
  COMMIT;
}
do_test limit-1.0 {
  execsql {SELECT count(*) FROM t1}
} {32}
do_test limit-1.1 {
  execsql {SELECT count(*) FROM t1 LIMIT  5}
} {32}
do_test limit-1.2.1 {
  execsql {SELECT x FROM t1 ORDER BY x LIMIT 5}
} {0 1 2 3 4}
do_test limit-1.2.2 {
  execsql {SELECT x FROM t1 ORDER BY x LIMIT 5 OFFSET 2}
} {2 3 4 5 6}
do_test limit-1.2.3 {
  execsql {SELECT x FROM t1 ORDER BY x+1 LIMIT 5 OFFSET -2}
} {0 1 2 3 4}
do_test limit-1.2.4 {
  execsql {SELECT x FROM t1 ORDER BY x+1 LIMIT 2, -5}
} {2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31}
do_test limit-1.2.5 {
  execsql {SELECT x FROM t1 ORDER BY x+1 LIMIT -2, 5}
} {0 1 2 3 4}
do_test limit-1.2.6 {
  execsql {SELECT x FROM t1 ORDER BY x+1 LIMIT -2, -5}
} {0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31}
do_test limit-1.2.7 {
  execsql {SELECT x FROM t1 ORDER BY x LIMIT 2, 5}
} {2 3 4 5 6}
do_test limit-1.3 {
  execsql {SELECT x FROM t1 ORDER BY x LIMIT 5 OFFSET 5}
} {5 6 7 8 9}
do_test limit-1.4.1 {
  execsql {SELECT x FROM t1 ORDER BY x LIMIT 50 OFFSET 30}
} {30 31}
do_test limit-1.4.2 {
  execsql {SELECT x FROM t1 ORDER BY x LIMIT 30, 50}
} {30 31}
do_test limit-1.5 {
  execsql {SELECT x FROM t1 ORDER BY x LIMIT 50 OFFSET 50}
} {}
do_test limit-1.6 {
  execsql {SELECT * FROM t1 AS a, t1 AS b ORDER BY a.x, b.x LIMIT 5}
} {0 5 0 5 0 5 1 5 0 5 2 5 0 5 3 5 0 5 4 5}
do_test limit-1.7 {
  execsql {SELECT * FROM t1 AS a, t1 AS b ORDER BY a.x, b.x LIMIT 5 OFFSET 32}
} {1 5 0 5 1 5 1 5 1 5 2 5 1 5 3 5 1 5 4 5}
ifcapable {view && subquery} {
  do_test limit-2.1 {
    execsql {
      CREATE VIEW v1 AS SELECT * FROM t1 LIMIT 2;
      SELECT count(*) FROM (SELECT * FROM v1);
    }
  } 2
} ;# ifcapable view
do_test limit-2.2 {
  execsql {
    CREATE TABLE t2 AS SELECT * FROM t1 LIMIT 2;
    SELECT count(*) FROM t2;
  }
} 2
ifcapable subquery {
  do_test limit-2.3 {
    execsql {
      SELECT count(*) FROM t1 WHERE rowid IN (SELECT rowid FROM t1 LIMIT 2);
    }
  } 2
}
ifcapable subquery {
  do_test limit-3.1 {
    execsql {
      SELECT z FROM (SELECT y*10+x AS z FROM t1 ORDER BY x LIMIT 10)
      ORDER BY z LIMIT 5;
    }
  } {50 51 52 53 54}
}
do_test limit-4.1 {
  ifcapable subquery { 
    execsql {
      BEGIN;
      CREATE TABLE t3(x);
      INSERT INTO t3 SELECT x FROM t1 ORDER BY x LIMIT 10 OFFSET 1;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      INSERT INTO t3 SELECT x+(SELECT max(x) FROM t3) FROM t3;
      END;
      SELECT count(*) FROM t3;
    }
  } else {
    execsql {
      BEGIN;
      CREATE TABLE t3(x);
      INSERT INTO t3 SELECT x FROM t1 ORDER BY x LIMIT 10 OFFSET 1;
    }
    for {set i 0} {$i<10} {incr i} {
      set max_x_t3 [execsql {SELECT max(x) FROM t3}]
      execsql "INSERT INTO t3 SELECT x+$max_x_t3 FROM t3;"
    }
    execsql {
      END;
      SELECT count(*) FROM t3;
    }
  }
} {10240}
do_test limit-4.2 {
  execsql {
    SELECT x FROM t3 LIMIT 2 OFFSET 10000
  }
} {10001 10002}
do_test limit-4.3 {
  execsql {
    CREATE TABLE t4 AS SELECT x,
       'abcdefghijklmnopqrstuvwyxz ABCDEFGHIJKLMNOPQRSTUVWYXZ' || x ||
       'abcdefghijklmnopqrstuvwyxz ABCDEFGHIJKLMNOPQRSTUVWYXZ' || x ||
       'abcdefghijklmnopqrstuvwyxz ABCDEFGHIJKLMNOPQRSTUVWYXZ' || x ||
       'abcdefghijklmnopqrstuvwyxz ABCDEFGHIJKLMNOPQRSTUVWYXZ' || x ||
       'abcdefghijklmnopqrstuvwyxz ABCDEFGHIJKLMNOPQRSTUVWYXZ' || x AS y
    FROM t3 LIMIT 1000;
    SELECT x FROM t4 ORDER BY y DESC LIMIT 1 OFFSET 999;
  }
} {1000}
do_test limit-5.1 {
  execsql {
    CREATE TABLE t5(x,y);
    INSERT INTO t5 SELECT x-y, x+y FROM t1 WHERE x BETWEEN 10 AND 15
        ORDER BY x LIMIT 2;
    SELECT * FROM t5 ORDER BY x;
  }
} {5 15 6 16}
do_test limit-5.2 {
  execsql {
    DELETE FROM t5;
    INSERT INTO t5 SELECT x-y, x+y FROM t1 WHERE x BETWEEN 10 AND 15
        ORDER BY x DESC LIMIT 2;
    SELECT * FROM t5 ORDER BY x;
  }
} {9 19 10 20}
do_test limit-5.3 {
  execsql {
    DELETE FROM t5;
    INSERT INTO t5 SELECT x-y, x+y FROM t1 WHERE x ORDER BY x DESC LIMIT 31;
    SELECT * FROM t5 ORDER BY x LIMIT 2;
  }
} {-4 6 -3 7}
do_test limit-5.4 {
  execsql {
    SELECT * FROM t5 ORDER BY x DESC, y DESC LIMIT 2;
  }
} {21 41 21 39}
do_test limit-5.5 {
  execsql {
    DELETE FROM t5;
    INSERT INTO t5 SELECT a.x*100+b.x, a.y*100+b.y FROM t1 AS a, t1 AS b
                   ORDER BY 1, 2 LIMIT 1000;
    SELECT count(*), sum(x), sum(y), min(x), max(x), min(y), max(y) FROM t5;
  }
} {1000 1528204 593161 0 3107 505 1005}
do_test limit-6.1 {
  execsql {
    BEGIN;
    CREATE TABLE t6(a);
    INSERT INTO t6 VALUES(1);
    INSERT INTO t6 VALUES(2);
    INSERT INTO t6 SELECT a+2 FROM t6;
    COMMIT;
    SELECT * FROM t6;
  }
} {1 2 3 4}
do_test limit-6.2 {
  execsql {
    SELECT * FROM t6 LIMIT -1 OFFSET -1;
  }
} {1 2 3 4}
do_test limit-6.3 {
  execsql {
    SELECT * FROM t6 LIMIT 2 OFFSET -123;
  }
} {1 2}
do_test limit-6.4 {
  execsql {
    SELECT * FROM t6 LIMIT -432 OFFSET 2;
  }
} {3 4}
do_test limit-6.5 {
  execsql {
    SELECT * FROM t6 LIMIT -1
  }
} {1 2 3 4}
do_test limit-6.6 {
  execsql {
    SELECT * FROM t6 LIMIT -1 OFFSET 1
  }
} {2 3 4}
do_test limit-6.7 {
  execsql {
    SELECT * FROM t6 LIMIT 0
  }
} {}
do_test limit-6.8 {
  execsql {
    SELECT * FROM t6 LIMIT 0 OFFSET 1
  }
} {}
ifcapable compound {
do_test limit-7.1.1 {
  catchsql {
    SELECT x FROM t2 LIMIT 5 UNION ALL SELECT a FROM t6;
  }
} {1 {LIMIT clause should come after UNION ALL not before}}
do_test limit-7.1.2 {
  catchsql {
    SELECT x FROM t2 LIMIT 5 UNION SELECT a FROM t6;
  }
} {1 {LIMIT clause should come after UNION not before}}
do_test limit-7.1.3 {
  catchsql {
    SELECT x FROM t2 LIMIT 5 EXCEPT SELECT a FROM t6 LIMIT 3;
  }
} {1 {LIMIT clause should come after EXCEPT not before}}
do_test limit-7.1.4 {
  catchsql {
    SELECT x FROM t2 LIMIT 0,5 INTERSECT SELECT a FROM t6;
  }
} {1 {LIMIT clause should come after INTERSECT not before}}
do_test limit-7.2 {
  execsql {
    SELECT x FROM t2 UNION ALL SELECT a FROM t6 LIMIT 5;
  }
} {31 30 1 2 3}
do_test limit-7.3 {
  execsql {
    SELECT x FROM t2 UNION ALL SELECT a FROM t6 LIMIT 3 OFFSET 1;
  }
} {30 1 2}
do_test limit-7.4 {
  execsql {
    SELECT x FROM t2 UNION ALL SELECT a FROM t6 ORDER BY 1 LIMIT 3 OFFSET 1;
  }
} {2 3 4}
do_test limit-7.5 {
  execsql {
    SELECT x FROM t2 UNION SELECT x+2 FROM t2 LIMIT 2 OFFSET 1;
  }
} {31 32}
do_test limit-7.6 {
  execsql {
    SELECT x FROM t2 UNION SELECT x+2 FROM t2 ORDER BY 1 DESC LIMIT 2 OFFSET 1;
  }
} {32 31}
do_test limit-7.7 {
  execsql {
    SELECT a+9 FROM t6 EXCEPT SELECT y FROM t2 LIMIT 2;
  }
} {11 12}
do_test limit-7.8 {
  execsql {
    SELECT a+9 FROM t6 EXCEPT SELECT y FROM t2 ORDER BY 1 DESC LIMIT 2;
  }
} {13 12}
do_test limit-7.9 {
  execsql {
    SELECT a+26 FROM t6 INTERSECT SELECT x FROM t2 LIMIT 1;
  }
} {30}
do_test limit-7.10 {
  execsql {
    SELECT a+27 FROM t6 INTERSECT SELECT x FROM t2 LIMIT 1;
  }
} {30}
do_test limit-7.11 {
  execsql {
    SELECT a+27 FROM t6 INTERSECT SELECT x FROM t2 LIMIT 1 OFFSET 1;
  }
} {31}
do_test limit-7.12 {
  execsql {
    SELECT a+27 FROM t6 INTERSECT SELECT x FROM t2 
       ORDER BY 1 DESC LIMIT 1 OFFSET 1;
  }
} {30}
} ;# ifcapable compound
do_test limit-8.1 {
  execsql {
    SELECT DISTINCT cast(round(x/100) as integer) FROM t3 LIMIT 5;
  }
} {0 1 2 3 4}
do_test limit-8.2 {
  execsql {
    SELECT DISTINCT cast(round(x/100) as integer) FROM t3 LIMIT 5 OFFSET 5;
  }
} {5 6 7 8 9}
do_test limit-8.3 {
  execsql {
    SELECT DISTINCT cast(round(x/100) as integer) FROM t3 LIMIT 5 OFFSET 25;
  }
} {25 26 27 28 29}
ifcapable subquery {
  do_test limit-9.1 {
    execsql {
      SELECT * FROM (SELECT * FROM t6 LIMIT 3);
    }
  } {1 2 3}
}
do_test limit-9.2.1 {
  execsql {
    CREATE TABLE t7 AS SELECT * FROM t6;
  }
} {}
ifcapable subquery {
  do_test limit-9.2.2 {
    execsql {
      SELECT * FROM (SELECT * FROM t7 LIMIT 3);
    }
  } {1 2 3}
}
ifcapable compound {
  ifcapable subquery {
    do_test limit-9.3 {
      execsql {
        SELECT * FROM (SELECT * FROM t6 LIMIT 3)
        UNION
        SELECT * FROM (SELECT * FROM t7 LIMIT 3)
        ORDER BY 1
      }
    } {1 2 3}
    do_test limit-9.4 {
      execsql {
        SELECT * FROM (SELECT * FROM t6 LIMIT 3)
        UNION
        SELECT * FROM (SELECT * FROM t7 LIMIT 3)
        ORDER BY 1
        LIMIT 2
      }
    } {1 2}
  }
  do_test limit-9.5 {
    catchsql {
      SELECT * FROM t6 LIMIT 3
      UNION
      SELECT * FROM t7 LIMIT 3
    }
  } {1 {LIMIT clause should come after UNION not before}}
}
do_test limit-10.1 {
  set limit 10
  db eval {
    SELECT x FROM t1 LIMIT :limit;
  }
} {31 30 29 28 27 26 25 24 23 22}
do_test limit-10.2 {
  set limit 5
  set offset 5
  db eval {
    SELECT x FROM t1 LIMIT :limit OFFSET :offset;
  }
} {26 25 24 23 22}
do_test limit-10.3 {
  set limit -1
  db eval {
    SELECT x FROM t1 WHERE x<10 LIMIT :limit;
  }
} {9 8 7 6 5 4 3 2 1 0}
do_test limit-10.4 {
  set limit 1.5
  set rc [catch {
  db eval {
    SELECT x FROM t1 WHERE x<10 LIMIT :limit;
  } } msg]
  list $rc $msg
} {1 {datatype mismatch}}
do_test limit-10.5 {
  set limit "hello world"
  set rc [catch {
  db eval {
    SELECT x FROM t1 WHERE x<10 LIMIT :limit;
  } } msg]
  list $rc $msg
} {1 {datatype mismatch}}
ifcapable subquery {
do_test limit-11.1 {
  db eval {
     SELECT x FROM (SELECT x FROM t1 ORDER BY x LIMIT 0) ORDER BY x
  }
} {}
} ;# ifcapable subquery
do_test limit-12.1 {
  catchsql {
     SELECT * FROM t1 LIMIT replace(1)
  }
} {1 {wrong number of arguments to function replace()}}
do_test limit-12.2 {
  catchsql {
     SELECT * FROM t1 LIMIT 5 OFFSET replace(1)
  }
} {1 {wrong number of arguments to function replace()}}
do_test limit-12.3 {
  catchsql {
     SELECT * FROM t1 LIMIT x
  }
} {1 {no such column: x}}
do_test limit-12.4 {
  catchsql {
     SELECT * FROM t1 LIMIT 1 OFFSET x
  }
} {1 {no such column: x}}
do_test limit-13.1 {
  db eval {
    CREATE TABLE t13(x);
    INSERT INTO t13 VALUES(1),(2);
    CREATE VIEW v13a AS SELECT x AS y FROM t13;
    CREATE VIEW v13b AS SELECT y AS z FROM v13a UNION ALL SELECT y+10 FROM v13a;
    CREATE VIEW v13c AS SELECT z FROM v13b UNION ALL SELECT z+20 FROM v13b;
  }
} {}
do_test limit-13.2 {
  db eval {SELECT z FROM v13c LIMIT 1}
} {1}
do_test limit-13.3 {
  db eval {SELECT z FROM v13c LIMIT 2}
} {1 2}
do_test limit-13.4 {
  db eval {SELECT z FROM v13c LIMIT 3}
} {1 2 11}
do_test limit-13.5 {
  db eval {SELECT z FROM v13c LIMIT 4}
} {1 2 11 12}
do_test limit-13.6 {
  db eval {SELECT z FROM v13c LIMIT 5}
} {1 2 11 12 21}
do_test limit-13.7 {
  db eval {SELECT z FROM v13c LIMIT 6}
} {1 2 11 12 21 22}
do_test limit-13.8 {
  db eval {SELECT z FROM v13c LIMIT 7}
} {1 2 11 12 21 22 31}
do_test limit-13.9 {
  db eval {SELECT z FROM v13c LIMIT 8}
} {1 2 11 12 21 22 31 32}
do_test limit-13.10 {
  db eval {SELECT z FROM v13c LIMIT 9}
} {1 2 11 12 21 22 31 32}
do_test limit-13.11 {
  db eval {SELECT z FROM v13c LIMIT 1 OFFSET 1}
} {2}
do_test limit-13.12 {
  db eval {SELECT z FROM v13c LIMIT 2 OFFSET 1}
} {2 11}
do_test limit-13.13 {
  db eval {SELECT z FROM v13c LIMIT 3 OFFSET 1}
} {2 11 12}
do_test limit-13.14 {
  db eval {SELECT z FROM v13c LIMIT 4 OFFSET 1}
} {2 11 12 21}
do_test limit-13.15 {
  db eval {SELECT z FROM v13c LIMIT 5 OFFSET 1}
} {2 11 12 21 22}
do_test limit-13.16 {
  db eval {SELECT z FROM v13c LIMIT 6 OFFSET 1}
} {2 11 12 21 22 31}
do_test limit-13.17 {
  db eval {SELECT z FROM v13c LIMIT 7 OFFSET 1}
} {2 11 12 21 22 31 32}
do_test limit-13.18 {
  db eval {SELECT z FROM v13c LIMIT 8 OFFSET 1}
} {2 11 12 21 22 31 32}
do_test limit-13.21 {
  db eval {SELECT z FROM v13c LIMIT 1 OFFSET 2}
} {11}
do_test limit-13.22 {
  db eval {SELECT z FROM v13c LIMIT 2 OFFSET 2}
} {11 12}
do_test limit-13.23 {
  db eval {SELECT z FROM v13c LIMIT 3 OFFSET 2}
} {11 12 21}
do_test limit-13.24 {
  db eval {SELECT z FROM v13c LIMIT 4 OFFSET 2}
} {11 12 21 22}
do_test limit-13.25 {
  db eval {SELECT z FROM v13c LIMIT 5 OFFSET 2}
} {11 12 21 22 31}
do_test limit-13.26 {
  db eval {SELECT z FROM v13c LIMIT 6 OFFSET 2}
} {11 12 21 22 31 32}
do_test limit-13.27 {
  db eval {SELECT z FROM v13c LIMIT 7 OFFSET 2}
} {11 12 21 22 31 32}
do_test limit-13.31 {
  db eval {SELECT z FROM v13c LIMIT 1 OFFSET 3}
} {12}
do_test limit-13.32 {
  db eval {SELECT z FROM v13c LIMIT 2 OFFSET 3}
} {12 21}
do_test limit-13.33 {
  db eval {SELECT z FROM v13c LIMIT 3 OFFSET 3}
} {12 21 22}
do_test limit-13.34 {
  db eval {SELECT z FROM v13c LIMIT 4 OFFSET 3}
} {12 21 22 31}
do_test limit-13.35 {
  db eval {SELECT z FROM v13c LIMIT 5 OFFSET 3}
} {12 21 22 31 32}
do_test limit-13.36 {
  db eval {SELECT z FROM v13c LIMIT 6 OFFSET 3}
} {12 21 22 31 32}
do_test limit-13.41 {
  db eval {SELECT z FROM v13c LIMIT 1 OFFSET 4}
} {21}
do_test limit-13.42 {
  db eval {SELECT z FROM v13c LIMIT 2 OFFSET 4}
} {21 22}
do_test limit-13.43 {
  db eval {SELECT z FROM v13c LIMIT 3 OFFSET 4}
} {21 22 31}
do_test limit-13.44 {
  db eval {SELECT z FROM v13c LIMIT 4 OFFSET 4}
} {21 22 31 32}
do_test limit-13.45 {
  db eval {SELECT z FROM v13c LIMIT 5 OFFSET 4}
} {21 22 31 32}
do_test limit-13.51 {
  db eval {SELECT z FROM v13c LIMIT 1 OFFSET 5}
} {22}
do_test limit-13.52 {
  db eval {SELECT z FROM v13c LIMIT 2 OFFSET 5}
} {22 31}
do_test limit-13.53 {
  db eval {SELECT z FROM v13c LIMIT 3 OFFSET 5}
} {22 31 32}
do_test limit-13.54 {
  db eval {SELECT z FROM v13c LIMIT 4 OFFSET 5}
} {22 31 32}
do_test limit-13.61 {
  db eval {SELECT z FROM v13c LIMIT 1 OFFSET 6}
} {31}
do_test limit-13.62 {
  db eval {SELECT z FROM v13c LIMIT 2 OFFSET 6}
} {31 32}
do_test limit-13.63 {
  db eval {SELECT z FROM v13c LIMIT 3 OFFSET 6}
} {31 32}
do_test limit-13.71 {
  db eval {SELECT z FROM v13c LIMIT 1 OFFSET 7}
} {32}
do_test limit-13.72 {
  db eval {SELECT z FROM v13c LIMIT 2 OFFSET 7}
} {32}
do_test limit-13.81 {
  db eval {SELECT z FROM v13c LIMIT 1 OFFSET 8}
} {}
do_execsql_test limit-14.1 {
  SELECT 123 LIMIT 1 OFFSET 0
} {123}
do_execsql_test limit-14.2 {
  SELECT 123 LIMIT 1 OFFSET 1
} {}
do_execsql_test limit-14.3 {
  SELECT 123 LIMIT 0 OFFSET 0
} {}
do_execsql_test limit-14.4 {
  SELECT 123 LIMIT 0 OFFSET 1
} {}
do_execsql_test limit-14.6 {
  SELECT 123 LIMIT -1 OFFSET 0
} {123}
do_execsql_test limit-14.7 {
  SELECT 123 LIMIT -1 OFFSET 1
} {}
reset_db
do_execsql_test limit-15.1 {
  CREATE TABLE t1(a PRIMARY KEY, b TEXT);
  CREATE TABLE t4(c PRIMARY KEY, d);
  CREATE TABLE t5(e PRIMARY KEY, f);
  CREATE TABLE t6(g, h);
  CREATE TABLE t3_a(k, v);
  CREATE TABLE t3_b(k, v);
  CREATE VIEW t3 AS SELECT * FROM t3_a UNION ALL SELECT * FROM t3_b;
  INSERT INTO t5(e,f) VALUES(500000,'orange');
  INSERT INTO t4(c,d) VALUES(300000,'blue'),(400,'green'),(8000,'grey');
  INSERT INTO t1(a,b) VALUES(300000,'purple');
  INSERT INTO t3_a VALUES(300000,'yellow'),(500,'pink'),(8000,'red');
  INSERT INTO t6 default values;
  SELECT (
      SELECT 100000 FROM
          (SELECT 200000 FROM t6 WHERE a = ( SELECT 300000 FROM t3 WHERE a ) ),
          (SELECT 400000 FROM t5 WHERE e=500000),
          (SELECT 600000 FROM t4 WHERE c=a)
  ) FROM t1;
} {100000}
finish_test
"#,
    );
}

#[test]
fn test_materialized_limit_constraints() {
    sqliters_tcltest::run_test_script(
        "materialized_limit",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
execsql {
  CREATE TABLE item(id INTEGER PRIMARY KEY, label TEXT);
  INSERT INTO item VALUES(1,'a'),(2,'b'),(3,'a'),(4,'c');
}
do_execsql_test materialized-limit-1 {
  SELECT DISTINCT label FROM item ORDER BY label LIMIT '2e0' OFFSET '1.0';
} {b c}
do_catchsql_test materialized-limit-2 {
  SELECT DISTINCT label FROM item ORDER BY label LIMIT 1.5;
} {1 {datatype mismatch}}
do_catchsql_test materialized-limit-3 {
  SELECT label,count(*) FROM item GROUP BY label ORDER BY label LIMIT 1 OFFSET NULL;
} {1 {datatype mismatch}}
do_execsql_test materialized-limit-4 {
  SELECT id FROM item UNION ALL SELECT id FROM item ORDER BY 1 LIMIT 0 OFFSET 'unused';
} {}
do_execsql_test materialized-limit-5 {
  WITH RECURSIVE seq(x) AS (
    VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<5 LIMIT '2e0' OFFSET '1'
  ) SELECT x FROM seq ORDER BY x;
} {2 3}
finish_test
"#,
    );
}

#[test]
fn test_ordinary_limit_evaluation_order() {
    sqliters_tcltest::run_test_script(
        "ordinary_limit_order",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
execsql {
  CREATE TABLE item(id INTEGER PRIMARY KEY, label TEXT);
  INSERT INTO item VALUES(1,'a'),(2,'b'),(3,'a');
  CREATE TABLE detail(id INTEGER PRIMARY KEY);
  INSERT INTO detail VALUES(1),(2);
}
do_execsql_test ordinary-limit-order-1 {
  SELECT DISTINCT abs(-9223372036854775808) FROM item LIMIT 0 OFFSET NULL;
} {}
do_catchsql_test ordinary-limit-order-2 {
  SELECT abs(-9223372036854775808),count(*) FROM item GROUP BY label LIMIT 1.5;
} {1 {datatype mismatch}}
do_catchsql_test ordinary-limit-order-3 {
  SELECT abs(-9223372036854775808) FROM item JOIN detail USING(id) LIMIT 1 OFFSET NULL;
} {1 {datatype mismatch}}
do_execsql_test ordinary-limit-order-4 {
  SELECT DISTINCT i.* FROM item AS i JOIN detail AS d ON d.id=i.id ORDER BY i.id LIMIT 0;
} {}
do_execsql_test ordinary-limit-order-5 {
  SELECT label,count(*) FROM item GROUP BY label ORDER BY label LIMIT '1e0' OFFSET 1;
} {b 1}
finish_test
"#,
    );
}

#[test]
fn test_compound_limit_evaluation_order() {
    sqliters_tcltest::run_test_script(
        "compound_limit_order",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
execsql {
  CREATE TABLE item(id INTEGER PRIMARY KEY);
  INSERT INTO item VALUES(1),(2),(3);
  CREATE TABLE detail(id INTEGER PRIMARY KEY);
  INSERT INTO detail VALUES(1),(3),(4);
}
do_execsql_test compound-limit-order-1 {
  SELECT abs(-9223372036854775808) FROM item UNION ALL SELECT id FROM detail LIMIT 0;
} {}
do_catchsql_test compound-limit-order-2 {
  SELECT id FROM item UNION SELECT abs(-9223372036854775808) FROM detail ORDER BY 1 LIMIT 1.5;
} {1 {datatype mismatch}}
do_catchsql_test compound-limit-order-3 {
  SELECT abs(-9223372036854775808) FROM item INTERSECT SELECT id FROM detail LIMIT 2 OFFSET NULL;
} {1 {datatype mismatch}}
do_execsql_test compound-limit-order-4 {
  SELECT id FROM item EXCEPT SELECT abs(-9223372036854775808) FROM detail ORDER BY 1 LIMIT 0 OFFSET 'unused';
} {}
do_execsql_test compound-limit-order-5 {
  SELECT id FROM item UNION SELECT id FROM detail ORDER BY 1 LIMIT '2e0' OFFSET 1.0;
} {2 3}
finish_test
"#,
    );
}

#[test]
fn test_subquery_limit_evaluation_order() {
    sqliters_tcltest::run_test_script(
        "subquery_limit_order",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
execsql {
  CREATE TABLE item(id INTEGER PRIMARY KEY, label TEXT);
  INSERT INTO item VALUES(1,'a'),(2,'b'),(3,'c');
}
do_execsql_test subquery-limit-order-1 {
  SELECT value FROM (SELECT value FROM (SELECT abs(-9223372036854775808) AS value FROM item)) LIMIT 0;
} {}
do_catchsql_test subquery-limit-order-2 {
  SELECT value FROM (SELECT abs(-9223372036854775808) AS value FROM item ORDER BY id) LIMIT 1.5;
} {1 {datatype mismatch}}
do_catchsql_test subquery-limit-order-3 {
  SELECT value FROM (SELECT abs(-9223372036854775808) AS value) LIMIT 1 OFFSET NULL;
} {1 {datatype mismatch}}
do_execsql_test subquery-limit-order-4 {
  SELECT q.* FROM (SELECT id AS repeated,label AS repeated FROM item) AS q LIMIT 0 OFFSET 'unused';
} {}
do_execsql_test subquery-limit-order-5 {
  SELECT q.id,q.label FROM (SELECT * FROM item) AS q ORDER BY q.id LIMIT 1 OFFSET 1;
} {2 b}
finish_test
"#,
    );
}

#[test]
fn test_cte_limit_evaluation_order() {
    sqliters_tcltest::run_test_script(
        "cte_limit_order",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
execsql {
  CREATE TABLE item(id INTEGER PRIMARY KEY);
  INSERT INTO item VALUES(1),(2),(3);
  CREATE TABLE detail(id INTEGER PRIMARY KEY);
  INSERT INTO detail VALUES(1),(2);
}
do_execsql_test cte-limit-order-1 {
  WITH named AS MATERIALIZED (SELECT abs(-9223372036854775808) AS value FROM item)
  SELECT value FROM named LIMIT 0 OFFSET 'unused';
} {}
do_catchsql_test cte-limit-order-2 {
  WITH first AS (SELECT abs(-9223372036854775808) AS value FROM item),
       second AS (SELECT value FROM first)
  SELECT value FROM second ORDER BY value LIMIT 1.5;
} {1 {datatype mismatch}}
do_catchsql_test cte-limit-order-3 {
  WITH named AS (SELECT id,abs(-9223372036854775808) AS value FROM item)
  SELECT value FROM named JOIN detail USING(id) LIMIT 1 OFFSET NULL;
} {1 {datatype mismatch}}
do_execsql_test cte-limit-order-4 {
  WITH named(value) AS (SELECT 'ordinary')
  SELECT value FROM named LIMIT 1;
} {ordinary}
proc count_limit {value} {
  incr ::limit_calls
  return $value
}
db function count_limit count_limit
do_test cte-limit-order-5 {
  set ::limit_calls 0
  set rows [execsql {
    WITH named AS (SELECT id FROM item ORDER BY id)
    SELECT id FROM named LIMIT count_limit(1) OFFSET count_limit(1);
  }]
  list $rows $::limit_calls
} {2 2}
do_test cte-limit-order-6 {
  set ::limit_calls 0
  set rows [execsql {
    WITH named AS (SELECT abs(-9223372036854775808) AS value FROM item)
    SELECT value FROM named LIMIT count_limit(0) OFFSET count_limit(1);
  }]
  list $rows $::limit_calls
} {{} 1}
finish_test
"#,
    );
}

#[test]
fn test_union_all_unused_arms() {
    sqliters_tcltest::run_test_script(
        "union_all_unused_arms",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
execsql {
  CREATE TABLE head(id INTEGER PRIMARY KEY);
  INSERT INTO head VALUES(1),(2),(3);
  CREATE TABLE tail(id INTEGER PRIMARY KEY);
  INSERT INTO tail VALUES(4),(5),(6);
  CREATE TABLE last(id INTEGER PRIMARY KEY);
  INSERT INTO last VALUES(7),(8);
}
do_execsql_test union-all-arms-1 {
  SELECT id FROM head UNION ALL SELECT abs(-9223372036854775808) FROM tail LIMIT 1;
} {1}
do_execsql_test union-all-arms-2 {
  SELECT id FROM head UNION ALL SELECT abs(-9223372036854775808) FROM tail LIMIT 2 OFFSET 1;
} {2 3}
do_execsql_test union-all-arms-3 {
  SELECT id FROM head UNION ALL SELECT id FROM tail
  UNION ALL SELECT abs(-9223372036854775808) FROM last LIMIT 4;
} {1 2 3 4}
do_catchsql_test union-all-arms-4 {
  SELECT id FROM head UNION ALL SELECT abs(-9223372036854775808) FROM tail LIMIT 1 OFFSET 3;
} {1 {integer overflow}}
do_catchsql_test union-all-arms-5 {
  SELECT id FROM head UNION ALL SELECT abs(-9223372036854775808) FROM tail ORDER BY 1 LIMIT 1;
} {1 {integer overflow}}
finish_test
"#,
    );
}

#[test]
fn test_ordered_recursive_cte_limit() {
    sqliters_tcltest::run_test_script(
        "ordered_recursive_cte_limit",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_execsql_test recursive-limit-order-1 {
  WITH RECURSIVE seq(x) AS (
    VALUES(abs(-9223372036854775808)) UNION ALL SELECT x+1 FROM seq WHERE x<4
  ) SELECT x FROM seq ORDER BY x LIMIT 0 OFFSET 'unused';
} {}
do_catchsql_test recursive-limit-order-2 {
  WITH RECURSIVE seq(x) AS (
    VALUES(1) UNION ALL SELECT abs(-9223372036854775808) FROM seq WHERE x<4
  ) SELECT x FROM seq ORDER BY x LIMIT 1.5;
} {1 {datatype mismatch}}
do_catchsql_test recursive-limit-order-3 {
  WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<4)
  SELECT abs(-9223372036854775808),x FROM seq ORDER BY x LIMIT 1 OFFSET NULL;
} {1 {datatype mismatch}}
do_execsql_test recursive-limit-order-4 {
  WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<4)
  SELECT x FROM seq ORDER BY x DESC LIMIT '2e0' OFFSET 1;
} {3 2}
do_execsql_test recursive-limit-order-5 {
  WITH RECURSIVE seq(x) AS (VALUES(1) UNION SELECT x+1 FROM seq WHERE x<4)
  SELECT abs(-9223372036854775808) FROM seq ORDER BY x LIMIT 0;
} {}
finish_test
"#,
    );
}

#[test]
fn test_recursive_stream_cte_limit() {
    sqliters_tcltest::run_test_script(
        "recursive_stream_cte_limit",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc count_limit {value} {
  incr ::limit_calls
  return $value
}
db function count_limit count_limit
do_test recursive-stream-limit-1 {
  set ::limit_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<4)
    SELECT abs(-9223372036854775808) FROM seq
    LIMIT count_limit(0) OFFSET count_limit(1);
  }]
  list $rows $::limit_calls
} {{} 1}
do_test recursive-stream-limit-2 {
  set ::limit_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<4)
    SELECT x FROM seq LIMIT count_limit(2) OFFSET count_limit(1);
  }]
  list $rows $::limit_calls
} {{2 3} 2}
do_test recursive-stream-limit-3 {
  set ::limit_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<4)
    SELECT x FROM seq LIMIT count_limit(-1) OFFSET count_limit(1);
  }]
  list $rows $::limit_calls
} {{2 3 4} 2}
do_test recursive-stream-limit-4 {
  set ::limit_calls 0
  set code [catch {execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<4)
    SELECT x FROM seq LIMIT count_limit('wrong') OFFSET count_limit(1);
  }} message]
  list $code $message $::limit_calls
} {1 {datatype mismatch} 1}
do_test recursive-stream-limit-5 {
  set ::limit_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<4)
    SELECT x FROM seq LIMIT count_limit(1) OFFSET count_limit(-1);
  }]
  list $rows $::limit_calls
} {1 2}
finish_test
"#,
    );
}

#[test]
fn test_recursive_stream_filter_once() {
    sqliters_tcltest::run_test_script(
        "recursive_stream_filter_once",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc count_even {value} {
  incr ::filter_calls
  return [expr {$value % 2 == 0}]
}
db function count_even count_even
do_test recursive-where-c-1 {
  set ::filter_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<8)
    SELECT x FROM seq WHERE count_even(x) LIMIT 2 OFFSET 1;
  }]
  list $rows $::filter_calls
} {{4 6} 6}
do_test recursive-where-c-2 {
  set ::filter_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<8)
    SELECT x FROM seq WHERE count_even(x) LIMIT 3 OFFSET 3;
  }]
  list $rows $::filter_calls
} {8 8}
do_test recursive-where-c-3 {
  set ::filter_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<8)
    SELECT x FROM seq WHERE count_even(x) LIMIT 1 OFFSET -1;
  }]
  list $rows $::filter_calls
} {2 2}
do_test recursive-where-c-4 {
  set ::filter_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<8)
    SELECT x FROM seq WHERE count_even(x) LIMIT 0 OFFSET NULL;
  }]
  list $rows $::filter_calls
} {{} 0}
do_test recursive-where-c-5 {
  set ::filter_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<8)
    SELECT x FROM seq WHERE count_even(x) AND x>8 LIMIT 2;
  }]
  list $rows $::filter_calls
} {{} 8}
do_test recursive-where-c-6 {
  set ::filter_calls 0
  set rows [execsql {
    WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x<8)
    SELECT x FROM seq WHERE count_even(x) LIMIT 1 OFFSET 8;
  }]
  list $rows $::filter_calls
} {{} 8}
finish_test
"#,
    );
}

#[test]
fn test_union_literal_limit_one() {
    sqliters_tcltest::run_test_script(
        "union_literal_limit_one",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
execsql {
  CREATE TABLE first_arm(id INTEGER PRIMARY KEY, label TEXT);
  INSERT INTO first_arm VALUES(10,'delta'),(20,'alpha'),(30,'omega');
  CREATE TABLE second_arm(id INTEGER PRIMARY KEY, label TEXT);
  INSERT INTO second_arm VALUES(10,'beta'),(20,'aardvark');
}
do_execsql_test union-limit-one-1 {
  SELECT label FROM first_arm UNION SELECT label FROM second_arm LIMIT 1;
} {delta}
do_execsql_test union-limit-one-2 {
  SELECT label FROM first_arm UNION SELECT label FROM second_arm LIMIT -(-1);
} {delta}
do_execsql_test union-limit-one-3 {
  SELECT label FROM first_arm UNION SELECT label FROM second_arm LIMIT 1 OFFSET 0;
} {aardvark}
do_execsql_test union-limit-one-4 {
  SELECT label FROM first_arm UNION SELECT label FROM second_arm LIMIT '1';
} {aardvark}
do_execsql_test union-limit-one-5 {
  SELECT label FROM first_arm UNION SELECT abs(-9223372036854775808) FROM second_arm LIMIT 1;
} {delta}
do_execsql_test union-limit-one-6 {
  SELECT label FROM first_arm WHERE id<0 UNION SELECT label FROM second_arm LIMIT 1;
} {beta}
do_execsql_test union-limit-one-7 {
  SELECT label FROM first_arm UNION SELECT label FROM second_arm ORDER BY 1 LIMIT 1;
} {aardvark}
finish_test
"#,
    );
}
