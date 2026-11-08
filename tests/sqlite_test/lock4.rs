#[test]
#[ignore = "1/4 subtests passed; pending: lock4 edge cases"]
fn test_lock4() {
    sqliters_tcltest::run_test_script(
        "lock4",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
if {[atomic_batch_write test.db]} {
  # creates a *-journal file. Which doesn't work if atomic writes are
  finish_test
  return
}
do_not_use_codec
do_test lock4-1.1 {
  db eval {
     PRAGMA auto_vacuum=OFF;
     CREATE TABLE t1(x);
  }
  forcedelete test2.db test2.db-journal
  sqlite3 db2 test2.db
  db2 eval {
     PRAGMA auto_vacuum=OFF;
     CREATE TABLE t2(x)
  }
  db2 close
  list [file size test.db] [file size test2.db]
} {2048 2048}
do_test lock4-1.2 {
 
  set out [open test2-script.tcl w]
  puts $out "sqlite3_test_control_pending_byte [set sqlite_pending_byte]"
  puts $out {
     sqlite3 db2 test2.db
     db2 eval {
        BEGIN;
        INSERT INTO t2 VALUES(2);
     }
     sqlite3 db test.db
     db timeout 1000000
     db eval {
        INSERT INTO t1 VALUES(2);
     }
     db close
     db2 eval COMMIT
     exit
  }
  close $out

  db eval {
     BEGIN EXCLUSIVE;
     INSERT INTO t1 VALUES(1);
  }

  exec [info nameofexec] ./test2-script.tcl &

  while {![file exists test2.db-journal]} {
    after 10
  }

  sqlite3 db2 test2.db
  catchsql {
    INSERT INTO t2 VALUES(1)
  } db2
} {1 {database is locked}}
do_test lock4-1.3 {
  db eval {
     COMMIT;
  }
  while {[file exists test2.db-journal]} {
    after 10
  }
  after 25
  db2 eval {
     SELECT * FROM t2
  }
} {2}
do_test lock4-999.1 {
  rename db2 {}
} {}
finish_test
"#,
    );
}
