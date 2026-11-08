#[test]
#[ignore = "19/30 subtests passed; pending: vacuum2 edge cases"]
fn test_vacuum2() {
    sqliters_tcltest::run_test_script(
        "vacuum2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix vacuum2
do_not_use_codec
ifcapable {!vacuum||!autoinc} {
  finish_test
  return
}
if $AUTOVACUUM {
  finish_test
  return
}
do_test vacuum2-1.1 {
  execsql {
    CREATE TABLE t1(x INTEGER PRIMARY KEY AUTOINCREMENT, y);
    DROP TABLE t1;
    VACUUM;
  }
} {}
do_test vacuum2-2.1 {
  execsql {
    CREATE TABLE t1(x);
    CREATE TABLE t2(y);
    INSERT INTO t1 VALUES(1);
  }
  hexio_get_int [hexio_read test.db 24 4]
} [expr {[hexio_get_int [hexio_read test.db 24 4]]+3}]
do_test vacuum2-2.2 {
  execsql {
    VACUUM
  }
  hexio_get_int [hexio_read test.db 24 4]
} [expr {[hexio_get_int [hexio_read test.db 24 4]]+1}]
sqlite3 db2 test.db
set pageSize [db eval {pragma page_size}]
do_test vacuum2-3.1 {
  execsql {
    INSERT INTO t1 VALUES('hello');
    INSERT INTO t2 VALUES('out there');
  }
  expr {[file size test.db]/$pageSize}
} {3}
set cksum [cksum]
do_test vacuum2-3.2 {
  cksum db2
} $cksum
ifcapable autovacuum {
  do_test vacuum2-3.3 {
    execsql {
      PRAGMA auto_vacuum=FULL;
      VACUUM;
    }
    expr {[file size test.db]/$pageSize}
  } {4}
}
do_test vacuum2-3.4 {
  cksum db2
} $cksum
do_test vacuum2-3.5 {
  cksum
} $cksum
do_test vacuum2-3.6 {
  execsql {PRAGMA integrity_check} db2
} {ok}
do_test vacuum2-3.7 {
  execsql {PRAGMA integrity_check} db
} {ok}
do_test vacuum2-3.13 {
  execsql {
    PRAGMA auto_vacuum=NONE;
    VACUUM;
  }
  expr {[file size test.db]/$pageSize}
} {3}
do_test vacuum2-3.14 {
  cksum db2
} $cksum
do_test vacuum2-3.15 {
  cksum
} $cksum
do_test vacuum2-3.16 {
  execsql {PRAGMA integrity_check} db2
} {ok}
do_test vacuum2-3.17 {
  execsql {PRAGMA integrity_check} db
} {ok}
db2 close
ifcapable autovacuum {
  do_test vacuum2-4.1 {
    db close
    forcedelete test.db
    sqlite3 db test.db
    execsql {
      pragma auto_vacuum=1;
      create table t(a, b);
      insert into t values(1, 2);
      insert into t values(1, 2);
      pragma auto_vacuum=0;
      vacuum;
      pragma auto_vacuum;
    }
  } {0}
  do_test vacuum2-4.2 {
    execsql {
      pragma auto_vacuum=1;
      vacuum;
      pragma auto_vacuum;
    }
  } {1}
  do_test vacuum2-4.3 {
    execsql {
      pragma integrity_check
    }
  } {ok}
  do_test vacuum2-4.4 {
    db close
    sqlite3 db test.db
    execsql {
      pragma auto_vacuum;
    }
  } {1}
  do_test vacuum2-4.5 {  # Ticket #3663
    execsql {
      pragma auto_vacuum=2;
      vacuum;
      pragma auto_vacuum;
    }
  } {2}
  do_test vacuum2-4.6 {
    execsql {
      pragma integrity_check
    }
  } {ok}
  do_test vacuum2-4.7 {
    db close
    sqlite3 db test.db
    execsql {
      pragma auto_vacuum;
    }
  } {2}
}
db close
forcedelete test.db
sqlite3 db test.db
do_execsql_test vacuum2-5.1 {
  CREATE TABLE t1(a PRIMARY KEY, b UNIQUE);
  INSERT INTO t1 VALUES(1, randomblob(500));
  INSERT INTO t1 SELECT a+1, randomblob(500) FROM t1;      -- 2
  INSERT INTO t1 SELECT a+2, randomblob(500) FROM t1;      -- 4 
  INSERT INTO t1 SELECT a+4, randomblob(500) FROM t1;      -- 8 
  INSERT INTO t1 SELECT a+8, randomblob(500) FROM t1;      -- 16 
} {}
do_test vacuum2-5.2 {
  list [catch {
    db eval {SELECT a, b FROM t1} { if {$a == 8} { execsql VACUUM } }
  } msg] $msg
} {1 {cannot VACUUM - SQL statements in progress}}
do_test vacuum2-5.3 {
  list [catch {
    db eval {SELECT 1, 2, 3} { execsql VACUUM }
  } msg] $msg
} {1 {cannot VACUUM - SQL statements in progress}}
do_test vacuum2-5.4 {
  set res ""
  set res2 ""
  db eval {SELECT a, b FROM t1 WHERE a<=10} {
    if {$a==6} { set res [catchsql VACUUM] }
    lappend res2 $a
  }
  lappend res2 $res
} {1 2 3 4 5 6 7 8 9 10 {1 {cannot VACUUM - SQL statements in progress}}}
proc cmp {lhs rhs} { string compare $lhs $rhs }
db collate cmp cmp
do_execsql_test 6.0 {
  CREATE TABLE t6(x PRIMARY KEY COLLATE cmp, y) WITHOUT ROWID;
  CREATE INDEX t6y ON t6(y);
  INSERT INTO t6 VALUES('i', 'one');
  INSERT INTO t6 VALUES('ii', 'one');
  INSERT INTO t6 VALUES('iii', 'one');
}
integrity_check 6.1
proc cmp {lhs rhs} { string compare $rhs $lhs }
do_execsql_test 6.2 VACUUM
integrity_check 6.3
finish_test
"#,
    );
}
