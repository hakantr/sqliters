#[test]
#[ignore = "48/64 subtests passed; pending: TEMP TABLE"]
fn test_lock() {
    sqliters_tcltest::run_test_script("lock", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test lock-1.0 {
  file mkdir tempdir/t1/t2
  sqlite3 db2 ./tempdir/../tempdir/t1/.//t2/../../..//test.db
  set dummy {}
} {}
do_test lock-1.1 {
  execsql {SELECT name FROM sqlite_master WHERE type='table' ORDER BY name}
} {}
do_test lock-1.2 {
  execsql {SELECT name FROM sqlite_master WHERE type='table' ORDER BY name} db2
} {}
do_test lock-1.3 {
  execsql {CREATE TABLE t1(a int, b int)}
  execsql {SELECT name FROM sqlite_master WHERE type='table' ORDER BY name}
} {t1}
do_test lock-1.5 {
  catchsql {
     SELECT name FROM sqlite_master WHERE type='table' ORDER BY name
  } db2
} {0 t1}
do_test lock-1.6 {
  execsql {INSERT INTO t1 VALUES(1,2)}
  execsql {SELECT * FROM t1}
} {1 2}
do_test lock-1.7.2 {
  catchsql {SELECT * FROM t1} db2
} {0 {1 2}}
do_test lock-1.8 {
  execsql {UPDATE t1 SET a=b, b=a} db2
  execsql {SELECT * FROM t1} db2
} {2 1}
do_test lock-1.9 {
  execsql {SELECT * FROM t1}
} {2 1}
do_test lock-1.10 {
  execsql {BEGIN TRANSACTION}
  execsql {UPDATE t1 SET a = 0 WHERE 0}
  execsql {SELECT * FROM t1}
} {2 1}
do_test lock-1.11 {
  catchsql {SELECT * FROM t1} db2
} {0 {2 1}}
do_test lock-1.12 {
  execsql {ROLLBACK}
  catchsql {SELECT * FROM t1}
} {0 {2 1}}
do_test lock-1.13 {
  execsql {CREATE TABLE t2(x int, y int)}
  execsql {INSERT INTO t2 VALUES(8,9)}
  execsql {SELECT * FROM t2}
} {8 9}
do_test lock-1.14.1 {
  catchsql {SELECT * FROM t2} db2
} {0 {8 9}}
do_test lock-1.14.2 {
  catchsql {SELECT * FROM t1} db2
} {0 {2 1}}
do_test lock-1.15 {
  catchsql {SELECT * FROM t2} db2
} {0 {8 9}}
do_test lock-1.16 {
  db eval {SELECT * FROM t1} qv {
    set x [db eval {SELECT * FROM t1}]
  }
  set x
} {2 1}
do_test lock-1.17 {
  db eval {SELECT * FROM t1} qv {
    set x [db eval {SELECT * FROM t2}]
  }
  set x
} {8 9}
do_test lock-1.19 {
  db eval {SELECT * FROM t1} qv {
    set r [catch {db eval {UPDATE t2 SET x=y, y=x}} msg]
    lappend r $msg
  }
  set r
} {0 {}}
do_test lock-1.20 {
  execsql {SELECT * FROM t2}
} {9 8}
do_test lock-1.21 {
  db eval {SELECT * FROM t1} qv {
    set r [catch {db eval {SELECT a FROM t1}} msg]
    lappend r $msg
  }
  set r
} {0 2}
if {$::tcl_platform(platform) eq "unix"} {
  do_test lock-1.22 {
    db eval {SELECT * FROM t1} qv {
      set r [catch {db2 eval {SELECT a FROM t1}} msg]
      lappend r $msg
    }
    set r
  } {0 2}
}
integrity_check lock-1.23
do_test lock-2.1 {
  execsql {BEGIN TRANSACTION}
  execsql {UPDATE t1 SET a = 0 WHERE 0}
  execsql {BEGIN TRANSACTION} db2
  set r [catch {execsql {UPDATE t1 SET a = 0 WHERE 0} db2} msg]
  execsql {ROLLBACK} db2
  lappend r $msg
} {1 {database is locked}}
do_test lock-2.2 {
  catchsql {SELECT * FROM t2} db2
} {0 {9 8}}
do_test lock-2.3.1 {
  proc callback {count} {
    set ::callback_value $count
    break
  }
  set ::callback_value {}
  db2 busy callback
  set r [catch {execsql {UPDATE t1 SET a=b, b=a} db2} msg]
  lappend r $msg
  lappend r $::callback_value
} {1 {database is locked} 0}
do_test lock-2.3.2 {
  set ::callback_value {}
  execsql {BEGIN; SELECT rowid FROM sqlite_master LIMIT 1} db2
  set r [catch {execsql {UPDATE t1 SET a=b, b=a} db2} msg]
  lappend r $msg
  lappend r $::callback_value
} {1 {database is locked} {}}
catch {execsql {ROLLBACK} db2}
do_test lock-2.4.1 {
  proc callback {count} {
    lappend ::callback_value $count
    if {$count>4} break
  }
  set ::callback_value {}
  db2 busy callback
  set r [catch {execsql {UPDATE t1 SET a=b, b=a} db2} msg]
  lappend r $msg
  lappend r $::callback_value
} {1 {database is locked} {0 1 2 3 4 5}}
do_test lock-2.4.2 {
  proc callback {count} {
    lappend ::callback_value $count
    if {$count>4} break
  }
  set ::callback_value {}
  db2 busy callback
  execsql {BEGIN; SELECT rowid FROM sqlite_master LIMIT 1} db2
  set r [catch {execsql {UPDATE t1 SET a=b, b=a} db2} msg]
  lappend r $msg
  lappend r $::callback_value
} {1 {database is locked} {}}
catch {execsql {ROLLBACK} db2}
do_test lock-2.5 {
  proc callback {count} {
    lappend ::callback_value $count
    if {$count>4} break
  }
  set ::callback_value {}
  db2 busy callback
  set r [catch {execsql {SELECT * FROM t1} db2} msg]
  lappend r $msg
  lappend r $::callback_value
} {0 {2 1} {}}
execsql {ROLLBACK}
do_test lock-2.8 {
  db2 timeout 400
  execsql BEGIN
  execsql {UPDATE t1 SET a = 0 WHERE 0}
  catchsql {BEGIN EXCLUSIVE;} db2
} {1 {database is locked}}
do_test lock-2.8b {
  db2 eval {PRAGMA busy_timeout}
} {400}
do_test lock-2.9 {
  db2 timeout 0
  execsql COMMIT
} {}
do_test lock-2.9b {
  db2 eval {PRAGMA busy_timeout}
} {0}
integrity_check lock-2.10
do_test lock-2.11 {
  db2 eval {PRAGMA busy_timeout(400)}
  execsql BEGIN
  execsql {UPDATE t1 SET a = 0 WHERE 0}
  catchsql {BEGIN EXCLUSIVE;} db2
} {1 {database is locked}}
do_test lock-2.11b {
  db2 eval {PRAGMA busy_timeout}
} {400}
do_test lock-2.12 {
  db2 eval {PRAGMA busy_timeout(0)}
  execsql COMMIT
} {}
do_test lock-2.12b {
  db2 eval {PRAGMA busy_timeout}
} {0}
integrity_check lock-2.13
do_test lock-3.1 {
  execsql {BEGIN TRANSACTION}
  set r [catch {execsql {BEGIN TRANSACTION}} msg]
  execsql {ROLLBACK}
  lappend r $msg
} {1 {cannot start a transaction within a transaction}}
integrity_check lock-3.2
do_test lock-4.1 {
  db2 close
  catch {db eval ROLLBACK}
  db eval BEGIN
  db eval {UPDATE t1 SET a=0 WHERE 0}
  sqlite3 db2 ./test.db
  catchsql {UPDATE t1 SET a=0} db2
} {1 {database is locked}}
do_test lock-4.2 {
  set ::callback_value {}
  set rc [catch {db2 eval {UPDATE t1 SET a=0}} msg]
  lappend rc $msg $::callback_value
} {1 {database is locked} {}}
do_test lock-4.3 {
  proc callback {count} {
    lappend ::callback_value $count
    if {$count>4} break
  }
  db2 busy callback
  set rc [catch {db2 eval {UPDATE t1 SET a=0}} msg]
  lappend rc $msg $::callback_value
} {1 {database is locked} {0 1 2 3 4 5}}
execsql {ROLLBACK}
proc tx_exec {sql} {
  db2 eval $sql
}
do_test lock-5.1 {
  execsql {
    SELECT * FROM t1
  }
} {2 1}
do_test lock-5.2 {
  db function tx_exec tx_exec
  catchsql {
    INSERT INTO t1(a,b) SELECT 3, tx_exec('SELECT y FROM t2 LIMIT 1');
  }
} {0 {}}
ifcapable tempdb {
  do_test lock-5.3 {
    execsql {
      CREATE TEMP TABLE t3(x);
      SELECT * FROM t3;
    }
  } {}
  do_test lock-5.4 {
    catchsql {
      INSERT INTO t3 SELECT tx_exec('SELECT y FROM t2 LIMIT 1');
    }
  } {0 {}}
  do_test lock-5.5 {
    execsql {
      SELECT * FROM t3;
    }
  } {8}
  do_test lock-5.6 {
    catchsql {
      UPDATE t1 SET a=tx_exec('SELECT x FROM t2');
    }
  } {0 {}}
  do_test lock-5.7 {
    execsql {
      SELECT * FROM t1;
    }
  } {9 1 9 8}
  do_test lock-5.8 {
    catchsql {
      UPDATE t3 SET x=tx_exec('SELECT x FROM t2');
    }
  } {0 {}}
  do_test lock-5.9 {
    execsql {
      SELECT * FROM t3;
    }
  } {9}
}
do_test lock-6.1 {
  execsql {
    CREATE TABLE t4(a PRIMARY KEY, b);
    INSERT INTO t4 VALUES(1, 'one');
    INSERT INTO t4 VALUES(2, 'two');
    INSERT INTO t4 VALUES(3, 'three');
  }

  set STMT [sqlite3_prepare $DB "SELECT * FROM sqlite_master" -1 TAIL]
  sqlite3_step $STMT

  execsql { DELETE FROM t4 }
  execsql { SELECT * FROM sqlite_master } db2
  execsql { SELECT * FROM t4 } db2
} {}
do_test lock-6.2 {
  execsql { 
    BEGIN;
    INSERT INTO t4 VALUES(1, 'one');
    INSERT INTO t4 VALUES(2, 'two');
    INSERT INTO t4 VALUES(3, 'three');
    COMMIT;
  }

  execsql { SELECT * FROM t4 } db2
} {1 one 2 two 3 three}
do_test lock-6.3 {
  execsql { SELECT a FROM t4 ORDER BY a } db2
} {1 2 3}
do_test lock-6.4 {
  execsql { PRAGMA integrity_check } db2
} {ok}
do_test lock-6.5 {
  sqlite3_finalize $STMT
} {SQLITE_OK}
set temp_status unknown
do_test lock-7.1 {
  set STMT [sqlite3_prepare $DB "SELECT * FROM sqlite_master" -1 TAIL]
  sqlite3_step $STMT
} {SQLITE_ROW}
do_test lock-7.2 {
  execsql { PRAGMA lock_status }
} [list main shared temp $temp_status]
do_test lock-7.3 {
  execsql {
    PRAGMA journal_mode = truncate;
    BEGIN;
    UPDATE t4 SET a = 10 WHERE 0;
    COMMIT;
  }
  execsql { PRAGMA lock_status }
} [list main shared temp $temp_status]
do_test lock-7.4 {
  sqlite3_finalize $STMT
} {SQLITE_OK}
do_test lock-999.1 {
  rename db2 {}
} {}
finish_test
"#);
}
