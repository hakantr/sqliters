#[test]
#[ignore = "101/126 subtests passed; pending: VIEW"]
fn test_in() {
    sqliters_tcltest::run_test_script("in", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test in-1.0 {
  execsql {
    BEGIN;
    CREATE TABLE t1(a int, b int);
  }
  for {set i 1} {$i<=10} {incr i} {
    execsql "INSERT INTO t1 VALUES($i,[expr {1<<$i}])"
  }
  execsql {
    COMMIT;
    SELECT count(*) FROM t1;
  }
} {10}
do_test in-1.1 {
  execsql {SELECT a FROM t1 WHERE b BETWEEN 10 AND 50 ORDER BY a}
} {4 5}
do_test in-1.2 {
  execsql {SELECT a FROM t1 WHERE b NOT BETWEEN 10 AND 50 ORDER BY a}
} {1 2 3 6 7 8 9 10}
do_test in-1.3 {
  execsql {SELECT a FROM t1 WHERE b BETWEEN a AND a*5 ORDER BY a}
} {1 2 3 4}
do_test in-1.4 {
  execsql {SELECT a FROM t1 WHERE b NOT BETWEEN a AND a*5 ORDER BY a}
} {5 6 7 8 9 10}
do_test in-1.6 {
  execsql {SELECT a FROM t1 WHERE b BETWEEN a AND a*5 OR b=512 ORDER BY a}
} {1 2 3 4 9}
do_test in-1.7 {
  execsql {SELECT a+ 100*(a BETWEEN 1 and 3) FROM t1 ORDER BY b}
} {101 102 103 4 5 6 7 8 9 10}
ifcapable !subquery {
  finish_test
  return
}
do_test in-2.1 {
  execsql {SELECT a FROM t1 WHERE b IN (8,12,16,24,32) ORDER BY a}
} {3 4 5}
do_test in-2.2 {
  execsql {SELECT a FROM t1 WHERE b NOT IN (8,12,16,24,32) ORDER BY a}
} {1 2 6 7 8 9 10}
do_test in-2.3 {
  execsql {SELECT a FROM t1 WHERE b IN (8,12,16,24,32) OR b=512 ORDER BY a}
} {3 4 5 9}
do_test in-2.4 {
  execsql {SELECT a FROM t1 WHERE b NOT IN (8,12,16,24,32) OR b=512 ORDER BY a}
} {1 2 6 7 8 9 10}
do_test in-2.5 {
  execsql {SELECT a+100*(b IN (8,16,24)) FROM t1 ORDER BY b}
} {1 2 103 104 5 6 7 8 9 10}
do_test in-2.6 {
  execsql {SELECT a FROM t1 WHERE b IN (b+8,64)}
} {6}
do_test in-2.7 {
  execsql {SELECT a FROM t1 WHERE b IN (max(5,10,b),20)}
} {4 5 6 7 8 9 10}
do_test in-2.8 {
  execsql {SELECT a FROM t1 WHERE b IN (8*2,64/2) ORDER BY b}
} {4 5}
do_test in-2.9 {
  execsql {SELECT a FROM t1 WHERE b IN (max(5,10),20)}
} {}
do_test in-2.10 {
  execsql {SELECT a FROM t1 WHERE min(0,b IN (a,30))}
} {}
do_test in-2.11 {
  set v [catch {execsql {SELECT a FROM t1 WHERE c IN (10,20)}} msg]
  lappend v $msg
} {1 {no such column: c}}
do_test in-3.1 {
  execsql {
    SELECT a FROM t1
    WHERE b IN (SELECT b FROM t1 WHERE a<5)
    ORDER BY a
  }
} {1 2 3 4}
do_test in-3.2 {
  execsql {
    SELECT a FROM t1
    WHERE b IN (SELECT b FROM t1 WHERE a<5) OR b==512
    ORDER BY a
  }
} {1 2 3 4 9}
do_test in-3.3 {
  execsql {
    SELECT a + 100*(b IN (SELECT b FROM t1 WHERE a<5)) FROM t1 ORDER BY b
  }
} {101 102 103 104 5 6 7 8 9 10}
do_test in-4.1 {
  execsql {
    UPDATE t1 SET b=b*2 
    WHERE b IN (SELECT b FROM t1 WHERE a>8)
  }
  execsql {SELECT b FROM t1 ORDER BY b}
} {2 4 8 16 32 64 128 256 1024 2048}
do_test in-4.2 {
  execsql {
    DELETE FROM t1 WHERE b IN (SELECT b FROM t1 WHERE a>8)
  }
  execsql {SELECT a FROM t1 ORDER BY a}
} {1 2 3 4 5 6 7 8}
do_test in-4.3 {
  execsql {
    DELETE FROM t1 WHERE b NOT IN (SELECT b FROM t1 WHERE a>4)
  }
  execsql {SELECT a FROM t1 ORDER BY a}
} {5 6 7 8}
do_test in-5.1 {
  execsql {
    INSERT INTO t1 VALUES('hello', 'world');
    SELECT * FROM t1
    WHERE a IN (
       'Do','an','IN','with','a','constant','RHS','but','where','the',
       'has','many','elements','We','need','to','test','that',
       'collisions','hash','table','are','resolved','properly',
       'This','in-set','contains','thirty','one','entries','hello');
  }
} {hello world}
do_test in-6.1 {
  execsql {
    CREATE TABLE ta(a INTEGER PRIMARY KEY, b);
    INSERT INTO ta VALUES(1,1);
    INSERT INTO ta VALUES(2,2);
    INSERT INTO ta VALUES(3,3);
    INSERT INTO ta VALUES(4,4);
    INSERT INTO ta VALUES(6,6);
    INSERT INTO ta VALUES(8,8);
    INSERT INTO ta VALUES(10,
       'This is a key that is long enough to require a malloc in the VDBE');
    SELECT * FROM ta WHERE a<10;
  }
} {1 1 2 2 3 3 4 4 6 6 8 8}
do_test in-6.2 {
  execsql {
    CREATE TABLE tb(a INTEGER PRIMARY KEY, b);
    INSERT INTO tb VALUES(1,1);
    INSERT INTO tb VALUES(2,2);
    INSERT INTO tb VALUES(3,3);
    INSERT INTO tb VALUES(5,5);
    INSERT INTO tb VALUES(7,7);
    INSERT INTO tb VALUES(9,9);
    INSERT INTO tb VALUES(11,
       'This is a key that is long enough to require a malloc in the VDBE');
    SELECT * FROM tb WHERE a<10;
  }
} {1 1 2 2 3 3 5 5 7 7 9 9}
do_test in-6.3 {
  execsql {
    SELECT a FROM ta WHERE b IN (SELECT a FROM tb);
  }
} {1 2 3}
do_test in-6.4 {
  execsql {
    SELECT a FROM ta WHERE b NOT IN (SELECT a FROM tb);
  }
} {4 6 8 10}
do_test in-6.5 {
  execsql {
    SELECT a FROM ta WHERE b IN (SELECT b FROM tb);
  }
} {1 2 3 10}
do_test in-6.6 {
  execsql {
    SELECT a FROM ta WHERE b NOT IN (SELECT b FROM tb);
  }
} {4 6 8}
do_test in-6.7 {
  execsql {
    SELECT a FROM ta WHERE a IN (SELECT a FROM tb);
  }
} {1 2 3}
do_test in-6.8 {
  execsql {
    SELECT a FROM ta WHERE a NOT IN (SELECT a FROM tb);
  }
} {4 6 8 10}
do_test in-6.9 {
  execsql {
    SELECT a FROM ta WHERE a IN (SELECT b FROM tb);
  }
} {1 2 3}
do_test in-6.10 {
  execsql {
    SELECT a FROM ta WHERE a NOT IN (SELECT b FROM tb);
  }
} {4 6 8 10}
do_test in-7.1 {
  execsql {
    SELECT a FROM t1 WHERE a IN ();
  }
} {}
do_test in-7.2 {
  execsql {
    SELECT a FROM t1 WHERE a IN (5);
  }
} {5}
do_test in-7.3 {
  execsql {
    SELECT a FROM t1 WHERE a NOT IN () ORDER BY a;
  }
} {5 6 7 8 hello}
do_test in-7.4 {
  execsql {
    SELECT a FROM t1 WHERE a IN (5) AND b IN ();
  }
} {}
do_test in-7.5 {
  execsql {
    SELECT a FROM t1 WHERE a IN (5) AND b NOT IN ();
  }
} {5}
do_test in-7.6.1 {
  execsql {
    SELECT a FROM ta WHERE a IN ();
  }
} {}
do_test in-7.6.2 {
  db status step
} {0}
do_test in-7.7 {
  execsql {
    SELECT a FROM ta WHERE a NOT IN ();
  }
} {1 2 3 4 6 8 10}
do_test in-7.8.1 {
  execsql {
    SELECT * FROM ta LEFT JOIN tb ON (ta.b=tb.b) WHERE ta.a IN ();
  }
} {}
do_test in-7.8.2 {
  db status step
} {0}
do_test in-8.3 {
  execsql {
    SELECT b FROM t1 WHERE a IN ('hello','there')
  }
} {world}
do_test in-8.4 {
  sqlite3_db_config db SQLITE_DBCONFIG_DQS_DML 1
  execsql {
    SELECT b FROM t1 WHERE a IN ("hello",'there')
  }
} {world}
do_test in-9.1 {
  execsql {
    CREATE TABLE t4 AS SELECT a FROM tb;
    SELECT * FROM t4;    
  }
} {1 2 3 5 7 9 11}
do_test in-9.2 {
  execsql {
    SELECT b FROM t1 WHERE a IN t4;
  }
} {32 128}
do_test in-9.3 {
  execsql {
    SELECT b FROM t1 WHERE a NOT IN t4;
  }
} {64 256 world}
do_test in-9.4 {
  catchsql {
    SELECT b FROM t1 WHERE a NOT IN tb;
  }
} {1 {sub-select returns 2 columns - expected 1}}
do_test in-10.1 {
  execsql {
    CREATE TABLE t5(
      a INTEGER,
      CHECK( a IN (111,222,333) )
    );
    INSERT INTO t5 VALUES(111);
    SELECT * FROM t5;
  }
} {111}
do_test in-10.2 {
  catchsql {
    INSERT INTO t5 VALUES(4);
  }
} {1 {CHECK constraint failed: a IN (111,222,333)}}
do_test in-11.1 {
  execsql {
    CREATE TABLE t6(a,b NUMERIC);
    INSERT INTO t6 VALUES(1,2);
    INSERT INTO t6 VALUES(2,3);
    SELECT * FROM t6 WHERE b IN (2);
  }
} {1 2}
do_test in-11.2 {
  # The '2' should be coerced into 2 because t6.b is NUMERIC
  execsql {
    SELECT * FROM t6 WHERE b IN ('2');
  }
} {1 2}
do_test in-11.3 {
  execsql {
    SELECT * FROM t6 WHERE +b IN ('2');
  }
} {}
do_test in-11.4 {
  execsql {
    SELECT * FROM t6 WHERE a IN ('2');
  }
} {}
do_test in-11.5 {
  execsql {
    SELECT * FROM t6 WHERE a IN (2);
  }
} {2 3}
do_test in-11.6 {
  execsql {
    SELECT * FROM t6 WHERE +a IN ('2');
  }
} {}
ifcapable compound {
do_test in-12.1 {
  execsql {
    CREATE TABLE t2(a, b, c);
    CREATE TABLE t3(a, b, c);
  }
} {}
do_test in-12.2 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 UNION ALL SELECT a, b FROM t2
    );
  }
} {1 {sub-select returns 2 columns - expected 1}}
do_test in-12.3 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 UNION SELECT a, b FROM t2
    );
  }
} {1 {sub-select returns 2 columns - expected 1}}
do_test in-12.4 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 EXCEPT SELECT a, b FROM t2
    );
  }
} {1 {sub-select returns 2 columns - expected 1}}
do_test in-12.5 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 INTERSECT SELECT a, b FROM t2
    );
  }
} {1 {sub-select returns 2 columns - expected 1}}
do_test in-12.6 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 UNION ALL SELECT a FROM t2
    );
  }
} {1 {SELECTs to the left and right of UNION ALL do not have the same number of result columns}}
do_test in-12.7 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 UNION SELECT a FROM t2
    );
  }
} {1 {SELECTs to the left and right of UNION do not have the same number of result columns}}
do_test in-12.8 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 EXCEPT SELECT a FROM t2
    );
  }
} {1 {SELECTs to the left and right of EXCEPT do not have the same number of result columns}}
do_test in-12.9 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 INTERSECT SELECT a FROM t2
    );
  }
} {1 {SELECTs to the left and right of INTERSECT do not have the same number of result columns}}
}
ifcapable compound {
do_test in-12.10 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a FROM t3 UNION ALL SELECT a, b FROM t2
    );
  }
} {1 {SELECTs to the left and right of UNION ALL do not have the same number of result columns}}
do_test in-12.11 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a FROM t3 UNION SELECT a, b FROM t2
    );
  }
} {1 {SELECTs to the left and right of UNION do not have the same number of result columns}}
do_test in-12.12 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a FROM t3 EXCEPT SELECT a, b FROM t2
    );
  }
} {1 {SELECTs to the left and right of EXCEPT do not have the same number of result columns}}
do_test in-12.13 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a FROM t3 INTERSECT SELECT a, b FROM t2
    );
  }
} {1 {SELECTs to the left and right of INTERSECT do not have the same number of result columns}}
do_test in-12.14 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 UNION ALL SELECT a, b FROM t2
    );
  }
} {1 {sub-select returns 2 columns - expected 1}}
do_test in-12.15 {
  catchsql {
    SELECT * FROM t2 WHERE a IN (
      SELECT a, b FROM t3 UNION ALL SELECT a FROM t2
    );
  }
} {1 {SELECTs to the left and right of UNION ALL do not have the same number of result columns}}
}; #ifcapable compound
do_test in-13.1 {
  db nullvalue "null"
  execsql { SELECT 
    1 IN (NULL, 1, 2),     -- The value 1 is a member of the set, return true.
    3 IN (NULL, 1, 2),     -- Ambiguous, return NULL.
    1 NOT IN (NULL, 1, 2), -- The value 1 is a member of the set, return false.
    3 NOT IN (NULL, 1, 2)  -- Ambiguous, return NULL.
  }
} {1 null 0 null}
do_test in-13.2 {
  execsql { 
    CREATE TABLE t7(a, b, c NOT NULL);
    INSERT INTO t7 VALUES(1,    1, 1);
    INSERT INTO t7 VALUES(2,    2, 2);
    INSERT INTO t7 VALUES(3,    3, 3);
    INSERT INTO t7 VALUES(NULL, 4, 4);
    INSERT INTO t7 VALUES(NULL, 5, 5);
  }
} {}
do_test in-13.3 {
  execsql { SELECT 2 IN (SELECT a FROM t7) }
} {1}
do_test in-13.4 {
  execsql { SELECT 6 IN (SELECT a FROM t7) }
} {null}
do_test in-13.5 {
  execsql { SELECT 2 IN (SELECT b FROM t7) }
} {1}
do_test in-13.6 {
  execsql { SELECT 6 IN (SELECT b FROM t7) }
} {0}
do_test in-13.7 {
  execsql { SELECT 2 IN (SELECT c FROM t7) }
} {1}
do_test in-13.8 {
  execsql { SELECT 6 IN (SELECT c FROM t7) }
} {0}
do_test in-13.9 {
  execsql {
    SELECT
      2 NOT IN (SELECT a FROM t7),
      6 NOT IN (SELECT a FROM t7),
      2 NOT IN (SELECT b FROM t7),
      6 NOT IN (SELECT b FROM t7),
      2 NOT IN (SELECT c FROM t7),
      6 NOT IN (SELECT c FROM t7)
  } 
} {0 null 0 1 0 1}
do_test in-13.10 {
  execsql { 
    SELECT b IN (
      SELECT inside.a 
      FROM t7 AS inside 
      WHERE inside.b BETWEEN outside.b+1 AND outside.b+2
    )
    FROM t7 AS outside ORDER BY b;
  }
} {0 null null null 0}
do_test in-13.11 {
  execsql {
    SELECT b NOT IN (
      SELECT inside.a 
      FROM t7 AS inside 
      WHERE inside.b BETWEEN outside.b+1 AND outside.b+2
    )
    FROM t7 AS outside ORDER BY b;
  }
} {1 null null null 1}
do_test in-13.12-desc {
  execsql {
    CREATE INDEX i1 ON t7(a DESC);
    CREATE INDEX i2 ON t7(b DESC);
    CREATE INDEX i3 ON t7(c DESC);
  }
  execsql {
    SELECT
      2 IN (SELECT a FROM t7),
      6 IN (SELECT a FROM t7),
      2 IN (SELECT b FROM t7),
      6 IN (SELECT b FROM t7),
      2 IN (SELECT c FROM t7),
      6 IN (SELECT c FROM t7)
  } 
} {1 null 1 0 1 0}
do_test in-13.13-desc {
  execsql {
    SELECT
      2 NOT IN (SELECT a FROM t7),
      6 NOT IN (SELECT a FROM t7),
      2 NOT IN (SELECT b FROM t7),
      6 NOT IN (SELECT b FROM t7),
      2 NOT IN (SELECT c FROM t7),
      6 NOT IN (SELECT c FROM t7)
  } 
} {0 null 0 1 0 1}
do_test in-13.12-asc {
  execsql {
    DROP INDEX i1;
    DROP INDEX i2;
    DROP INDEX i3;
    CREATE INDEX i1 ON t7(a);
    CREATE INDEX i2 ON t7(b);
    CREATE INDEX i3 ON t7(c);
  }
  execsql {
    SELECT
      2 IN (SELECT a FROM t7),
      6 IN (SELECT a FROM t7),
      2 IN (SELECT b FROM t7),
      6 IN (SELECT b FROM t7),
      2 IN (SELECT c FROM t7),
      6 IN (SELECT c FROM t7)
  } 
} {1 null 1 0 1 0}
do_test in-13.13-asc {
  execsql {
    SELECT
      2 NOT IN (SELECT a FROM t7),
      6 NOT IN (SELECT a FROM t7),
      2 NOT IN (SELECT b FROM t7),
      6 NOT IN (SELECT b FROM t7),
      2 NOT IN (SELECT c FROM t7),
      6 NOT IN (SELECT c FROM t7)
  } 
} {0 null 0 1 0 1}
do_test in-13.14 {
  execsql {
    BEGIN TRANSACTION;
    CREATE TABLE a(id INTEGER);
    INSERT INTO a VALUES(1);
    INSERT INTO a VALUES(2);
    INSERT INTO a VALUES(3);
    CREATE TABLE b(id INTEGER);
    INSERT INTO b VALUES(NULL);
    INSERT INTO b VALUES(3);
    INSERT INTO b VALUES(4);
    INSERT INTO b VALUES(5);
    COMMIT;
    SELECT * FROM a WHERE id NOT IN (SELECT id FROM b);
  }
} {}
do_test in-13.14-desc {
  execsql {
    CREATE INDEX i5 ON b(id DESC);
    SELECT * FROM a WHERE id NOT IN (SELECT id FROM b);
  }
} {}
do_execsql_test in-13.14-asc {
  DROP INDEX i5;
  CREATE INDEX i5 ON b(id DESC);
  SELECT * FROM a WHERE id NOT IN (SELECT id FROM b);
} {}
do_test in-13.15 {
  catchsql {
    SELECT 0 WHERE (SELECT 0,0) OR (0 IN (1,2));
  }
} {1 {sub-select returns 2 columns - expected 1}}
do_test in-13.X {
  db nullvalue ""
} {}
do_execsql_test in-14.0 {
  CREATE TABLE c1(a);
  INSERT INTO c1 VALUES(1), (2), (4), (3);
}
do_execsql_test in-14.1 {
  SELECT * FROM c1 WHERE a IN (SELECT a FROM c1) ORDER BY 1
} {1 2 3 4}
do_execsql_test in-15.0 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE IF NOT EXISTS t1(id INTEGER PRIMARY KEY);
  INSERT INTO t1 VALUES(1);
  SELECT a.id FROM t1 AS a JOIN t1 AS b ON a.id=b.id WHERE a.id IN (1,2,3);
} {1}
do_execsql_test in-15.1 {
  DROP TABLE IF EXISTS t2;
  CREATE TABLE t2(a INTEGER PRIMARY KEY,b);
  INSERT INTO t2 VALUES(1,11);
  INSERT INTO t2 VALUES(2,22);
  INSERT INTO t2 VALUES(3,33);
  SELECT b, a IN (3,4,5) FROM t2 ORDER BY b;
} {11 0 22 0 33 1}
do_execsql_test in-15.2 {
  DROP TABLE IF EXISTS t3;
  CREATE TABLE t3(x INTEGER PRIMARY KEY);
  INSERT INTO t3 VALUES(8);
  SELECT CASE WHEN x NOT IN (5,6,7) THEN 'yes' ELSE 'no' END FROM t3;
  SELECT CASE WHEN x NOT IN (NULL,6,7) THEN 'yes' ELSE 'no' END FROM t3;
} {yes no}
do_execsql_test in-15.3 {
  SELECT CASE WHEN x NOT IN (5,6,7) OR x=0 THEN 'yes' ELSE 'no' END FROM t3;
  SELECT CASE WHEN x NOT IN (NULL,6,7) OR x=0 THEN 'yes' ELSE 'no' END FROM t3;
} {yes no}
do_execsql_test in-15.4 {
  DROP TABLE IF EXISTS t4;
  CREATE TABLE t4(a INTEGER PRIMARY KEY, b INT);
  WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<20)
    INSERT INTO t4(a,b) SELECT x, x+100 FROM c;
  SELECT b FROM t4 WHERE a IN (3,null,8) ORDER BY +b;
} {103 108}
do_execsql_test in-15.5 {
  SELECT b FROM t4 WHERE a NOT IN (3,null,8);
} {}
do_execsql_test in-15.6 {
  DROP TABLE IF EXISTS t5;
  DROP TABLE IF EXISTS t6;
  CREATE TABLE t5(id INTEGER PRIMARY KEY, name TEXT);
  CREATE TABLE t6(id INTEGER PRIMARY KEY, name TEXT, t5_id INT);
  INSERT INTO t5 VALUES(1,'Alice'),(2,'Emma');
  INSERT INTO t6 VALUES(1,'Bob',1),(2,'Cindy',1),(3,'Dave',2);
  SELECT a.*
    FROM t5 AS 'a' JOIN t5 AS 'b' ON b.id=a.id
   WHERE b.id IN (
          SELECT t6.t5_id
            FROM t6
           WHERE name='Bob'
             AND t6.t5_id IS NOT NULL
             AND t6.id IN (
                  SELECT id
                    FROM (SELECT t6.id, count(*) AS x
                            FROM t6
                           WHERE name='Bob'
                         ) AS 't'
                   WHERE x=1
                 )
             AND t6.id IN (1,id)
         );
} {1 Alice}
reset_db
do_execsql_test in-16.0 {
  CREATE TABLE x1(a, b);
  INSERT INTO x1(a) VALUES(1), (2), (3), (4), (5), (6);
  CREATE INDEX x1i ON x1(a, b);
}
do_execsql_test in-16.1 {
  SELECT * FROM x1 
  WHERE a IN (SELECT a FROM x1 WHERE (a%2)==0) 
  ORDER BY a DESC, b;
} {6 {} 4 {} 2 {}}
do_execsql_test in-16.2 {
  SELECT * FROM x1 
  WHERE a IN (SELECT a FROM x1 WHERE (a%7)==0) 
  ORDER BY a DESC, b;
} {}
do_execsql_test in-17.1 {
  SELECT 1 IN ('1');
} 0
do_execsql_test in-17.2 {
  SELECT 1 IN ('1' COLLATE nocase);
} 0
do_execsql_test in-17.3 {
  SELECT 1 IN (CAST('1' AS text));
} 0
do_execsql_test in-17.4 {
  SELECT 1 IN (CAST('1' AS text) COLLATE nocase);
} 0
do_execsql_test in-18.1 {
  DROP TABLE IF EXISTS t0;
  CREATE TABLE t0(c0 INT UNIQUE);
  INSERT INTO t0(c0) VALUES (1);
  SELECT * FROM t0 WHERE '1' IN (t0.c0);
} {}
do_execsql_test in-19.10 {
  DROP TABLE IF EXISTS t0;
  CREATE TABLE t0(c0 REAL UNIQUE);
  INSERT INTO t0(c0) VALUES(2.0625E00);
  SELECT 1 FROM t0 WHERE c0 IN ('2.0625');
} {1}
do_execsql_test in-19.20 {
  SELECT c0 IN ('2.0625') FROM t0;
} {1}
do_execsql_test in-19.21 {
  SELECT c0 = ('2.0625') FROM t0;
} {1}
do_execsql_test in-19.22 {
  SELECT c0 = ('0.20625e+01') FROM t0;
} {1}
do_execsql_test in-19.30 {
  SELECT c0 IN ('2.0625',2,3) FROM t0;
} {1}
do_execsql_test in-19.40 {
  DROP TABLE t0;
  CREATE TABLE t0(c0 TEXT, c1 REAL, c2, PRIMARY KEY(c2, c0, c1));
  CREATE INDEX i0 ON t0(c1 IN (c0));
  INSERT INTO t0(c0, c2) VALUES (0, NULL) ON CONFLICT(c2, c1, c0) DO NOTHING;
  PRAGMA integrity_check;
} {ok}
do_execsql_test in-20.1 {
  SELECT (1 IN (2 IS TRUE));
} {1}
reset_db
do_execsql_test in-21.1 {
  CREATE TABLE t0(c0);
  SELECT COUNT(*) FROM t0 ORDER BY (t0.c0 IN ());
} {0}
do_execsql_test in-22.1 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(x INT PRIMARY KEY, y INT);
  WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<8)
    INSERT INTO t1(x,y) SELECT x, x*100 FROM c;
  DROP TABLE IF EXISTS t2;
  CREATE TABLE t2(a INT);
  INSERT INTO t2 VALUES(2),(4),(6);
  SELECT * FROM t1 WHERE x IN (SELECT a FROM t2);
} {2 200 4 400 6 600}
do_execsql_test in-22.2 {
  SELECT * FROM t1 WHERE x IN ((SELECT a FROM t2));
} {2 200 4 400 6 600}
do_execsql_test in-22.3 {
  SELECT * FROM t1 WHERE x IN (((SELECT a FROM t2)));
} {2 200 4 400 6 600}
do_execsql_test in-22.4 {
  SELECT * FROM t1 WHERE x IN ((((((SELECT a FROM t2))))));
} {2 200 4 400 6 600}
do_execsql_test in-23.0 {
  DROP TABLE IF EXISTS t4;
  CREATE TABLE t4(a TEXT, b INT);
  INSERT INTO t4(a,b) VALUES('abc',0),('ABC',1),('def',2);
  CREATE INDEX t4x ON t4(a, +a COLLATE NOCASE);
  SELECT a0.a, group_concat(a1.a) AS b
    FROM t4 AS a0 JOIN t4 AS a1
   GROUP BY a0.a
  HAVING (SELECT sum( (a1.a == +a0.a COLLATE NOCASE) IN (SELECT b FROM t4)));
} {ABC abc,ABC,def abc abc,ABC,def def abc,ABC,def}
do_execsql_test in-23.0-b {
  SELECT a0.a, group_concat(a1.a) AS b
    FROM t4 AS a0 JOIN t4 AS a1
   GROUP BY a0.a
  HAVING (SELECT sum( (a1.a GLOB +a0.a COLLATE NOCASE) IN (SELECT b FROM t4)));
} {ABC abc,ABC,def abc abc,ABC,def def abc,ABC,def}
do_execsql_test in-23.1 {
  CREATE VIEW t5 AS
    SELECT 1 AS b
     WHERE (SELECT count(0=NOT+a COLLATE NOCASE IN (SELECT 0))
              FROM t4
             GROUP BY a);
  SELECT * FROM t5;
} 1
finish_test
"#);
}
