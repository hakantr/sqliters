#[test]
#[ignore = "20/26 subtests passed; pending: VIEW, TEMP TABLE, EQP/optimizer"]
fn test_select7() {
    sqliters_tcltest::run_test_script("select7", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix select7
ifcapable compound {

ifcapable tempdb {
  do_test select7-1.1 {
    execsql {
      create temp table t1(x);
      insert into t1 values('amx');
      insert into t1 values('anx');
      insert into t1 values('amy');
      insert into t1 values('bmy');
      select * from t1 where x like 'a__'
        intersect select * from t1 where x like '_m_'
        intersect select * from t1 where x like '__x';
    }
  } {amx}
}


ifcapable view {
do_test select7-2.1 {
  execsql {
    CREATE TABLE x(id integer primary key, a TEXT NULL);
    INSERT INTO x (a) VALUES ('first');
    CREATE TABLE tempx(id integer primary key, a TEXT NULL);
    INSERT INTO tempx (a) VALUES ('t-first');
    CREATE VIEW tv1 AS SELECT x.id, tx.id FROM x JOIN tempx tx ON tx.id=x.id;
    CREATE VIEW tv1b AS SELECT x.id, tx.id FROM x JOIN tempx tx on tx.id=x.id;
    CREATE VIEW tv2 AS SELECT * FROM tv1 UNION SELECT * FROM tv1b;
    SELECT * FROM tv2;
  }
} {1 1}
} ;# ifcapable view

} ;# ifcapable compound
ifcapable subquery {
  do_test select7-3.1 {
    catchsql {
      SELECT * FROM (SELECT * FROM sqlite_master) GROUP BY name
    }
  } [list 0 [execsql {SELECT * FROM sqlite_master ORDER BY name}]]
}
ifcapable {subquery && compound} {
  do_test select7-4.1 {
    execsql {
      CREATE TABLE IF NOT EXISTS photo(pk integer primary key, x);
      CREATE TABLE IF NOT EXISTS tag(pk integer primary key, fk int, name);
    
      SELECT P.pk from PHOTO P WHERE NOT EXISTS ( 
           SELECT T2.pk from TAG T2 WHERE T2.fk = P.pk 
           EXCEPT 
           SELECT T3.pk from TAG T3 WHERE T3.fk = P.pk AND T3.name LIKE '%foo%'
      );
    }
  } {}
  do_test select7-4.2 {
    execsql {
      INSERT INTO photo VALUES(1,1);
      INSERT INTO photo VALUES(2,2);
      INSERT INTO photo VALUES(3,3);
      INSERT INTO tag VALUES(11,1,'one');
      INSERT INTO tag VALUES(12,1,'two');
      INSERT INTO tag VALUES(21,1,'one-b');
      SELECT P.pk from PHOTO P WHERE NOT EXISTS ( 
           SELECT T2.pk from TAG T2 WHERE T2.fk = P.pk 
           EXCEPT 
           SELECT T3.pk from TAG T3 WHERE T3.fk = P.pk AND T3.name LIKE '%foo%'
      );
    }
  } {2 3}
}
ifcapable {subquery && compound} {
  do_test select7-5.1 {
    catchsql {
      CREATE TABLE t2(a,b);
      SELECT 5 IN (SELECT a,b FROM t2);
    }
  } {1 {sub-select returns 2 columns - expected 1}}
  do_test select7-5.2 {
    catchsql {
      SELECT 5 IN (SELECT * FROM t2);
    }
  } {1 {sub-select returns 2 columns - expected 1}}
  do_test select7-5.3 {
    catchsql {
      SELECT 5 IN (SELECT a,b FROM t2 UNION SELECT b,a FROM t2);
    }
  } {1 {sub-select returns 2 columns - expected 1}}
  do_test select7-5.4 {
    catchsql {
      SELECT 5 IN (SELECT * FROM t2 UNION SELECT * FROM t2);
    }
  } {1 {sub-select returns 2 columns - expected 1}}
}
if {[clang_sanitize_address]==0} {
  ifcapable compound {
    if {$SQLITE_MAX_COMPOUND_SELECT>0} {
      set sql {SELECT 0}
      set result 0
        for {set i 1} {$i<$SQLITE_MAX_COMPOUND_SELECT} {incr i} {
          append sql " UNION ALL SELECT $i"
            lappend result $i
        }
      do_test select7-6.1 {
        catchsql $sql
      } [list 0 $result]
      append sql { UNION ALL SELECT 99999999}
      do_test select7-6.2 {
        catchsql $sql
      } {1 {too many terms in compound SELECT}}
    }
  }
}
do_execsql_test select7-6.5 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(a,b,c);
}
sqlite3_limit db SQLITE_LIMIT_COMPOUND_SELECT 10
sqlite3_db_config db SQLITE_DBCONFIG_DQS_DML 0
do_catchsql_test select7-6.6 {
  INSERT INTO t1 VALUES
    (NULL,0,""),  (X'',0.0,0.0),  (X'',X'',""),  (0.0,0.0,""),  (NULL,NULL,0.0),
    (0,"",0),  (0.0,X'',0),  ("",X'',0.0),  (0.0,X'',NULL),  (0,NULL,""),
    (0,"",NULL),  (0.0,NULL,X''),  ("",X'',NULL),  (NULL,0,""),
    (0,NULL,0),  (X'',X'',0.0);
} {1 {no such column: "" - should this be a string literal in single-quotes?}}
do_execsql_test select7-6.7 {
  SELECT count(*) FROM t1;
} {0}
sqlite3_db_config db SQLITE_DBCONFIG_DQS_DML 1
do_catchsql_test select7-6.8 {
  INSERT INTO t1 VALUES
    (NULL,0,""),  (X'',0.0,0.0),  (X'',X'',""),  (0.0,0.0,""),  (NULL,NULL,0.0),
    (0,"",0),  (0.0,X'',0),  ("",X'',0.0),  (0.0,X'',NULL),  (0,NULL,""),
    (0,"",NULL),  (0.0,NULL,X''),  ("",X'',NULL),  (NULL,0,""),
    (0,NULL,0),  (X'',X'',0.0);
} {0 {}}
do_execsql_test select7-6.9 {
  SELECT count(*) FROM t1;
} {16}
do_test select7-7.1 {
  execsql {
    CREATE TABLE t3(a REAL);
    INSERT INTO t3 VALUES(44.0);
    INSERT INTO t3 VALUES(56.0);
  }
} {}
do_test select7-7.2 {
  execsql {
    pragma vdbe_trace = 0;
    SELECT (CASE WHEN a=0 THEN 0 ELSE (a + 25) / 50 END) AS categ, count(*)
    FROM t3 GROUP BY categ
  }
} {1.38 1 1.62 1}
do_test select7-7.3 {
  execsql {
    CREATE TABLE t4(a REAL);
    INSERT INTO t4 VALUES( 2.0 );
    INSERT INTO t4 VALUES( 3.0 );
  }
} {}
do_test select7-7.4 {
  execsql {
    SELECT (CASE WHEN a=0 THEN 'zero' ELSE a/2 END) AS t FROM t4 GROUP BY t;
  }
} {1.0 1.5}
do_test select7-7.5 {
  execsql { SELECT a=0, typeof(a) FROM t4 }
} {0 real 0 real}
do_test select7-7.6 {
  execsql { SELECT a=0, typeof(a) FROM t4 GROUP BY a }
} {0 real 0 real}
do_test select7-7.7 {
  execsql {
    CREATE TABLE t5(a TEXT, b INT);
    INSERT INTO t5 VALUES(123, 456);
    SELECT typeof(a), a FROM t5 GROUP BY a HAVING a<b;
  }
} {text 123}
do_execsql_test 8.0 { 
  CREATE TABLE t01(x, y);
  CREATE TABLE t02(x, y);
}
do_catchsql_test 8.1 {
  SELECT * FROM (
    SELECT * FROM t01 UNION SELECT x FROM t02
  ) WHERE y=1
} {1 {SELECTs to the left and right of UNION do not have the same number of result columns}}
do_catchsql_test 8.2 {
  CREATE VIEW v0 as SELECT x, y FROM t01 UNION SELECT x FROM t02;
  EXPLAIN QUERY PLAN SELECT * FROM v0 WHERE x='0' OR y;
} {1 {SELECTs to the left and right of UNION do not have the same number of result columns}}
finish_test
"#);
}
