#[test]
#[ignore = "0/0 subtests passed; pending: VIEW, TEMP TABLE"]
fn test_insert4() {
    sqliters_tcltest::run_test_script("insert4", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix insert4
ifcapable !view||!subquery {
  finish_test
  return
}
proc xferopt_test {testname N} {
  do_test $testname {set ::sqlite3_xferopt_count} $N
}
sqlite3_db_config db LEGACY_FILE_FORMAT 0
execsql {
  CREATE TABLE t1(a int, b int, check(b>a));
  CREATE TABLE t2(x int, y int);
  CREATE VIEW v2 AS SELECT y, x FROM t2;
  CREATE TABLE t3(a int, b int);
}
do_test insert4-1.1 {
  set sqlite3_xferopt_count 0
  execsql {
    DELETE FROM t1;
    DELETE FROM t2;
    INSERT INTO t2 VALUES(9,1);
  }
  catchsql {
    INSERT INTO t1 SELECT * FROM t2;
  }
} {1 {CHECK constraint failed: b>a}}
xferopt_test insert4-1.2 0
do_test insert4-1.3 {
  execsql {
    SELECT * FROM t1;
  }
} {}
do_test insert4-2.1.1 {
  execsql {
    DELETE FROM t1;
    INSERT INTO t1 SELECT 4, 8;
    SELECT * FROM t1;
  }
} {4 8}
xferopt_test insert4-2.1.2  0
do_test insert4-2.2.1 {
  catchsql {
    DELETE FROM t1;
    INSERT INTO t1 SELECT * FROM v2;
    SELECT * FROM t1;
  }
} {0 {1 9}}
xferopt_test insert4-2.2.2 0
do_test insert4-2.3.1 {
  execsql {
    DELETE FROM t2;
    INSERT INTO t2 VALUES(9,1);
    INSERT INTO t2 SELECT y, x FROM t2;
    INSERT INTO t3 SELECT * FROM t2 LIMIT 1;
    SELECT * FROM t3;
  }
} {9 1}
xferopt_test insert4-2.3.2  0
do_test insert4-2.3.3 {
  catchsql {
    DELETE FROM t1;
    INSERT INTO t1 SELECT * FROM t2 LIMIT 1;
    SELECT * FROM t1;
  }
} {1 {CHECK constraint failed: b>a}}
xferopt_test insert4-2.3.4 0
do_test insert4-2.4.1 {
  execsql {
    DELETE FROM t3;
    INSERT INTO t3 SELECT DISTINCT * FROM t2;
    SELECT * FROM t3;
  }
} {9 1 1 9}
xferopt_test insert4-2.4.2 0
do_test insert4-2.4.3 {
  catchsql {
    DELETE FROM t1;
    INSERT INTO t1 SELECT DISTINCT * FROM t2;
  }
} {1 {CHECK constraint failed: b>a}}
xferopt_test insert4-2.4.4 0
proc xfer_check {testid xferused initdata destschema srcschema} {
  execsql "CREATE TABLE dest($destschema)"
  execsql "CREATE TABLE src($srcschema)"
  execsql "INSERT INTO src VALUES([join $initdata ,])"
  set ::sqlite3_xferopt_count 0
  do_test $testid.1 {
    execsql {
      INSERT INTO dest SELECT * FROM src;
      SELECT * FROM dest;
    }
  } $initdata
  do_test $testid.2 {
    set ::sqlite3_xferopt_count
  } $xferused
  execsql {
    DROP TABLE dest;
    DROP TABLE src;
  }
}
xfer_check insert4-3.1 1 {1 9} \
    {a int, b int CHECK(b>a)} \
    {x int, y int CHECK(y>x)}
xfer_check insert4-3.2 1 {1 9} \
    {a int, b int CHECK(b>a)} \
    {x int CHECK(y>x), y int}
xfer_check insert4-3.3 1 {1 9} \
    {a int, b int} \
    {x int, y int CHECK(y>x)}
xfer_check insert4-3.4 0 {1 9} \
    {a int, b int CHECK(b>a)} \
    {x int, y int}
xfer_check insert4-3.5 0 {1 9} \
    {a int, b int NOT NULL} \
    {x int, y int}
xfer_check insert4-3.6 0 {1 9} \
    {a int, b int NOT NULL} \
    {x int NOT NULL, y int}
xfer_check insert4-3.7 0 {1 9} \
    {a int NOT NULL, b int NOT NULL} \
    {x int NOT NULL, y int}
xfer_check insert4-3.8 0 {1 9} \
    {a int NOT NULL, b int} \
    {x int, y int}
xfer_check insert4-3.9 1 {1 9} \
    {a int, b int} \
    {x int NOT NULL, y int}
xfer_check insert4-3.10 1 {1 9} \
    {a int, b int} \
    {x int NOT NULL, y int NOT NULL}
xfer_check insert4-3.11 1 {1 9} \
    {a int NOT NULL, b int} \
    {x int NOT NULL, y int NOT NULL}
xfer_check insert4-3.12 1 {1 9} \
    {a int, b int NOT NULL} \
    {x int NOT NULL, y int NOT NULL}
xfer_check insert4-3.20 0 {1 9} \
    {a text, b int} \
    {x int, b int}
xfer_check insert4-3.21 0 {1 9} \
    {a int, b int} \
    {x text, b int}
xfer_check insert4-3.22 1 {1 9} \
    {a int, b int} \
    {x integer, b int}
do_test insert4-4.1a {
  execsql {CREATE TABLE t4(a, b, UNIQUE(a,b))}
} {}
ifcapable vacuum {
  do_test insert4-4.1b {
    execsql {
      INSERT INTO t4 VALUES(NULL,0);
      INSERT INTO t4 VALUES(NULL,1);
      INSERT INTO t4 VALUES(NULL,1);
      VACUUM;   
    }
  } {}
}
do_test insert4-5.1 {
  catchsql { INSERT INTO t2 SELECT a, b FROM nosuchtable }
} {1 {no such table: nosuchtable}}
do_test insert4-5.2 {
  catchsql { 
    CREATE TABLE t5(a, b, c);
    INSERT INTO t4 SELECT * FROM t5;
  }
} {1 {table t4 has 2 columns but 3 values were supplied}}
do_test insert4-6.1 {
  set ::sqlite3_xferopt_count 0
  execsql {
    CREATE INDEX t2_i2 ON t2(x, y COLLATE nocase); 
    CREATE INDEX t2_i1 ON t2(x ASC, y DESC);
    CREATE INDEX t3_i1 ON t3(a, b);
    INSERT INTO t2 SELECT * FROM t3;
  }
  set ::sqlite3_xferopt_count
} {0}
do_test insert4-6.2 {
  set ::sqlite3_xferopt_count 0
  execsql {
    DROP INDEX t2_i2;
    INSERT INTO t2 SELECT * FROM t3;
  }
  set ::sqlite3_xferopt_count
} {0}
do_test insert4-6.3 {
  set ::sqlite3_xferopt_count 0
  execsql {
    DROP INDEX t2_i1;
    CREATE INDEX t2_i1 ON t2(x ASC, y ASC);
    INSERT INTO t2 SELECT * FROM t3;
  }
  set ::sqlite3_xferopt_count
} {1}
do_test insert4-6.4 {
  set ::sqlite3_xferopt_count 0
  execsql {
    DROP INDEX t2_i1;
    CREATE INDEX t2_i1 ON t2(x ASC, y COLLATE RTRIM);
    INSERT INTO t2 SELECT * FROM t3;
  }
  set ::sqlite3_xferopt_count
} {0}
do_test insert4-6.5 {
  execsql {
    CREATE TABLE t6a(x CHECK( x<>'abc' ));
    INSERT INTO t6a VALUES('ABC');
    SELECT * FROM t6a;
  }
} {ABC}
do_test insert4-6.6 {
  execsql {
    CREATE TABLE t6b(x CHECK( x<>'abc' COLLATE nocase ));
  }
  catchsql {
    INSERT INTO t6b SELECT * FROM t6a;
  }
} {1 {CHECK constraint failed: x<>'abc' COLLATE nocase}}
do_test insert4-6.7 {
  execsql {
    DROP TABLE t6b;
    CREATE TABLE t6b(x CHECK( x COLLATE nocase <>'abc' ));
  }
  catchsql {
    INSERT INTO t6b SELECT * FROM t6a;
  }
} {1 {CHECK constraint failed: x COLLATE nocase <>'abc'}}
ifcapable foreignkey {
  do_test insert4-7.1 {
    set ::sqlite3_xferopt_count 0
    execsql {
      CREATE TABLE t7a(x INTEGER PRIMARY KEY); INSERT INTO t7a VALUES(123);
      CREATE TABLE t7b(y INTEGER REFERENCES t7a);
      CREATE TABLE t7c(z INT);  INSERT INTO t7c VALUES(234);
      INSERT INTO t7b SELECT * FROM t7c;
      SELECT * FROM t7b;
    }
  } {234}
  do_test insert4-7.2 {
    set ::sqlite3_xferopt_count
  } {1}
  do_test insert4-7.3 {
    set ::sqlite3_xferopt_count 0
    execsql {
      DELETE FROM t7b;
      PRAGMA foreign_keys=ON;
    }
    catchsql {
      INSERT INTO t7b SELECT * FROM t7c;
    }
  } {1 {FOREIGN KEY constraint failed}}
  do_test insert4-7.4 {
    execsql {SELECT * FROM t7b}
  } {}
  do_test insert4-7.5 {
    set ::sqlite3_xferopt_count
  } {0}
  do_test insert4-7.6 {
    set ::sqlite3_xferopt_count 0
    execsql {
      DELETE FROM t7b; DELETE FROM t7c;
      INSERT INTO t7c VALUES(123);
      INSERT INTO t7b SELECT * FROM t7c;
      SELECT * FROM t7b;
    }
  } {123}
  do_test insert4-7.7 {
    set ::sqlite3_xferopt_count
  } {0}
  do_test insert4-7.7 {
    set ::sqlite3_xferopt_count 0
    execsql {
      PRAGMA foreign_keys=OFF;
      DELETE FROM t7b;
      INSERT INTO t7b SELECT * FROM t7c;
      SELECT * FROM t7b;
    }
  } {123}
  do_test insert4-7.8 {
    set ::sqlite3_xferopt_count
  } {1}
}
do_test insert4-8.1 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT REPLACE, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT REPLACE, y);
    INSERT INTO t1 VALUES(1,2);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 3}
