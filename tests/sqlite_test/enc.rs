#[test]
#[ignore = "0/0 subtests passed; pending: ATTACH, VIRTUAL TABLE, TEMP TABLE"]
fn test_enc() {
    sqliters_tcltest::run_test_script(
        "enc",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable {!utf16} {
  finish_test
  return
}
proc do_bincmp_test {testname got expect} {
  binary scan $expect \c* expectvals
  binary scan $got \c* gotvals
  do_test $testname [list set dummy $gotvals] $expectvals
}
proc swap_byte_order {utf16} {
  binary scan $utf16 \c* ints

  foreach {a b} $ints {
    lappend ints2 $b
    lappend ints2 $a
  }

  return [binary format \c* $ints2]
}
proc test_conversion {testname str} {
 
  set utf16le_sqlite3 [test_translate $str UTF8 UTF16LE]
  set utf16le_tcl [encoding convertto unicode $str]
  append utf16le_tcl "\x00\x00"
  if { $::tcl_platform(byteOrder)!="littleEndian" } {
    set utf16le_tcl [swap_byte_order $utf16le_tcl]
  }
  do_bincmp_test $testname.1 $utf16le_sqlite3 $utf16le_tcl
  set utf16le $utf16le_tcl

  set utf16be_sqlite3 [test_translate $str UTF8 UTF16BE]
  set utf16be_tcl [encoding convertto unicode $str]
  append utf16be_tcl "\x00\x00"
  if { $::tcl_platform(byteOrder)=="littleEndian" } {
    set utf16be_tcl [swap_byte_order $utf16be_tcl]
  }
  do_bincmp_test $testname.2 $utf16be_sqlite3 $utf16be_tcl
  set utf16be $utf16be_tcl
 
  if { $::tcl_platform(byteOrder)=="littleEndian" } {
    set utf16 $utf16le
  } else {
    set utf16 $utf16be
  }
  set utf8_sqlite3 [test_translate $utf16 UTF16 UTF8]
  do_bincmp_test $testname.3 $utf8_sqlite3 [binarize $str]

  append utf16le_bom "\xFF\xFE" $utf16le
  set utf8_sqlite3 [test_translate $utf16le_bom UTF16 UTF8 1]
  do_bincmp_test $testname.4.le $utf8_sqlite3 [binarize $str]

  append utf16be_bom "\xFE\xFF" $utf16be
  set utf8_sqlite3 [test_translate $utf16be_bom UTF16 UTF8]
  do_bincmp_test $testname.4.be $utf8_sqlite3 [binarize $str]

  set utf16_sqlite3 [test_translate $utf16le_bom UTF16LE UTF16LE]
  do_bincmp_test $testname.5.le.le $utf16_sqlite3 $utf16le

  set utf16_sqlite3 [test_translate $utf16be_bom UTF16 UTF16BE]
  do_bincmp_test $testname.5.be.be $utf16_sqlite3 $utf16be

  set utf16_sqlite3 [test_translate $utf16be_bom UTF16 UTF16LE]
  do_bincmp_test $testname.5.be.le $utf16_sqlite3 $utf16le

  set utf16_sqlite3 [test_translate $utf16le_bom UTF16 UTF16BE]
  do_bincmp_test $testname.5.le.be $utf16_sqlite3 $utf16be
}
translate_selftest
test_conversion enc-1 "hello world"
test_conversion enc-2 "sqlite"
test_conversion enc-3 ""
test_conversion enc-X "\u0100"
test_conversion enc-4 "\u1234"
test_conversion enc-5 "\u4321abc"
test_conversion enc-6 "\u4321\u1234"
test_conversion enc-7 [string repeat "abcde\u00EF\u00EE\uFFFCabc" 100]
test_conversion enc-8 [string repeat "\u007E\u007F\u0080\u0081" 100]
test_conversion enc-9 [string repeat "\u07FE\u07FF\u0800\u0801\uFFF0" 100]
test_conversion enc-10 [string repeat "\uE000" 100]
proc test_collate {enc zLeft zRight} {
  return [string compare $zLeft $zRight]
}
add_test_collate $::DB 0 0 1
do_test enc-11.1 {
  execsql {
    CREATE TABLE ab(a COLLATE test_collate, b);
    INSERT INTO ab VALUES(CAST (X'C388' AS TEXT), X'888800');
    INSERT INTO ab VALUES(CAST (X'C0808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808388' AS TEXT), X'888800');
    CREATE INDEX ab_i ON ab(a, b);
  }
} {}
do_test enc-11.2 {
  set cp200 "\u00C8"
  execsql {
    SELECT count(*) FROM ab WHERE a = $::cp200;
  }
} {2}
reset_db
forcedelete test.db2
forcedelete test.db3
do_execsql_test enc-12.0 {
  PRAGMA encoding = 'utf-8';
  CREATE TABLE t1(a, b, c);
  INSERT INTO t1 VALUES('a', 'b', 'c');
  ATTACH 'test.db3' AS aux;
  CREATE TABLE aux.t3(x, y, z);
  INSERT INTO t3 VALUES('xxx', 'yyy', 'zzz');
  PRAGMA encoding;
} {UTF-8}
do_test enc-12.1 {
  sqlite3 db2 test.db2
  db2 eval {
    PRAGMA encoding = 'UTF-16le';
    CREATE TABLE t2(d, e, f);
    INSERT INTO t2 VALUES('d', 'e', 'f');
    PRAGMA encoding;
  }
} {UTF-16le}
do_test enc-12.2 {
  db2 backup test.db
  db2 close
} {}
do_catchsql_test enc-12.3 {
  SELECT * FROM t2;
} {1 {attached databases must use the same text encoding as main database}}
db close
sqlite3 db test.db3
do_execsql_test enc-12.4 {
  SELECT * FROM t3;
  PRAGMA encoding = 'UTF-16le';
  SELECT * FROM t3;
} {xxx yyy zzz xxx yyy zzz}
db close
sqlite3 db test.db3
breakpoint
do_execsql_test enc-12.5 {
  PRAGMA encoding = 'UTF-16le';
  PRAGMA encoding;
} {UTF-8}
reset_db
do_execsql_test enc-12.6 {
  PRAGMA encoding = 'UTF-8';
  CREATE TEMP TABLE t1(a, b, c);
  INSERT INTO t1 VALUES('xxx', 'yyy', 'zzz');
}
do_test enc-12.7 {
  sqlite3 db2 test.db2
  db2 backup test.db
  db2 close
  db eval {
    SELECT * FROM t1;
  }
} {xxx yyy zzz}
do_catchsql_test enc-12.8 {
  SELECT * FROM t2;
  SELECT * FROM t1;
} {1 {attached databases must use the same text encoding as main database}}
db close
sqlite3 db test.db
do_execsql_test enc-12.9 {
  CREATE TEMP TABLE t1(a, b, c);
  INSERT INTO t1 VALUES('xxx', 'yyy', 'zzz');
}
do_execsql_test enc-12.10 {
  SELECT * FROM t2;
  SELECT * FROM t1;
} {d e f xxx yyy zzz}
db close
forcedelete utf16.db
sqlite3 db utf16.db
db eval {PRAGMA encoding=UTF16; CREATE TABLE t2(y); INSERT INTO t2 VALUES('utf16');}
db close
sqlite3 db utf16.db
do_test enc-13.1 {
  db eval {PRAGMA function_list} {db eval {SELECT * FROM sqlite_schema}}
} {}
ifcapable rtree {
  db eval {CREATE VIRTUAL TABLE t3 USING rtree(id,x1,x2)}
  db close
  sqlite3 db utf16.db
  do_execsql_test enc-13.2 {
    WITH t1(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM t1 WHERE x<3)
    SELECT rtreecheck('t3') FROM t1;
  } {ok ok ok}
}
do_execsql_test enc-14.1 {
  CREATE TABLE t14(n, c);
  INSERT INTO t14 VALUES
     (1000, CAST(printf('%.*c', 1000, 'a') AS BLOB)),
     (5000, CAST(printf('%.*c', 5000, 'a') AS BLOB));
  SELECT n, length(c), length(CAST(c AS TEXT))
    FROM t14 ORDER BY +n;
} {1000 2000 1000 5000 10000 5000}
finish_test
"#,
    );
}
