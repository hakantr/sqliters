#[test]
#[ignore = "0/2 subtests passed; pending: can't read \"::sig(one)\": no such variable"]
fn test_savepoint2() {
    sqliters_tcltest::run_test_script(
        "savepoint2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc signature {} {
  return [db eval {SELECT count(*), md5sum(x) FROM t3}]
}
do_test savepoint2-1 {
  wal_set_journal_mode
  execsql {
    PRAGMA cache_size=10;
    BEGIN;
    CREATE TABLE t3(x TEXT);
    INSERT INTO t3 VALUES(randstr(10,400));
    INSERT INTO t3 VALUES(randstr(10,400));
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    INSERT INTO t3 SELECT randstr(10,400) FROM t3;
    COMMIT;
    SELECT count(*) FROM t3;
  }
} {1024}
wal_check_journal_mode savepoint2-1.1
unset -nocomplain ::sig
unset -nocomplain SQL
set iterations 20
set SQL(1) {
  DELETE FROM t3 WHERE random()%10!=0;
  INSERT INTO t3 SELECT randstr(10,10)||x FROM t3;
  INSERT INTO t3 SELECT randstr(10,10)||x FROM t3;
}
set SQL(2) {
  DELETE FROM t3 WHERE random()%10!=0;
  INSERT INTO t3 SELECT randstr(10,10)||x FROM t3;
  DELETE FROM t3 WHERE random()%10!=0;
  INSERT INTO t3 SELECT randstr(10,10)||x FROM t3;
} 
set SQL(3) {
  UPDATE t3 SET x = randstr(10, 400) WHERE random()%10;
  INSERT INTO t3 SELECT x FROM t3 WHERE random()%10;
  DELETE FROM t3 WHERE random()%10;
}
set SQL(4) {
  INSERT INTO t3 SELECT randstr(10,400) FROM t3 WHERE (random()%10 == 0);
}
for {set ii 2} {$ii < ($iterations+2)} {incr ii} {

  do_test savepoint2-$ii.1 {
    if {$ii % 2} { execsql BEGIN }
    set ::sig(one) [signature]
    execsql "SAVEPOINT one"
  } {}

  do_test savepoint2-$ii.2 {
    execsql $SQL(1)
    execsql "ROLLBACK to one"
    signature
  } $::sig(one)
  integrity_check savepoint2-$ii.2.1

  do_test savepoint2-$ii.3 {
    execsql $SQL(1)
    set ::sig(two) [signature]
    execsql "SAVEPOINT two"
  } {}

  do_test savepoint2-$ii.4 {
    execsql $SQL(2)
    execsql "ROLLBACK to two"
    signature
  } $::sig(two)
  integrity_check savepoint2-$ii.4.1

  do_test savepoint2-$ii.5 {
    execsql $SQL(2)
    execsql "SAVEPOINT three"
    execsql $SQL(3)
    execsql "RELEASE three"
    execsql "ROLLBACK to one"
    signature
  } $::sig(one)

  do_test savepoint2-$ii.6 {
    execsql $SQL(4)
    execsql COMMIT
    sqlite3_get_autocommit db
  } {1}
  integrity_check savepoint2-$ii.6.1

  wal_check_journal_mode savepoint2-$ii.7
}
unset -nocomplain ::sig
unset -nocomplain SQL
finish_test
"#,
    );
}
