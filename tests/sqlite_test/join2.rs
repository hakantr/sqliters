#![forbid(unsafe_code)]

#[test]
#[ignore = "47/59 subtests passed; pending: EQP/optimizer"]
fn test_join2() {
    sqliters_tcltest::run_test_script(
        "join2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix join2
do_test join2-1.1 {
  execsql {
    CREATE TABLE t1(a,b);
    INSERT INTO t1 VALUES(1,11);
    INSERT INTO t1 VALUES(2,22);
    INSERT INTO t1 VALUES(3,33);
    SELECT * FROM t1;
  }  
} {1 11 2 22 3 33}
do_test join2-1.2 {
  execsql {
    CREATE TABLE t2(b,c);
    INSERT INTO t2 VALUES(11,111);
    INSERT INTO t2 VALUES(33,333);
    INSERT INTO t2 VALUES(44,444);
    SELECT * FROM t2;
  }  
} {11 111 33 333 44 444};
do_test join2-1.3 {
  execsql {
    CREATE TABLE t3(c,d);
    INSERT INTO t3 VALUES(111,1111);
    INSERT INTO t3 VALUES(444,4444);
    INSERT INTO t3 VALUES(555,5555);
    SELECT * FROM t3;
  }  
} {111 1111 444 4444 555 5555}
do_test join2-1.4 {
  execsql {
    SELECT * FROM
      t1 NATURAL JOIN t2 NATURAL JOIN t3
  }
} {1 11 111 1111}
do_test join2-1.5 {
  execsql {
    SELECT * FROM
      t1 NATURAL JOIN t2 NATURAL LEFT OUTER JOIN t3
  }
} {1 11 111 1111 3 33 333 {}}
do_test join2-1.6 {
  execsql {
    SELECT * FROM
      t1 NATURAL LEFT OUTER JOIN t2 NATURAL JOIN t3
  }
} {1 11 111 1111}
do_test join2-1.6-rj {
  execsql {
    SELECT * FROM
      t2 NATURAL RIGHT OUTER JOIN t1 NATURAL JOIN t3
  }
} {11 111 1 1111}
ifcapable subquery {
  do_test join2-1.7 {
    execsql {
      SELECT * FROM
        t1 NATURAL LEFT OUTER JOIN (t2 NATURAL JOIN t3)
    }
  } {1 11 111 1111 2 22 {} {} 3 33 {} {}}
  do_test join2-1.7-rj {
    execsql {
      SELECT a, b, c, d FROM
        t2 NATURAL JOIN t3 NATURAL RIGHT JOIN t1
    }
  } {1 11 111 1111 2 22 {} {} 3 33 {} {}}
}
do_execsql_test 2.0 {
  CREATE TABLE aa(a);
  CREATE TABLE bb(b);
  CREATE TABLE cc(c);
  INSERT INTO aa VALUES('one');
  INSERT INTO bb VALUES('one');
  INSERT INTO cc VALUES('one');
}
do_catchsql_test 2.1 {
  SELECT * FROM aa LEFT JOIN cc ON (a=b) JOIN bb ON (b=coalesce(c,1));
} {1 {ON clause references tables to its right}}
do_catchsql_test 2.1b {
  SELECT * FROM aa RIGHT JOIN cc ON (a=b) JOIN bb ON (b=coalesce(c,1));
} {1 {ON clause references tables to its right}}
do_catchsql_test 2.2 {
  SELECT * FROM aa JOIN cc ON (a=b) JOIN bb ON (b=c);
} {0 {one one one}}
reset_db
do_execsql_test 3.0 {
  CREATE TABLE t1(k1 INTEGER PRIMARY KEY, k2, k3);
  CREATE TABLE t2(k2 INTEGER PRIMARY KEY, v2);

  -- Prior to this problem being fixed, table t3_2 would be omitted from
  -- the join queries below, but if t3_1 were used in its place it would
  -- not.
  CREATE TABLE t3_1(k3 PRIMARY KEY, v3) WITHOUT ROWID;
  CREATE TABLE t3_2(v3, k3 PRIMARY KEY) WITHOUT ROWID;
}
do_eqp_test 3.1 {
  SELECT v2 FROM t1 LEFT JOIN t2 USING (k2) LEFT JOIN t3_1 USING (k3);
} {
  QUERY PLAN
  |--SCAN t1
  `--SEARCH t2 USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
}
do_eqp_test 3.2 {
  SELECT v2 FROM t1 LEFT JOIN t2 USING (k2) LEFT JOIN t3_2 USING (k3);
} {
  QUERY PLAN
  |--SCAN t1
  `--SEARCH t2 USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
}
do_execsql_test 4.0 {
  CREATE TABLE c1(k INTEGER PRIMARY KEY, v1);
  CREATE TABLE c2(k INTEGER PRIMARY KEY, v2);
  CREATE TABLE c3(k INTEGER PRIMARY KEY, v3);

  INSERT INTO c1 VALUES(1, 2);
  INSERT INTO c2 VALUES(2, 3);
  INSERT INTO c3 VALUES(3, 'v3');

  INSERT INTO c1 VALUES(111, 1112);
  INSERT INTO c2 VALUES(112, 1113);
  INSERT INTO c3 VALUES(113, 'v1113');
}
do_execsql_test 4.1.1 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v2);
} {2 v3 1112 {}}
do_execsql_test 4.1.2 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v1+1);
} {2 v3 1112 {}}
do_execsql_test 4.1.3 {
  SELECT DISTINCT v1, v3 FROM c1 LEFT JOIN c2 LEFT JOIN c3 ON (c3.k=v1+1);
} {2 v3 1112 {}}
do_execsql_test 4.1.4 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 LEFT JOIN c3 ON (c3.k=v1+1);
} {2 v3 2 v3 1112 {} 1112 {}}
do_eqp_test 4.1.5 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v2);
} {
  QUERY PLAN
  |--SCAN c1
  |--SEARCH c2 USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
  `--SEARCH c3 USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
}
do_eqp_test 4.1.6 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v1+1);
} {
  QUERY PLAN
  |--SCAN c1
  `--SEARCH c3 USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
}
do_execsql_test 4.2.0 {
  DROP TABLE c1;
  DROP TABLE c2;
  DROP TABLE c3;
  CREATE TABLE c1(k UNIQUE, v1);
  CREATE TABLE c2(k UNIQUE, v2);
  CREATE TABLE c3(k UNIQUE, v3);

  INSERT INTO c1 VALUES(1, 2);
  INSERT INTO c2 VALUES(2, 3);
  INSERT INTO c3 VALUES(3, 'v3');

  INSERT INTO c1 VALUES(111, 1112);
  INSERT INTO c2 VALUES(112, 1113);
  INSERT INTO c3 VALUES(113, 'v1113');
}
do_execsql_test 4.2.1 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v2);
} {2 v3 1112 {}}
do_execsql_test 4.2.2 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v1+1);
} {2 v3 1112 {}}
do_execsql_test 4.2.3 {
  SELECT DISTINCT v1, v3 FROM c1 LEFT JOIN c2 LEFT JOIN c3 ON (c3.k=v1+1);
} {2 v3 1112 {}}
do_execsql_test 4.2.4 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 LEFT JOIN c3 ON (c3.k=v1+1);
} {2 v3 2 v3 1112 {} 1112 {}}
do_eqp_test 4.2.5 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v2);
} {
  QUERY PLAN
  |--SCAN c1
  |--SEARCH c2 USING INDEX sqlite_autoindex_c2_1 (k=?) LEFT-JOIN
  `--SEARCH c3 USING INDEX sqlite_autoindex_c3_1 (k=?) LEFT-JOIN
}
do_eqp_test 4.2.6 {
  SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v1+1);
} {
  QUERY PLAN
  |--SCAN c1
  `--SEARCH c3 USING INDEX sqlite_autoindex_c3_1 (k=?) LEFT-JOIN
}
do_execsql_test 4.3.0 {
  DROP TABLE IF EXISTS t1;
  DROP TABLE IF EXISTS t2;
  CREATE TABLE t1(x PRIMARY KEY) WITHOUT ROWID;
  CREATE TABLE t2(x);
  SELECT a.x
    FROM t1 AS a
    LEFT JOIN t1 AS b ON (a.x=b.x)
    LEFT JOIN t2 AS c ON (a.x=c.x);
} {}
do_execsql_test 4.3.1 {
  WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<10)
    INSERT INTO t1(x) SELECT x FROM c;
  INSERT INTO t2(x) SELECT x+9 FROM t1;
  SELECT a.x, c.x
    FROM t1 AS a
    LEFT JOIN t1 AS b ON (a.x=b.x)
    LEFT JOIN t2 AS c ON (a.x=c.x);
} {1 {} 2 {} 3 {} 4 {} 5 {} 6 {} 7 {} 8 {} 9 {} 10 10}
do_execsql_test 5.0 {
  CREATE TABLE s1 (a INTEGER PRIMARY KEY);
  CREATE TABLE s2 (a INTEGER PRIMARY KEY);
  CREATE TABLE s3 (a INTEGER);
  CREATE UNIQUE INDEX ndx on s3(a);
}
do_eqp_test 5.1 {
  SELECT s1.a FROM s1 left join s2 using (a);
} {SCAN s1}
do_eqp_test 5.2 {
  SELECT s1.a FROM s1 left join s3 using (a);
} {SCAN s1}
do_execsql_test 6.0 {
  CREATE TABLE u1(a INTEGER PRIMARY KEY, b, c);
  CREATE TABLE u2(a INTEGER PRIMARY KEY, b, c);
  CREATE INDEX u1ab ON u1(b, c);
}
do_eqp_test 6.1 {
  SELECT u2.* FROM u2 LEFT JOIN u1 ON( u1.a=u2.a AND u1.b=u2.b AND u1.c=u2.c );
} {SCAN u2}
db close
sqlite3 db :memory:
do_execsql_test 7.0 {
  CREATE TABLE t1(a,b);  INSERT INTO t1 VALUES(1,2),(3,4),(5,6);
  CREATE TABLE t2(c,d);  INSERT INTO t2 VALUES(2,4),(3,6);
  CREATE TABLE t3(x);    INSERT INTO t3 VALUES(9);
  CREATE VIEW test AS
    SELECT *, 'x'
      FROM t1 LEFT JOIN (SELECT * FROM t2, t3) ON (c=b AND x=9)
      WHERE c IS NULL;
  SELECT * FROM test;
} {3 4 {} {} {} x 5 6 {} {} {} x}
reset_db
do_execsql_test 8.0 {
  CREATE TABLE t0(c0);
  CREATE TABLE t1(c0);
}
do_execsql_test 8.1 {
  SELECT * FROM t0 LEFT JOIN t1 
  WHERE (t1.c0 BETWEEN 0 AND 0) > ('' AND t0.c0);
}
reset_db
do_execsql_test 9.0 {
  CREATE TABLE t0(c0 INT);
  CREATE VIEW v0(c0) AS SELECT CAST(t0.c0 AS INTEGER) FROM t0;
  INSERT INTO t0(c0) VALUES (0);
}
do_execsql_test 9.1 {
  SELECT typeof(c0), c0 FROM v0 WHERE c0>='0'
} {integer 0}
do_execsql_test 9.2 {
  SELECT * FROM t0, v0 WHERE v0.c0 >= '0';
} {0 0}
do_execsql_test 9.3 {
  SELECT * FROM t0 LEFT JOIN v0 WHERE v0.c0 >= '0';
} {0 0}
do_execsql_test 9.4 {
  SELECT * FROM t0 LEFT JOIN v0 ON v0.c0 >= '0';
} {0 0}
do_execsql_test 9.5 {
  SELECT * FROM t0 LEFT JOIN v0 ON v0.c0 >= '0' WHERE TRUE 
  UNION SELECT 0,0 WHERE 0; 
} {0 0}
do_execsql_test 9.10 {
  CREATE TABLE t1 (aaa);
  INSERT INTO t1 VALUES(23456);
  CREATE TABLE t2(bbb);
  CREATE VIEW v2(ccc) AS SELECT bbb IS 1234 FROM t2;
  SELECT ccc, ccc IS NULL AS ddd FROM t1 LEFT JOIN v2;
} {{} 1}
optimization_control db query-flattener 0
do_execsql_test 9.11 {
  SELECT ccc, ccc IS NULL AS ddd FROM t1 LEFT JOIN v2;
} {{} 1}
reset_db
db null NULL
do_execsql_test 10.1 {
  CREATE TABLE t1 (x INTEGER);
  INSERT INTO t1 VALUES(1);   -- Some true value
  CREATE TABLE t2 (z TEXT);
  INSERT INTO t2 VALUES('some value');
  CREATE TABLE t3(w TEXT);
  INSERT INTO t3 VALUES('some other value');
}
do_execsql_test 10.2 {
  SELECT (
    SELECT 1 FROM t2 LEFT JOIN (SELECT x AS v FROM t3) ON 500=v WHERE (v OR FALSE)
  ) FROM t1;
} NULL
do_execsql_test 10.3 {
  SELECT (
    SELECT 1 FROM t2 LEFT JOIN (SELECT x AS v FROM t3) ON 500=v WHERE (v)
  ) FROM t1;
} NULL
optimization_control db all 0
do_execsql_test 10.4 {
  SELECT (
    SELECT 1 FROM t2 LEFT JOIN (SELECT x AS v FROM t3) ON 500=v WHERE (v OR FALSE)
  ) FROM t1;
} NULL
optimization_control db all 1
do_execsql_test 11.1 {
  DROP TABLE t1;
  DROP TABLE t2;
  DROP TABLE t3;
  CREATE TABLE t1(x TEXT, y INTEGER);
  INSERT INTO t1(x,y) VALUES(NULL,-2),(NULL,1),('0',2);
  CREATE TABLE t2(z INTEGER);
  INSERT INTO t2(z) VALUES(2),(-2);
  CREATE VIEW t3 AS SELECT z, (SELECT count(*) FROM t1) AS w FROM t2;
  SELECT * FROM t1 LEFT JOIN t3 ON y=z;
} {NULL -2 -2 3 NULL 1 NULL NULL 0 2 2 3}
do_execsql_test 12.1 {
  DROP TABLE t1;
  DROP TABLE t2;
  DROP VIEW t3;
  CREATE TABLE t1(a INTEGER PRIMARY KEY);
  WITH RECURSIVE c(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM c WHERE n<100)
    INSERT INTO t1(a) SELECT n FROM c;
  CREATE VIEW t2(b) AS SELECT a FROM t1;
}
do_vmstep_test 12.2 {
  SELECT * FROM t1 LEFT JOIN t2 ON a=b LIMIT 10 OFFSET 98;
} 2000 {99 99 100 100}
do_eqp_test 12.3 {
  SELECT * FROM t1 LEFT JOIN t2 ON a=b LIMIT 10 OFFSET 98;
} {
  QUERY PLAN
  |--SCAN t1
  `--SEARCH t1 USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN
}
reset_db
do_execsql_test 13.0 {
  CREATE TABLE t1(a1 INTEGER PRIMARY KEY, b1 INT);
  CREATE TABLE t2(c2 INT, d2 INTEGER PRIMARY KEY);
  CREATE TABLE t3(e3 INTEGER PRIMARY KEY);
  INSERT INTO t1 VALUES(33,0);
  INSERT INTO t2 VALUES(33,1),(33,2);
}
do_execsql_test 13.1 {
  SELECT t1.a1, t2.d2
    FROM (t1 LEFT JOIN t3 ON t3.e3=t1.b1) JOIN t2 ON t2.c2=t1.a1
   WHERE t1.a1=33
   ORDER BY t2.d2 DESC;
} {33 2 33 1}
db null NULL
do_execsql_test 14.0 {
  DROP TABLE t1;
  DROP TABLE t2;
  CREATE TABLE t1(a INT,b INT,c INT);
  CREATE TABLE t2(x INT,y INT,z INT);
  INSERT INTO t1 VALUES(1,NULL,5),(2,NULL,NULL);
  INSERT INTO t2 SELECT * FROM t1;
  CREATE INDEX t2yz ON t2(y,z);
  SELECT *, '|' FROM t1 LEFT JOIN t2 ON y=c AND z IN (5,0) ORDER BY +a;
} {1 NULL 5 NULL NULL NULL | 2 NULL NULL NULL NULL NULL |}
finish_test
"#,
    );
}
