#[test]
#[ignore = "0/24 subtests passed; pending: pragma3 edge cases"]
fn test_pragma3() {
    sqliters_tcltest::run_test_script(
        "pragma3",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
if {[sqlite3 -has-codec]} {
  finish_test
  return
}
do_execsql_test pragma3-100 {
  PRAGMA data_version;
} {1}
do_execsql_test pragma3-101 {
  PRAGMA temp.data_version;
} {1}
do_execsql_test pragma3-102 {
  PRAGMA main.data_version=1234;
  PRAGMA main.data_version;
} {1 1}
do_execsql_test pragma3-110 {
  PRAGMA data_version;
  BEGIN IMMEDIATE;
  PRAGMA data_version;
  CREATE TABLE t1(a);
  INSERT INTO t1 VALUES(100),(200),(300);
  PRAGMA data_version;
  COMMIT;
  SELECT * FROM t1;
  PRAGMA data_version;
} {1 1 1 100 200 300 1}
sqlite3 db2 test.db
do_test pragma3-120 {
  db2 eval {
    SELECT * FROM t1;
    PRAGMA data_version;
  }
} {100 200 300 1}
do_execsql_test pragma3-130 {
  PRAGMA data_version;
  BEGIN IMMEDIATE;
  PRAGMA data_version;
  INSERT INTO t1 VALUES(400),(500);
  PRAGMA data_version;
  COMMIT;
  SELECT * FROM t1;
  PRAGMA data_version;
  PRAGMA shrink_memory;
} {1 1 1 100 200 300 400 500 1}
do_test pragma3-140 {
  db2 eval {
    SELECT * FROM t1;
    PRAGMA data_version;
    BEGIN IMMEDIATE;
    PRAGMA data_version;
    UPDATE t1 SET a=a+1;
    COMMIT;
    SELECT * FROM t1;
    PRAGMA data_version;
  }
} {100 200 300 400 500 2 2 101 201 301 401 501 2}
do_execsql_test pragma3-150 {
  SELECT * FROM t1;
  PRAGMA data_version;
} {101 201 301 401 501 2}
do_test pragma3-160 {
  db eval {
    BEGIN;
    PRAGMA data_version;
    UPDATE t1 SET a=555 WHERE a=501;
    PRAGMA data_version;
    SELECT * FROM t1 ORDER BY a;
    PRAGMA data_version;
  }
} {2 2 101 201 301 401 555 2}
do_test pragma3-170 {
  db2 eval {
    PRAGMA data_version;
  }
} {2}
do_test pragma3-180 {
  db eval {
    COMMIT;
    PRAGMA data_version;
  }
} {2}
do_test pragma3-190 {
  db2 eval {
    PRAGMA data_version;
  }
} {3}
do_test pragma3-195 {
  expr {[db eval {PRAGMA data_version}]!=[db2 eval {PRAGMA data_version}]}
} {1}
do_test pragma3-200 {
  db eval {PRAGMA data_version; SELECT * FROM t1;}
} {2 101 201 301 401 555}
do_test pragma3-201 {
  set fd [open pragma3.txt wb]
  puts $fd {
     sqlite3 db test.db;
     db eval {DELETE FROM t1 WHERE a>300};
     db close;
     exit;
  }
  close $fd
  exec [info nameofexec] pragma3.txt
  forcedelete pragma3.txt
  db eval {
    PRAGMA data_version;
    SELECT * FROM t1;
  }
} {3 101 201}
db2 close
db close
ifcapable shared_cache {
  set ::enable_shared_cache [sqlite3_enable_shared_cache 1]
  sqlite3 db test.db
  sqlite3 db2 test.db
  do_test pragma3-300 {
    db eval {
      PRAGMA data_version;
      BEGIN;
      CREATE TABLE t3(a,b,c);
      CREATE TABLE t4(x,y,z);
      INSERT INTO t4 VALUES(123,456,789);
      PRAGMA data_version;
      COMMIT;
      PRAGMA data_version;
    }
  } {1 1 1}
  do_test pragma3-310 {
    db2 eval {
      PRAGMA data_version;
      BEGIN;
      INSERT INTO t3(a,b,c) VALUES('abc','def','ghi');
      SELECT * FROM t3;
      PRAGMA data_version;
    }
  } {2 abc def ghi 2}
  do_test pragma3-320 {
    db eval {
      PRAGMA data_version;
      SELECT * FROM t4;
    }
  } {1 123 456 789}
  do_test pragma3-330 {
    db2 eval {
      COMMIT;
      PRAGMA data_version;
      SELECT * FROM t4;
    }
  } {2 123 456 789}
  do_test pragma3-340 {
    db eval {
      PRAGMA data_version;
      SELECT * FROM t3;
      SELECT * FROM t4;
    }
  } {2 abc def ghi 123 456 789}
  db2 close
  db close
  sqlite3_enable_shared_cache $::enable_shared_cache
}
if {[wal_is_capable]} {
if {[permutation]!="inmemory_journal"} {

  sqlite3 db test.db
  db eval {PRAGMA journal_mode=WAL}
  sqlite3 db2 test.db
  do_test pragma3-400 {
    db eval {
      PRAGMA data_version;
      PRAGMA journal_mode;
      SELECT * FROM t1;
    }
  } {2 wal 101 201}
  do_test pragma3-410 {
    db2 eval {
      PRAGMA data_version;
      PRAGMA journal_mode;
      SELECT * FROM t1;
    }
  } {2 wal 101 201}
  do_test pragma3-420 {
    db eval {UPDATE t1 SET a=111*(a/100); PRAGMA data_version; SELECT * FROM t1}
  } {2 111 222}
  do_test pragma3-430 {
    db2 eval {PRAGMA data_version; SELECT * FROM t1;}
  } {3 111 222}
  db2 close
}
}
foreach {tn sql} {
  A {
  }
  B {
    PRAGMA journal_mode = PERSIST;
    PRAGMA locking_mode = EXCLUSIVE;
  }
} {
  reset_db
  execsql $sql

  do_execsql_test pragma3-510$tn {
    CREATE TABLE t1(x, y);
    INSERT INTO t1 VALUES(1, 2);
    PRAGMA data_version;
  } {1}

  do_execsql_test pragma3-520$tn {
    BEGIN EXCLUSIVE;
    COMMIT;
    PRAGMA data_version;
  } {1}
}
finish_test
"#,
    );
}
