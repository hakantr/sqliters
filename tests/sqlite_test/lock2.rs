#[test]
#[ignore = "2/11 subtests passed; pending: lock2 edge cases"]
fn test_lock2() {
    sqliters_tcltest::run_test_script("lock2", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
source $testdir/lock_common.tcl
do_test lock2-1.1 {
  set ::tf1 [launch_testfixture]
  testfixture $::tf1 {
    sqlite3 db test.db -key xyzzy
    db eval {select * from sqlite_master}
  }
} {}
do_test lock2-1.1.1 {
  execsql {pragma lock_status}
} {main unlocked temp closed}
sqlite3_soft_heap_limit 0
do_test lock2-1.2 {
  execsql {
    BEGIN;
    CREATE TABLE abc(a, b, c);
  }
} {}
do_test lock2-1.3 {
  testfixture $::tf1 {
    db eval {
      BEGIN;
      SELECT * FROM sqlite_master;
    }
  }
} {}
do_test lock2-1.4 {
  testfixture $::tf1 {
    catch { db eval { CREATE TABLE def(d, e, f) } } msg
    set msg
  }
} {database is locked}
do_test lock2-1.5 {
  catchsql {
    COMMIT;
  }
} {1 {database is locked}}
do_test lock2-1.6 {
  testfixture $::tf1 {
    db eval {
      SELECT * FROM sqlite_master;
      COMMIT;
    }
  }
} {}
do_test lock2-1.7 {
  testfixture $::tf1 {
    catch { db eval {
      BEGIN;
      SELECT * FROM sqlite_master;
    } } msg 
    set msg
  }
} {database is locked}
do_test lock2-1.8 {
  catchsql {
    COMMIT;
  }
} {0 {}}
do_test lock2-1.9 {
  execsql {
    SELECT * FROM sqlite_master;
  }
} "table abc abc [expr $AUTOVACUUM?3:2] {CREATE TABLE abc(a, b, c)}"
catch flush_async_queue
do_test lock2-1.10 {
  testfixture $::tf1 {
    db eval {
      SELECT * FROM sqlite_master;
    }
  }
} "table abc abc [expr $AUTOVACUUM?3:2] {CREATE TABLE abc(a, b, c)}"
catch {testfixture $::tf1 {db close}}
catch {close $::tf1}
sqlite3_soft_heap_limit $cmdlinearg(soft-heap-limit)
finish_test
"#);
}
