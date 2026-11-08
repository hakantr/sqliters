#[test]
fn test_savepoint7() {
    sqliters_tcltest::run_test_script("savepoint7", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test savepoint7-1.1 {
  db eval {
    CREATE TABLE t1(a,b,c);
    CREATE TABLE t2(x,y,z);
    INSERT INTO t1 VALUES(1,2,3);
    INSERT INTO t1 VALUES(4,5,6);
    INSERT INTO t1 VALUES(7,8,9);
    SAVEPOINT x1;
  }
  db eval {SELECT * FROM t1} {
    db eval {
      SAVEPOINT x2;
      CREATE TABLE IF NOT EXISTS t3(xyz);
      INSERT INTO t2 VALUES($a,$b,$c);
      RELEASE x2;
    }
  }
  db eval {SELECT * FROM t2; RELEASE x1}
} {1 2 3 4 5 6 7 8 9}
do_test savepoint7-1.2 {
  db eval {DELETE FROM t2;}
  db eval {SELECT * FROM t1} {
    db eval {
      SAVEPOINT x2;
      INSERT INTO t2 VALUES($a,$b,$c);
      RELEASE x2;
    }
  }
  db eval {SELECT * FROM t2;}
} {1 2 3 4 5 6 7 8 9}
do_test savepoint7-1.3 {
  db eval {DELETE FROM t2; BEGIN;}
  db eval {SELECT * FROM t1} {
    db eval {
      SAVEPOINT x2;
      INSERT INTO t2 VALUES($a,$b,$c);
      RELEASE x2;
    }
  }
  db eval {SELECT * FROM t2; ROLLBACK;}
} {1 2 3 4 5 6 7 8 9}
do_test savepoint7-2.1 {
  db eval {DELETE FROM t2; SAVEPOINT x1; CREATE TABLE t4(abc);}
  set rc [catch {
    db eval {SELECT * FROM t1} {
      db eval {
        SAVEPOINT x2;
        INSERT INTO t2 VALUES($a,$b,$c);
        ROLLBACK TO x2;
      }
    }
  } msg]
  db eval {RELEASE x1}
  list $rc $msg [db eval {SELECT * FROM t2}]
} {1 {abort due to ROLLBACK} {}}
do_test savepoint7-2.2 {
  db eval {DELETE FROM t2;}
  set rc [catch {
    db eval {SELECT * FROM t1} {
      db eval {
        SAVEPOINT x2;
        CREATE TABLE t5(pqr);
        INSERT INTO t2 VALUES($a,$b,$c);
        ROLLBACK TO x2;
      }
    }
  } msg]
  list $rc $msg [db eval {SELECT * FROM t2}]
} {1 {abort due to ROLLBACK} {}}
unset -nocomplain i
for {set i 248} {$i<=253} {incr i} {
  do_test savepoint7-3.$i {
    db close
    forcedelete test.db
    sqlite3 db test.db
    db eval {
      PRAGMA page_size=1024;
      PRAGMA temp_store=MEMORY;
      BEGIN;
      CREATE TABLE t1(x INTEGER PRIMARY KEY, y TEXT);
      WITH RECURSIVE c(x) AS (VALUES(1) UNION SELECT x+1 FROM c WHERE x<$::i)
      INSERT INTO t1(x,y) SELECT x*10, printf('%04d%.800c',x,'*') FROM c;
      SAVEPOINT one;
        SELECT count(*) FROM t1;
        WITH RECURSIVE c(x) AS (VALUES(1) UNION SELECT x+1 FROM c WHERE x<$::i)
        INSERT INTO t1(x,y) SELECT x*10+1, printf('%04d%.800c',x,'*') FROM c;
      ROLLBACK TO one;
        SELECT count(*) FROM t1;
        SAVEPOINT twoB;
          WITH RECURSIVE c(x) AS (VALUES(1) UNION SELECT x+1 FROM c WHERE x<10)
          INSERT INTO t1(x,y) SELECT x*10+2, printf('%04d%.800c',x,'*') FROM c;
        ROLLBACK TO twoB;
      RELEASE one;
      COMMIT;
    }
  } [list $i $i]
}
finish_test
"#);
}
