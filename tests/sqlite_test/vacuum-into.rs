#[test]
#[ignore = "1/16 subtests passed; pending: C testfixture hooks"]
fn test_vacuum_into() {
    sqliters_tcltest::run_test_script(
        "vacuum-into",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable {!vacuum} {
  omit_test vacuum.test {Compiled with SQLITE_OMIT_VACUUM}
  finish_test
  return
}
forcedelete out.db
do_execsql_test vacuum-into-100 {
  CREATE TABLE t1(
    a INTEGER PRIMARY KEY,
    b ANY,
    c INT AS (b+1),                          --- See "2024-04-09" block
    CHECK( typeof(b)!='integer' OR b>a-5 )   --- comment below
  );
  WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<100)
  INSERT INTO t1(a,b) SELECT x, randomblob(600) FROM c;
  CREATE INDEX t1b ON t1(b);
  DELETE FROM t1 WHERE a%2;
  SELECT count(*), sum(a), sum(length(b)) FROM t1;
} {50 2550 30000}
do_execsql_test vacuum-into-110 {
  VACUUM main INTO 'out.db';
} {}
sqlite3 db2 out.db
do_test vacuum-into-120 {
  db2 eval {SELECT count(*), sum(a), sum(length(b)) FROM t1}
} {50 2550 30000}
do_catchsql_test vacuum-into-130 {
  VACUUM INTO 'out.db';
} {1 {output file already exists}}
forcedelete out2.db
do_catchsql_test vacuum-into-140 {
  VACUUM INTO 'out2.db';
} {0 {}}
do_catchsql_test vacuum-into-150 {
  VACUUM INTO 'out2.db';
} {1 {output file already exists}}
do_catchsql_test vacuum-into-200 {
  VACUUM main INTO ':memory:';
} {0 {}}
do_execsql_test vacuum-into-300 {
  CREATE TABLE t2(name TEXT);
  INSERT INTO t2 VALUES(':memory:');
  VACUUM main INTO (SELECT name FROM t2);
} {}
do_catchsql_test vacuum-into-310 {
  VACUUM INTO null;
} {1 {non-text filename}}
do_catchsql_test vacuum-into-320 {
  VACUUM INTO x;
} {1 {no such column: x}}
do_catchsql_test vacuum-into-330 {
  VACUUM INTO t1.nosuchcol;
} {1 {no such column: t1.nosuchcol}}
do_catchsql_test vacuum-into-340 {
  VACUUM INTO main.t1.nosuchcol;
} {1 {no such column: main.t1.nosuchcol}}
forcedelete test.db2
db func target target
proc target {} { return "test.db2" }
do_test vacuum-into-410 {
  execsql { VACUUM INTO target() }
  file exists test.db2
} 1
do_catchsql_test vacuum-into-420 {
  VACUUM INTO target2()
} {1 {no such function: target2}}
db close
if {$tcl_platform(platform) eq "windows"} {
  file attributes test.db -readonly 1
} else {
  file attributes test.db -permissions 292  ;# 292 == 0444
}
sqlite3 db test.db -readonly 1
forcedelete test.db2
do_execsql_test vacuum-into-500 {
  VACUUM INTO 'test.db2';
}
if {$tcl_platform(platform) eq "windows"} {
  file attributes test.db -readonly 0
} else {
  file attributes test.db -permissions 420   ;# 420 = 0644
}
sqlite3 db2 test.db2
do_test vacuum-into-510 {
  db2 eval {SELECT name FROM sqlite_master ORDER BY 1}
} {t1 t1b t2}
db2 close
db close
if {[wal_is_capable]} {
  forcedelete test.db
  forcedelete test.db2
  do_test vacuum-into-600 {
    sqlite3 db test.db
    db eval {
      PRAGMA page_size=4096;
      PRAGMA journal_mode=WAL;
      CREATE TABLE t1(a);
      INSERT INTO t1 VALUES(19);
      CREATE INDEX t1a ON t1(a);
      PRAGMA integrity_check;
    }
  } {wal ok}
  do_execsql_test vacuum-into-610 {
    PRAGMA page_size;
  } {4096}
  do_execsql_test vacuum-into-620 {
    PRAGMA page_size=1024;
    VACUUM INTO 'test.db2';
  } {}
  do_test vacuum-into-630 {
    sqlite3 db test.db2
    db eval {
      PRAGMA page_size;
      PRAGMA integrity_check;
    }
  } {1024 ok}
}
testvfs tvfs -default 1
tvfs filter xSync
tvfs script xSyncCb
proc xSyncCb {method file fileid flags} {
  incr ::sync($flags)
}
reset_db
do_execsql_test vacuum-into-700 {
  CREATE TABLE t1(a, b);
  INSERT INTO t1 VALUES(1, 2);
}
foreach {tn pragma res} {
  710 {
    PRAGMA synchronous = normal
  } {normal 2}
  720 {
    PRAGMA synchronous = full
  } {normal 3}
  730 {
    PRAGMA synchronous = off
  } {}
  740 {
    PRAGMA synchronous = extra;
  } {normal 3}
  750 {
    PRAGMA fullfsync = 1;
    PRAGMA synchronous = full;
  } {full|dataonly 1 full 2}
} {

  forcedelete test.db2
  array unset ::sync
  do_execsql_test vacuum-into-$tn.1 "
    $pragma ;
    VACUUM INTO 'test.db2'
  "

  do_test vacuum-into-$tn.2 {
    array get ::sync
  } $res
}
db close
tvfs delete
finish_test
"#,
    );
}
