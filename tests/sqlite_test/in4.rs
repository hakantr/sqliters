#![forbid(unsafe_code)]

#[test]
fn test_in4() {
    sqliters_tcltest::run_test_script(
        "in4",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix in4
do_test in4-1.1 {
  execsql {
    CREATE TABLE t1(a, b);
    CREATE INDEX i1 ON t1(a);
  }
} {}
do_test in4-1.2 {
  execsql {
    SELECT * FROM t1 WHERE a IN ('aaa', 'bbb', 'ccc');
  }
} {}
do_test in4-1.3 {
  execsql {
    INSERT INTO t1 VALUES('aaa', 1);
    INSERT INTO t1 VALUES('ddd', 2);
    INSERT INTO t1 VALUES('ccc', 3);
    INSERT INTO t1 VALUES('eee', 4);
    SELECT b FROM t1 WHERE a IN ('aaa', 'bbb', 'ccc');
  }
} {1 3}
do_test in4-1.4 {
  execsql {
    SELECT a FROM t1 WHERE rowid IN (1, 3);
  }
} {aaa ccc}
do_test in4-1.5 {
  execsql {
    SELECT a FROM t1 WHERE rowid IN ();
  }
} {}
do_test in4-1.6 {
  execsql {
    SELECT a FROM t1 WHERE a IN ('ddd');
  }
} {ddd}
do_test in4-2.1 {
  execsql {
    CREATE TABLE t2(a INTEGER PRIMARY KEY, b TEXT);
    INSERT INTO t2 VALUES(-1, '-one');
    INSERT INTO t2 VALUES(0, 'zero');
    INSERT INTO t2 VALUES(1, 'one');
    INSERT INTO t2 VALUES(2, 'two');
    INSERT INTO t2 VALUES(3, 'three');
  }
} {}
do_test in4-2.2 {
  execsql { SELECT b FROM t2 WHERE a IN (0, 2) }
} {zero two}
do_test in4-2.3 {
  execsql { SELECT b FROM t2 WHERE a IN (2, 0) }
} {zero two}
do_test in4-2.4 {
  execsql { SELECT b FROM t2 WHERE a IN (2, -1) }
} {-one two}
do_test in4-2.5 {
  execsql { SELECT b FROM t2 WHERE a IN (NULL, 3) }
} {three}
do_test in4-2.6 {
  execsql { SELECT b FROM t2 WHERE a IN (1.0, 2.1) }
} {one}
do_test in4-2.7 {
  execsql { SELECT b FROM t2 WHERE a IN ('1', '2') }
} {one two}
do_test in4-2.8 {
  execsql { SELECT b FROM t2 WHERE a IN ('', '0.0.0', '2') }
} {two}
do_test in4-3.1 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(x, id);
    CREATE TABLE t2(x, id);
    INSERT INTO t1 VALUES(NULL, NULL);
    INSERT INTO t1 VALUES(0, NULL);
    INSERT INTO t1 VALUES(1, 3);
    INSERT INTO t1 VALUES(2, 4);
    INSERT INTO t1 VALUES(3, 5);
    INSERT INTO t1 VALUES(4, 6);
    INSERT INTO t2 VALUES(0, NULL);
    INSERT INTO t2 VALUES(4, 1);
    INSERT INTO t2 VALUES(NULL, 1);
    INSERT INTO t2 VALUES(NULL, NULL);
  }
} {}
do_test in4-3.2 {
  execsql {
    SELECT x FROM t1 WHERE id IN () AND x IN (SELECT x FROM t2 WHERE id=1)
  }
} {}
do_test in4-3.3 {
  execsql {
    CREATE TABLE t3(x, y, z);
    CREATE INDEX t3i1 ON t3(x, y);
    INSERT INTO t3 VALUES(1, 1, 1);
    INSERT INTO t3 VALUES(10, 10, 10);
  }
  execsql { SELECT * FROM t3 WHERE x IN () }
} {}
do_test in4-3.4 {
  execsql { SELECT * FROM t3 WHERE x = 10 AND y IN () }
} {}
do_test in4-3.5 {
  execsql { SELECT * FROM t3 WHERE x IN () AND y = 10 }
} {}
do_test in4-3.6 {
  execsql { SELECT * FROM t3 WHERE x IN () OR x = 10 }
} {10 10 10}
do_test in4-3.7 {
  execsql { SELECT * FROM t3 WHERE y IN () }
} {}
do_test in4-3.8 {
  execsql { SELECT x IN() AS a FROM t3 WHERE a }
} {}
do_test in4-3.9 {
  execsql { SELECT x IN() AS a FROM t3 WHERE NOT a }
} {0 0}
do_test in4-3.10 {
  execsql { SELECT * FROM t3 WHERE oid IN () }
} {}
do_test in4-3.11 {
  execsql { SELECT * FROM t3 WHERE x IN (1, 2) OR y IN ()}
} {1 1 1}
do_test in4-3.12 {
  execsql { SELECT * FROM t3 WHERE x IN (1, 2) AND y IN ()}
} {}
do_execsql_test in4-3.21 {
  SELECT * FROM t3 WHERE x=10 AND y IN (10);
} {10 10 10}
do_execsql_test in4-3.22 {
  SELECT * FROM t3 WHERE x IN (10) AND y=10;
} {10 10 10}
do_execsql_test in4-3.23 {
  SELECT * FROM t3 WHERE x IN (10) AND y IN (10);
} {10 10 10}
do_execsql_test in4-3.24 {
  SELECT * FROM t3 WHERE x=1 AND y NOT IN (10);
} {1 1 1}
do_execsql_test in4-3.25 {
  SELECT * FROM t3 WHERE x  NOT IN (10) AND y=1;
} {1 1 1}
do_execsql_test in4-3.26 {
  SELECT * FROM t3 WHERE x NOT IN (10) AND y NOT IN (10);
} {1 1 1}
do_execsql_test in4-3.31 {
  DROP INDEX t3i1;
  CREATE UNIQUE INDEX t3xy ON t3(x,y);

  SELECT *, '|' FROM t3 A, t3 B
   WHERE A.x=10 AND A.y IN (10)
     AND B.x=1 AND B.y IN (1);
} {10 10 10 1 1 1 |}
do_execsql_test in4-3.32 {
  EXPLAIN QUERY PLAN
  SELECT *, '|' FROM t3 A, t3 B
   WHERE A.x=10 AND A.y IN (10)
     AND B.x=1 AND B.y IN (1);
} {~/B-TREE/}  ;# No separate sorting pass
do_execsql_test in4-3.33 {
  SELECT *, '|' FROM t3 A, t3 B
   WHERE A.x IN (10) AND A.y=10
     AND B.x IN (1) AND B.y=1;
} {10 10 10 1 1 1 |}
do_execsql_test in4-3.34 {
  EXPLAIN QUERY PLAN
  SELECT *, '|' FROM t3 A, t3 B
   WHERE A.x IN (10) AND A.y=10
     AND B.x IN (1) AND B.y=1;
} {~/B-TREE/}  ;# No separate sorting pass
do_execsql_test in4-3.41 {
  SELECT * FROM t3 WHERE x IN (10,11);
} {10 10 10}
do_execsql_test in4-3.42 {
  EXPLAIN
  SELECT * FROM t3 WHERE x IN (10,11);
} {/OpenEphemeral/}
do_execsql_test in4-3.43 {
  SELECT * FROM t3 WHERE x IN (10);
} {10 10 10}
do_execsql_test in4-3.45 {
  SELECT * FROM t3 WHERE x NOT IN (10,11,99999);
} {1 1 1}
do_execsql_test in4-3.46 {
  EXPLAIN
  SELECT * FROM t3 WHERE x NOT IN (10,11,99999);
} {/OpenEphemeral/}
do_execsql_test in4-3.47 {
  SELECT * FROM t3 WHERE x NOT IN (10);
} {1 1 1}
do_execsql_test in4-3.48 {
  EXPLAIN
  SELECT * FROM t3 WHERE x NOT IN (10);
} {~/OpenEphemeral/}
do_execsql_test in4-4.1 {
  CREATE TABLE t4a(a TEXT, b TEXT COLLATE nocase, c);
  INSERT INTO t4a VALUES('ABC','abc',1);
  INSERT INTO t4a VALUES('def','xyz',2);
  INSERT INTO t4a VALUES('ghi','ghi',3);
  SELECT c FROM t4a WHERE a=b ORDER BY c;
} {3}
do_execsql_test in4-4.2 {
  SELECT c FROM t4a WHERE b=a ORDER BY c;
} {1 3}
do_execsql_test in4-4.3 {
  SELECT c FROM t4a WHERE (a||'')=b ORDER BY c;
} {1 3}
do_execsql_test in4-4.4 {
  SELECT c FROM t4a WHERE (a||'')=(b||'') ORDER BY c;
} {3}
do_execsql_test in4-4.5 {
  SELECT c FROM t4a WHERE a IN (b) ORDER BY c;
} {3}
do_execsql_test in4-4.6 {
  SELECT c FROM t4a WHERE (a||'') IN (b) ORDER BY c;
} {3}
do_execsql_test in4-4.11 {
  CREATE TABLE t4b(a TEXT, b NUMERIC, c);
  INSERT INTO t4b VALUES('1.0',1,4);
  SELECT c FROM t4b WHERE a=b;
} {4}
do_execsql_test in4-4.12 {
  SELECT c FROM t4b WHERE b=a;
} {4}
do_execsql_test in4-4.13 {
  SELECT c FROM t4b WHERE +a=b;
} {4}
do_execsql_test in4-4.14 {
  SELECT c FROM t4b WHERE a=+b;
} {}
do_execsql_test in4-4.15 {
  SELECT c FROM t4b WHERE +b=a;
} {}
do_execsql_test in4-4.16 {
  SELECT c FROM t4b WHERE b=+a;
} {4}
do_execsql_test in4-4.17 {
  SELECT c FROM t4b WHERE a IN (b);
} {}
do_execsql_test in4-4.18 {
  SELECT c FROM t4b WHERE b IN (a);
} {4}
do_execsql_test in4-4.19 {
  SELECT c FROM t4b WHERE +b IN (a);
} {}
do_execsql_test in4-5.1 {
  CREATE TABLE t5(c INTEGER PRIMARY KEY, d TEXT COLLATE nocase);
  INSERT INTO t5 VALUES(17, 'fuzz');
  SELECT 1 FROM t5 WHERE 'fuzz' IN (d);  -- match
  SELECT 2 FROM t5 WHERE 'FUZZ' IN (d);  -- no match
  SELECT 3 FROM t5 WHERE d IN ('fuzz');  -- match
  SELECT 4 FROM t5 WHERE d IN ('FUZZ');  -- match
} {1 3 4}
do_execsql_test in4-6.1 {
  CREATE TABLE t6a(a INTEGER PRIMARY KEY, b);
  INSERT INTO t6a VALUES(1,2),(3,4),(5,6);
  CREATE TABLE t6b(c INTEGER PRIMARY KEY, d);
  INSERT INTO t6b VALUES(4,44),(5,55),(6,66);

  SELECT * FROM t6a, t6b WHERE a=3 AND b IN (c);
} {3 4 4 44}
do_execsql_test in4-6.1-eqp {
  EXPLAIN QUERY PLAN
  SELECT * FROM t6a, t6b WHERE a=3 AND b IN (c);
} {~/SCAN t6a/}
do_execsql_test in4-6.2 {
  SELECT * FROM t6a, t6b WHERE a=3 AND c IN (b);
} {3 4 4 44}
do_execsql_test in4-6.2-eqp {
  EXPLAIN QUERY PLAN
  SELECT * FROM t6a, t6b WHERE a=3 AND c IN (b);
} {~/SCAN/}
reset_db
do_execsql_test 7.0 {
  CREATE TABLE t1(a, b, c);
  CREATE TABLE t2(d, e);
  CREATE INDEX t1bc ON t1(c, b);
  INSERT INTO t2(e) VALUES(1);
  INSERT INTO t1 VALUES(NULL, NULL, NULL);
}
do_execsql_test 7.1 {
  SELECT * FROM t2 LEFT JOIN t1 ON c = d AND b IN (10,10,10);
} {{} 1 {} {} {}}
ifcapable rtree {
  reset_db
  do_execsql_test 7.2 {
    CREATE VIRTUAL TABLE t1 USING rtree(a, b, c);
    CREATE TABLE t2(d INTEGER, e INT);
    INSERT INTO t2(e) VALUES(1);
  }

  do_execsql_test 7.3 {
    SELECT * FROM t2 LEFT JOIN t1 ON c IN (d) AND b IN (10,10,10);
  } {{} 1 {} {} {}}
}
reset_db
do_execsql_test 8.0 {
  CREATE TABLE t1(x INTEGER PRIMARY KEY, y);
  CREATE UNIQUE INDEX t1y ON t1(y);
  INSERT INTO t1 VALUES(111, 'AAA'),(222, 'BBB'),(333, 'CCC');
  CREATE TABLE t2(z);
  INSERT INTO t2 VALUES('BBB'),('AAA');
  ANALYZE sqlite_schema;
  INSERT INTO sqlite_stat1 VALUES('t1', 't1y','100 1');
}
db close
sqlite3 db test.db
do_execsql_test 8.1 {
  SELECT t1.x FROM t2 CROSS JOIN t1 WHERE t2.z = t1.y;
} {222 111}
do_execsql_test 8.2 {
  SELECT t1.x FROM t2 CROSS JOIN t1 WHERE t2.z = t1.y AND +t1.x IN (111, 222);
} {222 111}
do_execsql_test 8.3 {
  SELECT t1.x FROM t2 CROSS JOIN t1 WHERE t2.z = t1.y AND t1.x IN (111, 222);
} {222 111}
reset_db
do_execsql_test 9.0 {
  CREATE TABLE node(node_id INTEGER PRIMARY KEY);
  CREATE TABLE edge(node_from INT, node_to INT);
  CREATE TABLE sub_nodes(node_id INTEGER PRIMARY KEY);
  CREATE INDEX edge_from_to ON edge(node_from,node_to);
  CREATE INDEX edge_to_from ON edge(node_to,node_from);
  ANALYZE;
  DELETE FROM sqlite_stat1;
  INSERT INTO sqlite_stat1 VALUES
    ('sub_nodes',NULL,'1000000'),
    ('edge','edge_to_from','20000000 2 2'),
    ('edge','edge_from_to','20000000 2 2'),
    ('node',NULL,'10000000');
  ANALYZE sqlite_schema;
} {}
do_eqp_test 9.1 {
SELECT count(*) FROM edge
 WHERE node_from IN sub_nodes AND node_to IN sub_nodes;
} {
  QUERY PLAN
  |--SEARCH edge USING COVERING INDEX edge_to_from (node_to=?)
  |--USING ROWID SEARCH ON TABLE sub_nodes FOR IN-OPERATOR
  `--USING ROWID SEARCH ON TABLE sub_nodes FOR IN-OPERATOR
}
reset_db
do_execsql_test 10.0 {
  CREATE TABLE t1(a,b,c,d,PRIMARY KEY(a,b,c)) WITHOUT ROWID;
  INSERT INTO t1(a,b,c,d) VALUES
    (0,-2,2,3),
    (0,2,3,4),
    (0,5,8,10),
    (1,7,11,13);
  ANALYZE sqlite_schema;
  INSERT INTO sqlite_stat1 VALUES('t1','t1','10 3 2 1');
  ANALYZE sqlite_schema;
  PRAGMA reverse_unordered_selects(1);
  SELECT d FROM t1 WHERE 0=a AND b IN (-17,-4,-3,1,5,25,7798);
} {10}
reset_db
do_execsql_test 11.0 {
  CREATE TABLE t1(a TEXT, b INT, c INT, d INT);
  INSERT INTO t1 VALUES('abc',123,4,5);
  INSERT INTO t1 VALUES('xyz',1,'abcdefxyz',99);
  CREATE INDEX t1abc ON t1(b,b,c);
  ANALYZE sqlite_schema;
  INSERT INTO sqlite_stat1 VALUES('t1','t1abc','10000 5 00 2003 10');
  ANALYZE sqlite_schema;
} {}
do_execsql_test 11.1 {
  SELECT * FROM t1
   WHERE b IN (345, (SELECT 1 FROM t1 
                      WHERE b IN (coalesce(1,random()))
                        AND c GLOB 'abc*xyz'))
     AND c BETWEEN 'abc' AND 'xyz';
} {xyz 1 abcdefxyz 99}
do_execsql_test 11.2 {
  EXPLAIN SELECT * FROM t1
   WHERE b IN (345, (SELECT 1 FROM t1 
                      WHERE b IN (coalesce(1,random()))
                        AND c GLOB 'abc*xyz'))
     AND c BETWEEN 'abc' AND 'xyz';
} {/ SeekScan /}
reset_db
do_execsql_test 12.0 {
  CREATE TABLE t1(a,b,c);
  CREATE INDEX t1abc ON t1(a,b,c);
  CREATE INDEX t1bca on t1(b,c,a);
  INSERT INTO t1 VALUES(56,1119,1115);
  INSERT INTO t1 VALUES(57,1147,1137);
  INSERT INTO t1 VALUES(100,1050,1023);
  INSERT INTO t1 VALUES(101,1050,1023);
  ANALYZE sqlite_schema;
  INSERT INTO sqlite_stat1 VALUES('t1','t1abc','358677 2 2 1');
  INSERT INTO sqlite_stat1 VALUES('t1','t1bca','358677 4 2 1');
  ANALYZE sqlite_schema;
  SELECT * FROM t1 NOT INDEXED
   WHERE (b = 1137 AND c IN (97, 98))
      OR (b = 1119 AND c IN (1115, 1023));
} {56 1119 1115}
do_execsql_test 12.1 {
  SELECT * FROM t1
   WHERE (b = 1137 AND c IN (97, 98))
      OR (b = 1119 AND c IN (1115, 1023));
} {56 1119 1115}
reset_db
do_execsql_test 13.0 {
  CREATE TABLE t1(id INTEGER PRIMARY KEY, a INT, b INT, c INT);
  INSERT INTO t1 VALUES(10,1,2,5);
  INSERT INTO t1 VALUES(20,1,3,5);
  INSERT INTO t1 VALUES(30,1,2,4);
  INSERT INTO t1 VALUES(40,1,3,4);
  ANALYZE sqlite_master;
  INSERT INTO sqlite_stat1 VALUES('t1','t1x','84000 3 2 1');
  CREATE INDEX t1x ON t1(a,b,c);
  PRAGMA writable_schema=RESET;
  SELECT * FROM t1
   WHERE a=1
     AND b IN (2,3)
     AND c BETWEEN 4 AND 5
   ORDER BY +id;
} {10 1 2 5 20 1 3 5 30 1 2 4 40 1 3 4}
reset_db
do_execsql_test 14.0 {
  CREATE TABLE t1(a INTEGER PRIMARY KEY, b);
  CREATE TABLE t2(c INTEGER PRIMARY KEY, d);
  CREATE TABLE t3(e INTEGER PRIMARY KEY, f);
  CREATE INDEX t3f ON t3(f);
}
do_execsql_test 14.1 {
  EXPLAIN SELECT count(*) FROM t2 WHERE d IN (SELECT DISTINCT a FROM t1)
} [execsql {
  EXPLAIN SELECT count(*) FROM t2 WHERE d IN (SELECT a FROM t1)
}]
do_execsql_test 14.2 {
  EXPLAIN SELECT count(*) FROM t2 WHERE d IN (SELECT DISTINCT f FROM t3)
} [execsql {
  EXPLAIN SELECT count(*) FROM t2 WHERE d IN (SELECT f FROM t3)
}]
finish_test
"#,
    );
}
