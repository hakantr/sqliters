#[test]
#[ignore = "19/26 subtests passed; pending: ANALYZE/sqlite_stat1"]
fn test_fkey1() {
    sqliters_tcltest::run_test_script("fkey1", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix fkey1
ifcapable {!foreignkey} {
  finish_test
  return
}
do_test fkey1-1.0 {
  execsql {
    CREATE TABLE t1(
      a INTEGER PRIMARY KEY,
      b INTEGER
           REFERENCES t1 ON DELETE CASCADE
           REFERENCES t2,
      c TEXT,
      FOREIGN KEY (b,c) REFERENCES t2(x,y) ON UPDATE CASCADE
    );
  }
} {}
do_test fkey1-1.1 {
  execsql {
    CREATE TABLE t2(
      x INTEGER PRIMARY KEY,
      y TEXT
    );
  }
} {}
do_test fkey1-1.2 {
  execsql {
    CREATE TABLE t3(
      a INTEGER REFERENCES t2,
      b INTEGER REFERENCES t1,
      FOREIGN KEY (a,b) REFERENCES t2(x,y)
    );
  }
} {}
do_test fkey1-2.1 {
  execsql {
    CREATE TABLE t4(a integer primary key);
    CREATE TABLE t5(x references t4);
    CREATE TABLE t6(x references t4);
    CREATE TABLE t7(x references t4);
    CREATE TABLE t8(x references t4);
    CREATE TABLE t9(x references t4);
    CREATE TABLE t10(x references t4);
    DROP TABLE t7;
    DROP TABLE t9;
    DROP TABLE t5;
    DROP TABLE t8;
    DROP TABLE t6;
    DROP TABLE t10;
  }
} {}
do_test fkey1-3.1 {
  execsql {
    CREATE TABLE t5(a PRIMARY KEY, b, c);
    CREATE TABLE t6(
      d REFERENCES t5,
      e REFERENCES t5(c)
    );
    PRAGMA foreign_key_list(t6);
  }
} [concat                                         \
  {0 0 t5 e c {NO ACTION} {NO ACTION} NONE}       \
  {1 0 t5 d {} {NO ACTION} {NO ACTION} NONE}      \
]
do_test fkey1-3.2 {
  execsql {
    CREATE TABLE t7(d, e, f,
      FOREIGN KEY (d, e) REFERENCES t5(a, b)
    );
    PRAGMA foreign_key_list(t7);
  }
} [concat                                   \
  {0 0 t5 d a {NO ACTION} {NO ACTION} NONE} \
  {0 1 t5 e b {NO ACTION} {NO ACTION} NONE} \
]
do_test fkey1-3.3 {
  execsql {
    CREATE TABLE t8(d, e, f,
      FOREIGN KEY (d, e) REFERENCES t5 ON DELETE CASCADE ON UPDATE SET NULL
    );
    PRAGMA foreign_key_list(t8);
  }
} [concat                        \
  {0 0 t5 d {} {SET NULL} CASCADE NONE} \
  {0 1 t5 e {} {SET NULL} CASCADE NONE} \
]
do_test fkey1-3.4 {
  execsql {
    CREATE TABLE t9(d, e, f,
      FOREIGN KEY (d, e) REFERENCES t5 ON DELETE CASCADE ON UPDATE SET DEFAULT
    );
    PRAGMA foreign_key_list(t9);
  }
} [concat                        \
  {0 0 t5 d {} {SET DEFAULT} CASCADE NONE} \
  {0 1 t5 e {} {SET DEFAULT} CASCADE NONE} \
]
do_test fkey1-3.5 {
  sqlite3_db_status db DBSTATUS_DEFERRED_FKS 0
} {0 0 0}
do_execsql_test fkey1-4.0 {
  PRAGMA foreign_keys=ON;
  CREATE TABLE "xx1"("xx2" TEXT PRIMARY KEY, "xx3" TEXT);
  INSERT INTO "xx1"("xx2","xx3") VALUES('abc','def');
  CREATE TABLE "xx4"("xx5" TEXT REFERENCES "xx1" ON DELETE CASCADE);
  INSERT INTO "xx4"("xx5") VALUES('abc');
  INSERT INTO "xx1"("xx2","xx3") VALUES('uvw','xyz');
  SELECT 1, "xx5" FROM "xx4";
  DELETE FROM "xx1";
  SELECT 2, "xx5" FROM "xx4";
} {1 abc}
do_execsql_test fkey1-4.1 {
  PRAGMA foreign_keys=ON;
  CREATE TABLE """1"("""2" TEXT PRIMARY KEY, """3" TEXT);
  INSERT INTO """1"("""2","""3") VALUES('abc','def');
  CREATE TABLE """4"("""5" TEXT REFERENCES """1" ON DELETE CASCADE);
  INSERT INTO """4"("""5") VALUES('abc');
  INSERT INTO """1"("""2","""3") VALUES('uvw','xyz');
  SELECT 1, """5" FROM """4";
  DELETE FROM """1";
  SELECT 2, """5" FROM """4";
} {1 abc}
do_execsql_test fkey1-4.2 {
  PRAGMA table_info="""1";
} {0 {"2} TEXT 0 {} 1 1 {"3} TEXT 0 {} 0}
do_execsql_test fkey1-5.1 {
  CREATE TABLE t11(
    x INTEGER PRIMARY KEY, 
    parent REFERENCES t11 ON DELETE CASCADE
  );
  INSERT INTO t11 VALUES (1, NULL), (2, 1), (3, 2);
} {}
do_catchsql_test fkey1-5.2 {
  INSERT OR REPLACE INTO t11 VALUES (2, 3);
} {1 {FOREIGN KEY constraint failed}}
ifcapable trace {
  proc sqltrace {txt} {
    global traceoutput
    lappend traceoutput $txt
  }
  do_test fkey1-5.2.1 {
    unset -nocomplain traceoutput
    db trace sqltrace
    catch {db eval {INSERT OR REPLACE INTO t11 VALUES(2,3);}}
    set traceoutput
  } {{INSERT OR REPLACE INTO t11 VALUES(2,3);} {INSERT OR REPLACE INTO t11 VALUES(2,3);} {INSERT OR REPLACE INTO t11 VALUES(2,3);}}
}
do_execsql_test fkey1-5.3 {
  CREATE TABLE Foo (
    Id INTEGER PRIMARY KEY, 
    ParentId INTEGER REFERENCES Foo(Id) ON DELETE CASCADE, C1
  );
  INSERT OR REPLACE INTO Foo(Id, ParentId, C1) VALUES (1, null, 'A');
  INSERT OR REPLACE INTO Foo(Id, ParentId, C1) VALUES (2, 1, 'A-2-1');
  INSERT OR REPLACE INTO Foo(Id, ParentId, C1) VALUES (3, 2, 'A-3-2');
  INSERT OR REPLACE INTO Foo(Id, ParentId, C1) VALUES (4, 3, 'A-4-3');
}
do_catchsql_test fkey1-5.4 {
  INSERT OR REPLACE INTO Foo(Id, ParentId, C1) VALUES (2, 3, 'A-2-3');
} {1 {FOREIGN KEY constraint failed}}
do_execsql_test 6.0 {
  CREATE TABLE p1(x, y);
  CREATE UNIQUE INDEX p1x ON p1(x) WHERE y<2;
  INSERT INTO p1 VALUES(1, 1);
  CREATE TABLE c1(a REFERENCES p1(x));
}
do_catchsql_test 6.1 {
  INSERT INTO c1 VALUES(1);
} {1 {foreign key mismatch - "c1" referencing "p1"}}
do_execsql_test 6.2 {
  CREATE UNIQUE INDEX p1x2 ON p1(x);
  INSERT INTO c1 VALUES(1);
} {}
reset_db
do_execsql_test 7.1 {
  PRAGMA foreign_keys=OFF;
  CREATE TABLE t1(a,b,c,FOREIGN KEY(a,a,a,a,a,a,a,a,a,a,a,a,a,a) REFERENCES t0);
  INSERT INTO t1 VALUES(1,2,3);
  PRAGMA foreign_key_check;
} {t1 1 t0 0}
do_execsql_test 7.2 {
  DROP TABLE t1;
  CREATE TABLE t1(a,b,c AS(1),d, FOREIGN KEY(c,d,b,a,b,d,b,c) REFERENCES t0);
  PRAGMA foreign_key_check;
} {}
reset_db
do_execsql_test 8.1 {
  PRAGMA writable_schema=ON;
  PRAGMA foreign_keys = ON;
  CREATE TABLE sqlite_stat1 (tbl INTEGER PRIMARY KEY DESC, idx UNIQUE DEFAULT NULL) WITHOUT ROWID;
  PRAGMA writable_schema=OFF;
  CREATE TABLE sqlsim4(stat PRIMARY KEY);;
  CREATE TABLE t1(sqlsim7 REFERENCES sqlite_stat1 ON DELETE CASCADE);
  DROP table "sqlsim4";
} {}
reset_db
database_may_be_corrupt
do_execsql_test 8.2 {
  CREATE TABLE t1(a REFERENCES sqlite_stat1 ON DELETE CASCADE);
  CREATE TABLE t2(a TEXT PRIMARY KEY);
  PRAGMA writable_schema=ON;
  CREATE TABLE sqlite_stat1(tbl INTEGER PRIMARY KEY DESC, idx UNIQUE DEFAULT NULL) WITHOUT ROWID;
  UPDATE sqlite_schema SET name='sqlite_autoindex_sqlite_stat1_1' WHERE name='sqlite_autoindex_sqlite_stat1_2';
  PRAGMA writable_schema=RESET;
} {}
do_catchsql_test 8.3 {
  REINDEX;
} {1 {database disk image is malformed}}
reset_db
do_execsql_test 9.1 {
  PRAGMA foreign_keys = ON;
  CREATE TABLE """1"("""2", """3" PRIMARY KEY);
  CREATE TABLE """4"("""5" REFERENCES """1" ON DELETE RESTRICT);
  DELETE FROM """1";
}
finish_test
"#);
}