do_test insert4-8.2 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT REPLACE, b);
    CREATE TABLE t2(x, y);
    INSERT INTO t1 VALUES(1,2);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 3}
do_test insert4-8.3 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT IGNORE, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT IGNORE, y);
    INSERT INTO t1 VALUES(1,2);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 2}
do_test insert4-8.4 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT IGNORE, b);
    CREATE TABLE t2(x, y);
    INSERT INTO t1 VALUES(1,2);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 2}
do_test insert4-8.5 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT FAIL, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT FAIL, y);
    INSERT INTO t1 VALUES(1,2);
    INSERT INTO t2 VALUES(-99,100);
    INSERT INTO t2 VALUES(1,3);
    SELECT * FROM t1;
  }
  catchsql {
    INSERT INTO t1 SELECT * FROM t2;
  }
} {1 {UNIQUE constraint failed: t1.a}}
do_test insert4-8.6 {
  execsql {
    SELECT * FROM t1;
  }
} {-99 100 1 2} 
do_test insert4-8.7 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT ABORT, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT ABORT, y);
    INSERT INTO t1 VALUES(1,2);
    INSERT INTO t2 VALUES(-99,100);
    INSERT INTO t2 VALUES(1,3);
    SELECT * FROM t1;
  }
  catchsql {
    INSERT INTO t1 SELECT * FROM t2;
  }
} {1 {UNIQUE constraint failed: t1.a}}
do_test insert4-8.8 {
  execsql {
    SELECT * FROM t1;
  }
} {1 2} 
do_test insert4-8.9 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT ROLLBACK, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT ROLLBACK, y);
    INSERT INTO t1 VALUES(1,2);
    INSERT INTO t2 VALUES(-99,100);
    INSERT INTO t2 VALUES(1,3);
    SELECT * FROM t1;
  }
  catchsql {
    BEGIN;
    INSERT INTO t1 VALUES(2,3);
    INSERT INTO t1 SELECT * FROM t2;
  }
} {1 {UNIQUE constraint failed: t1.a}}
do_test insert4-8.10 {
  catchsql {COMMIT}
} {1 {cannot commit - no transaction is active}}
do_test insert4-8.11 {
  execsql {
    SELECT * FROM t1;
  }
} {1 2} 
do_test insert4-8.21 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT REPLACE, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT REPLACE, y);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 3}
do_test insert4-8.22 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT IGNORE, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT IGNORE, y);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 3}
do_test insert4-8.23 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT ABORT, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT ABORT, y);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 3}
do_test insert4-8.24 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT FAIL, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT FAIL, y);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 3}
do_test insert4-8.25 {
  execsql {
    DROP TABLE IF EXISTS t1;
    DROP TABLE IF EXISTS t2;
    CREATE TABLE t1(a INTEGER PRIMARY KEY ON CONFLICT ROLLBACK, b);
    CREATE TABLE t2(x INTEGER PRIMARY KEY ON CONFLICT ROLLBACK, y);
    INSERT INTO t2 VALUES(1,3);
    INSERT INTO t1 SELECT * FROM t2;
    SELECT * FROM t1;
  }
} {1 3}
do_catchsql_test insert4-9.1 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(x);
  INSERT INTO t1(x) VALUES(5 COLLATE xyzzy) UNION SELECT 0;
} {1 {no such collation sequence: xyzzy}}
do_execsql_test 10.1 {
  CREATE TABLE t8(
    rid INTEGER,
    pid INTEGER,
    mid INTEGER,
    px INTEGER DEFAULT(0) CHECK(px IN(0, 1))
  );
  CREATE TEMP TABLE x(
    rid INTEGER,
    pid INTEGER,
    mid INTEGER,
    px INTEGER DEFAULT(0) CHECK(px IN(0, 1))
  );
}
do_test 10.2 {
  set sqlite3_xferopt_count 0
  execsql { INSERT INTO x SELECT * FROM t8 }
  set sqlite3_xferopt_count
} {1}
do_test 10.3 {
  execsql { PRAGMA integrity_check }
  set sqlite3_xferopt_count 0
  execsql { INSERT INTO x     SELECT * FROM t8 }
  set sqlite3_xferopt_count
} {1}
do_test 10.4 {
  execsql { PRAGMA integrity_check }
  set sqlite3_xferopt_count 0
  execsql { INSERT INTO x     SELECT * FROM t8  RETURNING * }
  set sqlite3_xferopt_count
} {0}
do_execsql_test 11.0 {
  CREATE TABLE t9(a, b, c);
  CREATE INDEX t9a ON t9(a);
  CREATE INDEX t9b ON t9(b) WHERE c=0;

  INSERT INTO t9 VALUES(1, 1, 1);
  INSERT INTO t9 VALUES(2, 2, 2);
  INSERT INTO t9 VALUES(3, 3, 3);

  CREATE TABLE t10(a, b, c);
  CREATE INDEX t10a ON t10(a);
  CREATE INDEX t10b ON t10(b) WHERE c=0;

  INSERT INTO t10 SELECT * FROM t9;
  SELECT * FROM t10;
  PRAGMA integrity_check;
} {1 1 1  2 2 2  3 3 3  ok}
do_execsql_test 12.0 {
  CREATE TABLE src(x,y,z, CONSTRAINT c1 CHECK(rowid<=5));
  CREATE TABLE dest(x,y,z, CONSTRAINT c2 CHECK(rowid<=5));
  INSERT INTO src(x) VALUES(22),(33),(44);
  UPDATE src SET y=x+100, z=x*100;
  INSERT INTO dest(x) VALUES(55),(66),(77);
  UPDATE dest SET y=x+100, z=x*100;
  PRAGMA integrity_check;
} ok
set sqlite3_xferopt_count 0
do_catchsql_test 12.1 {
  INSERT INTO dest SELECT * FROM src;
} {1 {CHECK constraint failed: c2}}
do_test 12.2 {
  set sqlite3_xferopt_count
} 0
do_execsql_test 12.3 {
  SELECT rowid, x FROM dest;
  PRAGMA integrity_check;
} {1 55 2 66 3 77 ok}
ifcapable altertable {
  do_execsql_test 12.4 {
    ALTER TABLE src DROP CONSTRAINT c1;
    ALTER TABLE dest DROP CONSTRAINT c2;
  }
  set sqlite3_xferopt_count 0
  do_catchsql_test 12.5 {
    INSERT INTO dest SELECT * FROM src;
  } {0 {}}
  do_test 12.6 {
    set sqlite3_xferopt_count
  } 1
  do_execsql_test 12.7 {
    SELECT rowid, x FROM dest;
    PRAGMA integrity_check;
  } {1 55 2 66 3 77 4 22 5 33 6 44 ok}
}
finish_test
"#);
}
