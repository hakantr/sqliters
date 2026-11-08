#[test]
#[ignore = "6/24 subtests passed; pending: ATTACH"]
fn test_pragma2() {
    sqliters_tcltest::run_test_script("pragma2", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable !pragma||!schema_pragmas {
  finish_test
  return
}
test_set_config_pagecache 0 0
db close
delete_file test.db test.db-journal
delete_file test3.db test3.db-journal
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
db eval {PRAGMA auto_vacuum=0}
do_test pragma2-1.1 {
  execsql {
    PRAGMA freelist_count;
  }
} {0}
do_test pragma2-1.2 {
  execsql {
    CREATE TABLE abc(a, b, c);
    PRAGMA freelist_count;
  }
} {0}
do_test pragma2-1.3 {
  execsql {
    DROP TABLE abc;
    PRAGMA freelist_count;
  }
} {1}
do_test pragma2-1.4 {
  execsql {
    PRAGMA main.freelist_count;
  }
} {1}
forcedelete test2.db
forcedelete test2.db-journal
ifcapable attach {
  do_test pragma2-2.1 {
    execsql {
      ATTACH 'test2.db' AS aux;
      PRAGMA aux.auto_vacuum=OFF;
      PRAGMA aux.freelist_count;
    }
  } {0}
  do_test pragma2-2.2 {
    execsql {
      CREATE TABLE aux.abc(a, b, c);
      PRAGMA aux.freelist_count;
    }
  } {0}
  do_test pragma2-2.3 {
    set ::val [string repeat 0123456789 1000]
    execsql {
      INSERT INTO aux.abc VALUES(1, 2, $::val);
      PRAGMA aux.freelist_count;
    }
  } {0}
  do_test pragma2-2.4 {
    expr {[file size test2.db] / 1024}
  } {11}
  do_test pragma2-2.5 {
    execsql {
      DELETE FROM aux.abc;
      PRAGMA aux.freelist_count;
    }
  } {9}
  
  do_test pragma2-3.1 {
    execsql {
      PRAGMA aux.freelist_count;
      PRAGMA main.freelist_count;
      PRAGMA freelist_count;
    }
  } {9 1 1}
  do_test pragma2-3.2 {
    execsql {
      PRAGMA freelist_count = 500;
      PRAGMA freelist_count;
    }
  } {1 1}
  do_test pragma2-3.3 {
    execsql {
      PRAGMA aux.freelist_count = 500;
      PRAGMA aux.freelist_count;
    }
  } {9 9}
}
db close
delete_file test.db test.db-journal
delete_file test2.db test2.db-journal
sqlite3 db test.db
do_execsql_test pragma2-4.1 {
  PRAGMA main.cache_size=2000;
  PRAGMA temp.cache_size=2000;
  PRAGMA cache_spill;
  PRAGMA main.cache_spill;
  PRAGMA temp.cache_spill;
} {2000 2000 2000}
do_execsql_test pragma2-4.2 {
  PRAGMA cache_spill=OFF;
  PRAGMA cache_spill;
  PRAGMA main.cache_spill;
  PRAGMA temp.cache_spill;
} {0 0 0}
do_execsql_test pragma2-4.3 {
  PRAGMA page_size=1024;
  PRAGMA cache_size=50;
  BEGIN;
  CREATE TABLE t1(a INTEGER PRIMARY KEY, b, c, d);
  INSERT INTO t1 VALUES(1, randomblob(400), 1, randomblob(400));
  INSERT INTO t1 SELECT a+1, randomblob(400), a+1, randomblob(400) FROM t1;
  INSERT INTO t1 SELECT a+2, randomblob(400), a+2, randomblob(400) FROM t1;
  INSERT INTO t1 SELECT a+4, randomblob(400), a+4, randomblob(400) FROM t1;
  INSERT INTO t1 SELECT a+8, randomblob(400), a+8, randomblob(400) FROM t1;
  INSERT INTO t1 SELECT a+16, randomblob(400), a+16, randomblob(400) FROM t1;
  INSERT INTO t1 SELECT a+32, randomblob(400), a+32, randomblob(400) FROM t1;
  INSERT INTO t1 SELECT a+64, randomblob(400), a+64, randomblob(400) FROM t1;
  COMMIT;
  ATTACH 'test2.db' AS aux1;
  CREATE TABLE aux1.t2(a INTEGER PRIMARY KEY, b, c, d);
  INSERT INTO t2 SELECT * FROM t1;
  DETACH aux1;
  PRAGMA cache_spill=ON;
} {}
sqlite3_release_memory
do_test pragma2-4.4 {
  db eval {
    BEGIN;
    UPDATE t1 SET c=c+1;
    PRAGMA lock_status;
  }
} {main exclusive temp unknown}  ;# EXCLUSIVE lock due to cache spill
do_test pragma2-4.5.1 {
  db eval {
    ROLLBACK;
    PRAGMA cache_spill=OFF;
    PRAGMA Cache_Spill;
    BEGIN;
    UPDATE t1 SET c=c+1;
    PRAGMA lock_status;
  }
} {0 main reserved temp unknown}   ;# No cache spill, so no exclusive lock
do_test pragma2-4.5.2 {
  db eval {
    ROLLBACK;
    PRAGMA cache_spill=100000;
    PRAGMA cache_spill;
    BEGIN;
    UPDATE t1 SET c=c+1;
    PRAGMA lock_status;
  }
} {100000 main reserved temp unknown}   ;# Big spill threshold -> no excl lock
ifcapable !memorymanage {
  do_test pragma2-4.5.3 {
    db eval {
      ROLLBACK;
      PRAGMA cache_spill=25;
      PRAGMA main.cache_spill;
      BEGIN;
      UPDATE t1 SET c=c+1;
      PRAGMA lock_status;
    }
  } {50 main exclusive temp unknown}   ;# Small cache spill -> exclusive lock
  do_test pragma2-4.5.4 {
    db eval {
      ROLLBACK;
      PRAGMA cache_spill(-25);
      PRAGMA main.cache_spill;
      BEGIN;
      UPDATE t1 SET c=c+1;
      PRAGMA lock_status;
    }
  } {50 main exclusive temp unknown}   ;# Small cache spill -> exclusive lock
}
do_execsql_test pragma2-4.6 {
  ROLLBACK;
  PRAGMA cache_spill=OFF;
  ATTACH 'test2.db' AS aux1;
  PRAGMA aux1.cache_size=50;
  BEGIN;
  UPDATE t2 SET c=c+1;
  PRAGMA lock_status;
} {main unlocked temp unknown aux1 reserved}
do_execsql_test pragma2-4.7 {
  COMMIT;
}
sqlite3_release_memory
do_execsql_test pragma2-4.8 {
  PRAGMA cache_spill=ON; -- Applies to all databases
  BEGIN;
  UPDATE t2 SET c=c-1;
  PRAGMA lock_status;
} {main unlocked temp unknown aux1 exclusive}
db close
forcedelete test.db
sqlite3 db test.db
do_execsql_test pragma2-5.1 {
  PRAGMA page_size=16384;
  CREATE TABLE t1(x);
  PRAGMA cache_size=2;
  PRAGMA cache_spill=YES;
  PRAGMA cache_spill;
} {2}
do_execsql_test pragma2-5.2 {
  PRAGMA cache_spill=NO;
  PRAGMA cache_spill;
} {0}
do_execsql_test pragma2-5.3 {
  PRAGMA cache_spill(-51);
  PRAGMA cache_spill;
} {3}
test_restore_config_pagecache
finish_test
"#);
}
