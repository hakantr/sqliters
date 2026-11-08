#[test]
#[ignore = "0/2 subtests passed; pending: VIEW, TRIGGER, ATTACH"]
fn test_vacuum() {
    sqliters_tcltest::run_test_script("vacuum", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable {!vacuum} {
  omit_test vacuum.test {Compiled with SQLITE_OMIT_VACUUM}
  finish_test
  return
}
if $AUTOVACUUM {
  omit_test vacuum.test {Auto-vacuum is enabled}
  finish_test
  return
}
set fcnt 1
do_test vacuum-1.1 {
  execsql {
    BEGIN;
    CREATE TABLE t1(a INTEGER PRIMARY KEY, b, c);
    INSERT INTO t1 VALUES(NULL,randstr(10,100),randstr(5,50));
    INSERT INTO t1 VALUES(123456,randstr(10,100),randstr(5,50));
    INSERT INTO t1 SELECT NULL, b||'-'||rowid, c||'-'||rowid FROM t1;
    INSERT INTO t1 SELECT NULL, b||'-'||rowid, c||'-'||rowid FROM t1;
    INSERT INTO t1 SELECT NULL, b||'-'||rowid, c||'-'||rowid FROM t1;
    INSERT INTO t1 SELECT NULL, b||'-'||rowid, c||'-'||rowid FROM t1;
    INSERT INTO t1 SELECT NULL, b||'-'||rowid, c||'-'||rowid FROM t1;
    INSERT INTO t1 SELECT NULL, b||'-'||rowid, c||'-'||rowid FROM t1;
    INSERT INTO t1 SELECT NULL, b||'-'||rowid, c||'-'||rowid FROM t1;
    CREATE INDEX i1 ON t1(b,c);
    CREATE UNIQUE INDEX i2 ON t1(c,a);
    CREATE TABLE t2 AS SELECT * FROM t1;
    COMMIT;
    DROP TABLE t2;
  }
  set ::size1 [file size test.db]
  set ::cksum [cksum]
  expr {$::cksum!=""}
} {1}
proc failing_app_func {args} {error "bad function"}
do_test vacuum-1.1b {
  db func substr failing_app_func
  db func like failing_app_func
  db func quote failing_app_func
  catchsql {SELECT substr(name,1,3) FROM sqlite_master}
} {1 {bad function}}
do_test vacuum-1.2 {
  execsql {
    VACUUM;
  }
  cksum
} $cksum
ifcapable vacuum {
  do_test vacuum-1.3 {
    expr {[file size test.db]<$::size1}
  } {1}
}
do_test vacuum-1.4 {
  set sql_script {
    BEGIN;
    CREATE TABLE t2 AS SELECT * FROM t1;
    CREATE TABLE t3 AS SELECT * FROM t1;
    CREATE VIEW v1 AS SELECT b, c FROM t3;
    CREATE TRIGGER r1 AFTER DELETE ON t2 BEGIN SELECT 1; END;
    COMMIT;
    DROP TABLE t2;
  }
  ifcapable !view {
    regsub {CREATE VIEW} $sql_script {-- CREATE VIEW} sql_script
  }
  ifcapable !trigger {
    regsub {CREATE TRIGGER} $sql_script {-- CREATE TRIGGER} sql_script
  }
  execsql $sql_script
  set ::size1 [file size test.db]
  set ::cksum [cksum]
  expr {$::cksum!=""}
} {1}
do_test vacuum-1.5 {
  execsql {
    VACUUM;
  }
  cksum
} $cksum
ifcapable vacuum {
  do_test vacuum-1.6 {
    expr {[file size test.db]<$::size1}
  } {1}
}
ifcapable vacuum {
  do_test vacuum-2.1.1 {
    catchsql {
      BEGIN;
      VACUUM;
    }
  } {1 {cannot VACUUM from within a transaction}}
  do_test vacuum-2.1.2 {
    sqlite3_get_autocommit db
  } {0}
  do_test vacuum-2.1.3 {
    db eval {COMMIT}
  } {}
}
do_test vacuum-2.2 {
  sqlite3 db2 test.db
  execsql {
    BEGIN;
    CREATE TABLE t4 AS SELECT * FROM t1;
    CREATE TABLE t5 AS SELECT * FROM t1;
    COMMIT;
    DROP TABLE t4;
    DROP TABLE t5;
  } db2
  set ::cksum [cksum db2]
  catchsql {
    VACUUM
  }
} {0 {}}
do_test vacuum-2.3 {
  cksum
} $cksum
do_test vacuum-2.4 {
  catch {db2 eval {SELECT count(*) FROM sqlite_master}}
  cksum db2
} $cksum
do_test vacuum-2.5 {
  execsql {
    BEGIN;
    CREATE TABLE t6 AS SELECT * FROM t1;
    CREATE TABLE t7 AS SELECT * FROM t1;
    COMMIT;
  }
  sqlite3 db3 test.db
  execsql {
    -- The "SELECT * FROM sqlite_master" statement ensures that this test
    -- works when shared-cache is enabled. If shared-cache is enabled, then
    -- db3 shares a cache with db2 (but not db - it was opened as 
    -- "./test.db").
    SELECT * FROM sqlite_master;
    SELECT * FROM t7 LIMIT 1
  } db3
  execsql {
    VACUUM;
  }
  execsql {
    INSERT INTO t7 VALUES(1234567890,'hello','world');
  } db3
  execsql {
    SELECT * FROM t7 WHERE a=1234567890
  }
} {1234567890 hello world}
integrity_check vacuum-2.6
do_test vacuum-2.7 {
  execsql {
    SELECT * FROM t7 WHERE a=1234567890
  } db3
} {1234567890 hello world}
do_test vacuum-2.8 {
  execsql {
    INSERT INTO t7 SELECT * FROM t6;
    SELECT count(*) FROM t7;
  }
} 513
integrity_check vacuum-2.9
do_test vacuum-2.10 {
  execsql {
    DELETE FROM t7;
    SELECT count(*) FROM t7;
  } db3
} 0
integrity_check vacuum-2.11
db3 close
do_test vacuum-3.1 {
  db close
  db2 close
  delete_file test.db
  sqlite3 db test.db
  execsql {
    PRAGMA empty_result_callbacks=on;
    VACUUM;
  }
} {}
do_test vacuum-4.1 {
  db close
  sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
  set VM [sqlite3_prepare $DB {VACUUM} -1 TAIL]
  sqlite3_step $VM
} {SQLITE_DONE}
do_test vacuum-4.2 {
  sqlite3_finalize $VM
} SQLITE_OK
ifcapable view {
do_test vacuum-5.1 {
  db close
  forcedelete test.db
  sqlite3 db test.db
  catchsql {
    CREATE TABLE Test (TestID int primary key);
    INSERT INTO Test VALUES (NULL);
    CREATE VIEW viewTest AS SELECT * FROM Test;

    BEGIN;
    CREATE TABLE tempTest (TestID int primary key, Test2 int NULL);
    INSERT INTO tempTest SELECT TestID, 1 FROM Test;
    DROP TABLE Test;
    CREATE TABLE Test(TestID int primary key, Test2 int NULL);
    INSERT INTO Test SELECT * FROM tempTest;
    DROP TABLE tempTest;
    COMMIT;
    VACUUM;
  }
} {0 {}}
do_test vacuum-5.2 {
  catchsql {
    VACUUM;
  }
} {0 {}}
} ;# ifcapable view
do_test vacuum-6.1 {
  execsql {
    CREATE TABLE "abc abc"(a, b, c);
    INSERT INTO "abc abc" VALUES(1, 2, 3);
    VACUUM;
  }
} {}
do_test vacuum-6.2 {
  execsql {
    select * from "abc abc";
  }
} {1 2 3}
ifcapable {bloblit} {
  do_test vacuum-6.3 {
    execsql {
      DELETE FROM "abc abc";
      INSERT INTO "abc abc" VALUES(X'00112233', NULL, NULL);
      VACUUM;
    }
  } {}
  do_test vacuum-6.4 {
    execsql {
      select count(*) from "abc abc" WHERE a = X'00112233';
    }
  } {1}
}
forcedelete :memory:
do_test vacuum-7.0 {
  sqlite3 db2 :memory:
  execsql {
    CREATE TABLE t1(t);
    VACUUM;
  } db2
} {}
do_test vacuum-7.1 {
  execsql {
    CREATE TABLE t2(t);
    CREATE TABLE t3(t);
    DROP TABLE t2;
    PRAGMA freelist_count;
  }
} {1}
do_test vacuum-7.2 {
  execsql {
    VACUUM;
    pragma integrity_check;
  } db2
} {ok}
do_test vacuum-7.3 {
  execsql { PRAGMA freelist_count; } db2
} {0}
ifcapable autovacuum {
  do_test vacuum-7.4 {
    execsql { PRAGMA auto_vacuum } db2
  } {0}
  do_test vacuum-7.5 {
    execsql { PRAGMA auto_vacuum = 1} db2
    execsql { PRAGMA auto_vacuum } db2
  } {0}
  do_test vacuum-7.6 {
    execsql { PRAGMA auto_vacuum = 1} db2
    execsql { VACUUM } db2
    execsql { PRAGMA auto_vacuum } db2
  } {1}
}
db2 close
do_test vacuum-8.1 {
  forcedelete a'z.db
  forcedelete a'z.db-journal
  sqlite3 db2 a'z.db
  execsql {
    CREATE TABLE t1(t);
    VACUUM;
  } db2
} {}
db2 close
ifcapable {autoinc} {
  do_test vacuum-9.1 {
    execsql {
      DROP TABLE 'abc abc';
      CREATE TABLE autoinc(a INTEGER PRIMARY KEY AUTOINCREMENT, b);
      INSERT INTO autoinc(b) VALUES('hi');
      INSERT INTO autoinc(b) VALUES('there');
      DELETE FROM autoinc;
    }
    set ::cksum [cksum]
    expr {$::cksum!=""}
  } {1}
  do_test vacuum-9.2 {
    execsql {
      VACUUM;
    }
    cksum
  } $::cksum
  do_test vacuum-9.3 {
    execsql {
      INSERT INTO autoinc(b) VALUES('one');
      INSERT INTO autoinc(b) VALUES('two');
    }
    set ::cksum [cksum]
    expr {$::cksum!=""}
  } {1}
  do_test vacuum-9.4 {
    execsql {
      VACUUM;
    }
    cksum
  } $::cksum
}
forcedelete {a'z.db}
do_test vacuum-10.1 {
  db close
  forcedelete test.db
  sqlite3 db test.db
  execsql {
    CREATE TABLE t8(a, b);
    INSERT INTO t8 VALUES('a', 'b');
    INSERT INTO t8 VALUES('c', 'd');
    PRAGMA count_changes = 1;
  }
} {}
do_test vacuum-10.2 { execsql VACUUM } {}
do_execsql_test vacuum-11.1 {
  PRAGMA page_size=1024;
  VACUUM;
  PRAGMA page_size;
} {1024}
sqlite3_db_config db ATTACH_CREATE 0
do_execsql_test vacuum-11.2 {
  PRAGMA page_size=2048;
  VACUUM;
  PRAGMA page_size;
} {2048}
sqlite3_db_config db ATTACH_CREATE 1
sqlite3_db_config db ATTACH_WRITE 0
do_execsql_test vacuum-11.3 {
  PRAGMA page_size=4096;
  VACUUM;
  PRAGMA page_size;
} {4096}
finish_test
"#);
}
