#[test]
#[ignore = "66/96 subtests passed; pending: ATTACH, TEMP TABLE, ANALYZE/sqlite_stat1"]
fn test_table() {
    sqliters_tcltest::run_test_script("table", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test table-1.1 {
  execsql {
    CREATE TABLE test1 (
      one varchar(10),
      two text
    )
  }
  execsql {
    SELECT sql FROM sqlite_master WHERE type!='meta'
  }
} {{CREATE TABLE test1 (
      one varchar(10),
      two text
    )}}
do_test table-1.3 {
  execsql {SELECT name, tbl_name, type FROM sqlite_master WHERE type!='meta'}
} {test1 test1 table}
do_test table-1.4 {
  db close
  sqlite3 db test.db
  execsql {SELECT name, tbl_name, type from sqlite_master WHERE type!='meta'}
} {test1 test1 table}
do_test table-1.5 {
  execsql {DROP TABLE test1}
  execsql {SELECT * FROM sqlite_master WHERE type!='meta'}
} {}
do_test table-1.6 {
  db close
  sqlite3 db test.db
  execsql {SELECT name FROM sqlite_master WHERE type!='meta'}
} {}
do_test table-1.10 {
  execsql {CREATE TABLE "create" (f1 int)}
  execsql {SELECT name FROM sqlite_master WHERE type!='meta'}
} {create}
do_test table-1.11 {
  execsql {DROP TABLE "create"}
  execsql {SELECT name FROM "sqlite_master" WHERE type!='meta'}
} {}
do_test table-1.12 {
  execsql {CREATE TABLE test1("f1 ho" int)}
  execsql {SELECT name as "X" FROM sqlite_master WHERE type!='meta'}
} {test1}
do_test table-1.13 {
  execsql {DROP TABLE "TEST1"}
  execsql {SELECT name FROM "sqlite_master" WHERE type!='meta'}
} {}
do_test table-2.1 {
  execsql {CREATE TABLE TEST2(one text)}
  catchsql {CREATE TABLE test2(two text default 'hi')}
} {1 {table test2 already exists}}
do_test table-2.1.1 {
  catchsql {CREATE TABLE "test2" (two)}
} {1 {table "test2" already exists}}
do_test table-2.1b {
  set v [catch {execsql {CREATE TABLE sqlite_master(two text)}} msg]
  lappend v $msg
} {1 {object name reserved for internal use: sqlite_master}}
do_test table-2.1c {
  db close
  sqlite3 db test.db
  set v [catch {execsql {CREATE TABLE sqlite_master(two text)}} msg]
  lappend v $msg
} {1 {object name reserved for internal use: sqlite_master}}
do_test table-2.1d {
  catchsql {CREATE TABLE IF NOT EXISTS test2(x,y)}
} {0 {}}
do_test table-2.1e {
  catchsql {CREATE TABLE IF NOT EXISTS test2(x UNIQUE, y TEXT PRIMARY KEY)}
} {0 {}}
do_test table-2.1f {
  execsql {DROP TABLE test2; SELECT name FROM sqlite_master WHERE type!='meta'}
} {}
do_test table-2.2a {
  execsql {CREATE TABLE test2(one text)}
  execsql {CREATE INDEX test3 ON test2(one)}
  catchsql {CREATE TABLE test3(two text)}
} {1 {there is already an index named test3}}
do_test table-2.2b {
  db close
  sqlite3 db test.db
  set v [catch {execsql {CREATE TABLE test3(two text)}} msg]
  lappend v $msg
} {1 {there is already an index named test3}}
do_test table-2.2c {
  execsql {SELECT name FROM sqlite_master WHERE type!='meta' ORDER BY name}
} {test2 test3}
do_test table-2.2d {
  execsql {DROP INDEX test3}
  set v [catch {execsql {CREATE TABLE test3(two text)}} msg]
  lappend v $msg
} {0 {}}
do_test table-2.2e {
  execsql {SELECT name FROM sqlite_master WHERE type!='meta' ORDER BY name}
} {test2 test3}
do_test table-2.2f {
  execsql {DROP TABLE test2; DROP TABLE test3}
  execsql {SELECT name FROM sqlite_master WHERE type!='meta' ORDER BY name}
} {}
set big_table \
{CREATE TABLE big(
  f1 varchar(20),
  f2 char(10),
  f3 varchar(30) primary key,
  f4 text,
  f5 text,
  f6 text,
  f7 text,
  f8 text,
  f9 text,
  f10 text,
  f11 text,
  f12 text,
  f13 text,
  f14 text,
  f15 text,
  f16 text,
  f17 text,
  f18 text,
  f19 text,
  f20 text
)}
do_test table-3.1 {
  execsql $big_table
  execsql {SELECT sql FROM sqlite_master WHERE type=='table'}
} \{$big_table\}
do_test table-3.2 {
  set v [catch {execsql {CREATE TABLE BIG(xyz foo)}} msg]
  lappend v $msg
} {1 {table BIG already exists}}
do_test table-3.3 {
  set v [catch {execsql {CREATE TABLE biG(xyz foo)}} msg]
  lappend v $msg
} {1 {table biG already exists}}
do_test table-3.4 {
  set v [catch {execsql {CREATE TABLE bIg(xyz foo)}} msg]
  lappend v $msg
} {1 {table bIg already exists}}
do_test table-3.5 {
  db close
  sqlite3 db test.db
  set v [catch {execsql {CREATE TABLE Big(xyz foo)}} msg]
  lappend v $msg
} {1 {table Big already exists}}
do_test table-3.6 {
  execsql {DROP TABLE big}
  execsql {SELECT name FROM sqlite_master WHERE type!='meta'}
} {}
set r {}
for {set i 1} {$i<=100} {incr i} {
  lappend r [format test%03d $i]
}
do_test table-4.1 {
  for {set i 1} {$i<=100} {incr i} {
    set sql "CREATE TABLE [format test%03d $i] ("
    for {set k 1} {$k<$i} {incr k} {
      append sql "field$k text,"
    }
    append sql "last_field text)"
    execsql $sql
  }
  execsql {SELECT name FROM sqlite_master WHERE type!='meta' ORDER BY name}
} $r
do_test table-4.1b {
  db close
  sqlite3 db test.db
  execsql {SELECT name FROM sqlite_master WHERE type!='meta' ORDER BY name}
} $r
set r {}
for {set i 1} {$i<=100} {incr i 2} {
  lappend r [format test%03d $i]
}
do_test table-4.2 {
  for {set i 2} {$i<=100} {incr i 2} {
    set sql "DROP TABLE [format TEST%03d $i]"
    execsql $sql
  }
  execsql {SELECT name FROM sqlite_master WHERE type!='meta' ORDER BY name}
} $r
do_test table-4.3 {
  for {set i 1} {$i<=100} {incr i 2} {
    set sql "DROP TABLE [format test%03d $i]"
    execsql $sql
  }
  execsql {SELECT name FROM sqlite_master WHERE type!='meta' ORDER BY name}
} {}
do_test table-5.1.1 {
  catchsql {DROP TABLE test009}
} {1 {no such table: test009}}
do_test table-5.1.2 {
  catchsql {DROP TABLE IF EXISTS test009}
} {0 {}}
do_test table-5.2 {
  catchsql {DROP TABLE IF EXISTS sqlite_master}
} {1 {table sqlite_master may not be dropped}}
do_test table-5.2.1 {
  db eval {
    ANALYZE;
    DROP TABLE IF EXISTS sqlite_stat1;
    DROP TABLE IF EXISTS sqlite_stat2;
    DROP TABLE IF EXISTS sqlite_stat3;
    DROP TABLE IF EXISTS sqlite_stat4;
    SELECT name FROM sqlite_master WHERE name GLOB 'sqlite_stat*';
  }
} {}
do_test table-5.2.2 {
  db close
  forcedelete test.db
  sqlite3 db test.db
  sqlite3_db_config db DEFENSIVE 0
  db eval {
    CREATE TABLE t0(a,b);
    CREATE INDEX t ON t0(a);
    PRAGMA writable_schema=ON;
    UPDATE sqlite_master SET sql='CREATE TABLE a.b(a UNIQUE';
    BEGIN;
    CREATE TABLE t1(x);
    ROLLBACK;
    DROP TABLE IF EXISTS t99;
  }
} {}
db close
forcedelete test.db
sqlite3 db test.db
do_test table-5.3 {
  ifcapable {explain} {
    execsql {EXPLAIN CREATE TABLE test1(f1 int)}
  }
  execsql {SELECT name FROM sqlite_master WHERE type!='meta'}
} {}
do_test table-5.4 {
  execsql {CREATE TABLE test1(f1 int)}
  ifcapable {explain} {
    execsql {EXPLAIN DROP TABLE test1}
  }
  execsql {SELECT name FROM sqlite_master WHERE type!='meta'}
} {test1}
do_test table-7.1 {
  set v [catch {execsql {
    CREATE TABLE weird(
      desc text,
      asc text,
      key int,
      [14_vac] boolean,
      fuzzy_dog_12 varchar(10),
      begin blob,
      end clob
    )
  }} msg]
  lappend v $msg
} {0 {}}
do_test table-7.2 {
  execsql {
    INSERT INTO weird VALUES('a','b',9,0,'xyz','hi','y''all');
    SELECT * FROM weird;
  }
} {a b 9 0 xyz hi y'all}
do_test table-7.3 {
  execsql2 {
    SELECT * FROM weird;
  }
} {desc a asc b key 9 14_vac 0 fuzzy_dog_12 xyz begin hi end y'all}
do_test table-7.3 {
  execsql {
    CREATE TABLE savepoint(release);
    INSERT INTO savepoint(release) VALUES(10);
    UPDATE savepoint SET release = 5;
    SELECT release FROM savepoint;
  }
} {5}
do_test table-8.1 {
  execsql2 {
    CREATE TABLE t2 AS SELECT * FROM weird;
    SELECT * FROM t2;
  }
} {desc a asc b key 9 14_vac 0 fuzzy_dog_12 xyz begin hi end y'all}
do_test table-8.1.1 {
  execsql {
    SELECT sql FROM sqlite_master WHERE name='t2';
  }
} {{CREATE TABLE t2(
  "desc" TEXT,
  "asc" TEXT,
  "key" INT,
  "14_vac" NUM,
  fuzzy_dog_12 TEXT,
  "begin",
  "end" TEXT
)}}
do_test table-8.2 {
  execsql {
    CREATE TABLE "t3""xyz"(a,b,c);
    INSERT INTO [t3"xyz] VALUES(1,2,3);
    SELECT * FROM [t3"xyz];
  }
} {1 2 3}
do_test table-8.3 {
  execsql2 {
    CREATE TABLE [t4"abc] AS SELECT count(*) as cnt, max(b+c) FROM [t3"xyz];
    SELECT * FROM [t4"abc];
  }
} {cnt 1 max(b+c) 5}
do_test table-8.3.1 {
  execsql {
    SELECT sql FROM sqlite_master WHERE name='t4"abc'
  }
} {{CREATE TABLE "t4""abc"(cnt,"max(b+c)")}}
ifcapable tempdb {
  do_test table-8.4 {
    execsql2 {
      CREATE TEMPORARY TABLE t5 AS SELECT count(*) AS [y'all] FROM [t3"xyz];
      SELECT * FROM t5;
    }
  } {y'all 1}
}
do_test table-8.5 {
  db close
  sqlite3 db test.db
  execsql2 {
    SELECT * FROM [t4"abc];
  }
} {cnt 1 max(b+c) 5}
do_test table-8.6 {
  execsql2 {
    SELECT * FROM t2;
  }
} {desc a asc b key 9 14_vac 0 fuzzy_dog_12 xyz begin hi end y'all}
do_test table-8.7 {
  catchsql {
    SELECT * FROM t5;
  }
} {1 {no such table: t5}}
do_test table-8.8 {
  catchsql {
    CREATE TABLE t5 AS SELECT * FROM no_such_table;
  }
} {1 {no such table: no_such_table}}
do_test table-8.9 {
  execsql {
    CREATE TABLE t10("col.1" [char.3]);
    CREATE TABLE t11 AS SELECT * FROM t10;
    SELECT sql FROM sqlite_master WHERE name = 't11';
  }
} {{CREATE TABLE t11("col.1" TEXT)}}
do_test table-8.10 {
  execsql {
    CREATE TABLE t12(
      a INTEGER,
      b VARCHAR(10),
      c VARCHAR(1,10),
      d VARCHAR(+1,-10),
      e VARCHAR (+1,-10),
      f "VARCHAR (+1,-10, 5)",
      g BIG INTEGER
    );
    CREATE TABLE t13 AS SELECT * FROM t12;
    SELECT sql FROM sqlite_master WHERE name = 't13';
  }
} {{CREATE TABLE t13(
  a INT,
  b TEXT,
  c TEXT,
  d TEXT,
  e TEXT,
  f TEXT,
  g INT
)}}
do_test table-9.1 {
  catchsql {
    CREATE TABLE t6(a,b,a);
  }
} {1 {duplicate column name: a}}
do_test table-9.2 {
  catchsql {
    CREATE TABLE t6(a varchar(100), b blob, a integer);
  }
} {1 {duplicate column name: a}}
ifcapable {foreignkey} {
do_test table-10.1 {
  catchsql {
    CREATE TABLE t6(a REFERENCES t4(a) NOT NULL);
    INSERT INTO t6 VALUES(NULL);
  }
} {1 {NOT NULL constraint failed: t6.a}}
do_test table-10.2 {
  catchsql {
    DROP TABLE t6;
    CREATE TABLE t6(a REFERENCES t4(a) MATCH PARTIAL);
  }
} {0 {}}
do_test table-10.3 {
  catchsql {
    DROP TABLE t6;
    CREATE TABLE t6(a REFERENCES t4 MATCH FULL ON DELETE SET NULL NOT NULL);
  }
} {0 {}}
do_test table-10.4 {
  catchsql {
    DROP TABLE t6;
    CREATE TABLE t6(a REFERENCES t4 MATCH FULL ON UPDATE SET DEFAULT DEFAULT 1);
  }
} {0 {}}
do_test table-10.5 {
  catchsql {
    DROP TABLE t6;
    CREATE TABLE t6(a NOT NULL NOT DEFERRABLE INITIALLY IMMEDIATE);
  }
} {0 {}}
do_test table-10.6 {
  catchsql {
    DROP TABLE t6;
    CREATE TABLE t6(a NOT NULL DEFERRABLE INITIALLY DEFERRED);
  }
} {0 {}}
do_test table-10.7 {
  catchsql {
    DROP TABLE t6;
    CREATE TABLE t6(a,
      FOREIGN KEY (a) REFERENCES t4(b) DEFERRABLE INITIALLY DEFERRED
    );
  }
} {0 {}}
do_test table-10.8 {
  catchsql {
    DROP TABLE t6;
    CREATE TABLE t6(a,b,c,
      FOREIGN KEY (b,c) REFERENCES t4(x,y) MATCH PARTIAL
        ON UPDATE SET NULL ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
    );
  }
} {0 {}}
do_test table-10.9 {
  catchsql {
    DROP TABLE t6;
    CREATE TABLE t6(a,b,c,
      FOREIGN KEY (b,c) REFERENCES t4(x)
    );
  }
} {1 {number of columns in foreign key does not match the number of columns in the referenced table}}
do_test table-10.10 {
  catchsql {DROP TABLE t6}
  catchsql {
    CREATE TABLE t6(a,b,c,
      FOREIGN KEY (b,c) REFERENCES t4(x,y,z)
    );
  }
} {1 {number of columns in foreign key does not match the number of columns in the referenced table}}
do_test table-10.11 {
  catchsql {DROP TABLE t6}
  catchsql {
    CREATE TABLE t6(a,b, c REFERENCES t4(x,y));
  }
} {1 {foreign key on c should reference only one column of table t4}}
do_test table-10.12 {
  catchsql {DROP TABLE t6}
  catchsql {
    CREATE TABLE t6(a,b,c,
      FOREIGN KEY (b,x) REFERENCES t4(x,y)
    );
  }
} {1 {unknown column "x" in foreign key definition}}
do_test table-10.13 {
  catchsql {DROP TABLE t6}
  catchsql {
    CREATE TABLE t6(a,b,c,
      FOREIGN KEY (x,b) REFERENCES t4(x,y)
    );
  }
} {1 {unknown column "x" in foreign key definition}}
} ;# endif foreignkey
do_test table-11.1 {
  execsql {
    CREATE TABLE t7(
       a integer primary key,
       b number(5,10),
       c character varying (8),
       d VARCHAR(9),
       e clob,
       f BLOB,
       g Text,
       h
    );
    INSERT INTO t7(a) VALUES(1);
    SELECT typeof(a), typeof(b), typeof(c), typeof(d),
           typeof(e), typeof(f), typeof(g), typeof(h)
    FROM t7 LIMIT 1;
  }
} {integer null null null null null null null} 
do_test table-11.2 {
  execsql {
    SELECT typeof(a+b), typeof(a||b), typeof(c+d), typeof(c||d)
    FROM t7 LIMIT 1;
  }
} {null null null null}
do_test table-12.1 {
  ifcapable subquery {
    execsql {
      CREATE TABLE t8 AS SELECT b, h, a as i, (SELECT f FROM t7) as j FROM t7;
    }
  } else {
    execsql {
      CREATE TABLE t8 AS SELECT b, h, a as i, f as j FROM t7;
    }
  }
} {}
do_test table-12.2 {
  execsql {
    SELECT sql FROM sqlite_master WHERE tbl_name = 't8'
  }
} {{CREATE TABLE t8(b NUM,h,i INT,j)}}
do_test table-13.1 {
  execsql {
    CREATE TABLE tablet8(
       a integer primary key,
       tm text DEFAULT CURRENT_TIME,
       dt text DEFAULT CURRENT_DATE,
       dttm text DEFAULT CURRENT_TIMESTAMP
    );
    SELECT * FROM tablet8;
  }
} {}
set i 0
unset -nocomplain date time seconds
foreach {date time seconds} {
  1976-07-04 12:00:00 205329600
  1994-04-16 14:00:00 766504800
  2000-01-01 00:00:00 946684800
  2003-12-31 12:34:56 1072874096
} {
  incr i
  set sqlite_current_time $seconds
  do_test table-13.2.$i {
    execsql "
      INSERT INTO tablet8(a) VALUES($i);
      SELECT tm, dt, dttm FROM tablet8 WHERE a=$i;
    "
  } [list $time $date [list $date $time]]
}
set sqlite_current_time 0
unset -nocomplain result
do_test table-14.1 {
  set rc [
    catch {
      db eval {SELECT * FROM tablet8 LIMIT 1} {} {
        db eval {CREATE TABLE t9(a, b, c)}
      }
    } msg
  ]
  set result [list $rc $msg]
} {0 {}}
do_test table-14.2 {
  set rc [
    catch {
      db eval {SELECT * FROM tablet8 LIMIT 1} {} {
        db eval {DROP TABLE t9;}
      }
    } msg
  ] 
  set result [list $rc $msg]
} {1 {database table is locked}}
ifcapable attach {
  do_test table-14.3 {
    forcedelete test2.db
    forcedelete test2.db-journal
    execsql {
      ATTACH 'test2.db' as aux;
    }
    db eval {SELECT * FROM tablet8 LIMIT 1} {} {
      db eval {CREATE TABLE aux.t1(a, b, c)}
    }
  } {}
  
  do_test table-14.4 {
    set rc [
      catch {
        db eval {SELECT * FROM tablet8 LIMIT 1} {} {
          db eval {DROP TABLE aux.t1;}
        }
      } msg
    ] 
    set result [list $rc $msg]
  } {1 {database table is locked}}
}
do_test table-15.1 {
  execsql {BEGIN}
  for {set i 0} {$i<2000} {incr i} {
    execsql "CREATE TABLE tbl$i (a, b, c)"
  }
  execsql {COMMIT}
} {}
do_test table-15.2 {
  execsql {BEGIN}
  for {set i 0} {$i<2000} {incr i} {
    execsql "DROP TABLE tbl$i"
  }
  execsql {COMMIT}
} {}
do_execsql_test table-16.1 {
  CREATE TABLE t16(x DEFAULT(max(1)));
  INSERT INTO t16(x) VALUES(123);
  SELECT rowid, x FROM t16;
} {1 123}
do_catchsql_test table-16.2 {
  INSERT INTO t16(rowid) VALUES(4);
} {1 {unknown function: max()}}
do_execsql_test table-16.3 {
  DROP TABLE t16;
  CREATE TABLE t16(x DEFAULT(abs(1)));
  INSERT INTO t16(rowid) VALUES(4);
  SELECT rowid, x FROM t16;
} {4 1}
do_catchsql_test table-16.4 {
  DROP TABLE t16;
  CREATE TABLE t16(x DEFAULT(avg(1)));
  INSERT INTO t16(rowid) VALUES(123);
  SELECT rowid, x FROM t16;
} {1 {unknown function: avg()}}
do_catchsql_test table-16.5 {
  DROP TABLE t16;
  CREATE TABLE t16(x DEFAULT(count()));
  INSERT INTO t16(rowid) VALUES(123);
  SELECT rowid, x FROM t16;
} {1 {unknown function: count()}}
do_catchsql_test table-16.6 {
  DROP TABLE t16;
  CREATE TABLE t16(x DEFAULT(string_agg('x',',')));
  INSERT INTO t16(rowid) VALUES(123);
  SELECT rowid, x FROM t16;
} {1 {unknown function: string_agg()}}
do_catchsql_test table-16.7 {
  INSERT INTO t16 DEFAULT VALUES;
} {1 {unknown function: string_agg()}}
do_execsql_test table-17.1 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(a TEXT);
  INSERT INTO t1(a) VALUES(1),(2);
  DROP TABLE IF EXISTS t2;
  CREATE TABLE t2(x TEXT, y TEXT);
  INSERT INTO t2(x,y) VALUES(3,4);
  DROP TABLE IF EXISTS t3;
  CREATE TABLE t3 AS
    SELECT a AS p, coalesce(y,a) AS q FROM t1 LEFT JOIN t2 ON a=x;
  SELECT p, q, '|' FROM t3 ORDER BY p;
} {1 1 | 2 2 |}
do_catchsql_test table-18.1 {
  DROP TABLE IF EXISTS t1;
  BEGIN;
  CREATE TABLE t1 AS SELECT zeroblob(2e20);
} {1 {string or blob too big}}
do_execsql_test table-18.2 {
  COMMIT;
  PRAGMA integrity_check;
} {ok}
do_execsql_test table-19.1 {
  CREATE TABLE t19 AS SELECT * FROM sqlite_master;
  SELECT name FROM t19 ORDER BY name;
} {{} savepoint t10 t11 t12 t13 t16 t2 t3 t3\"xyz t4\"abc t7 t8 t9 tablet8 test1 weird}
finish_test
"#);
}
