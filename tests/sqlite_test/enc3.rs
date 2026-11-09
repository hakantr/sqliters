#![forbid(unsafe_code)]

#[test]
#[ignore = "4/11 subtests passed; pending: UTF-16, shared_cache"]
fn test_enc3() {
    sqliters_tcltest::run_test_script(
        "enc3",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable {utf16} {
  do_test enc3-1.1 {
    execsql {
      PRAGMA encoding=utf16le;
      PRAGMA encoding;
    }
  } {UTF-16le}
}
do_test enc3-1.2 {
  execsql {
    CREATE TABLE t1(x,y);
    INSERT INTO t1 VALUES('abc''123',5);
    SELECT * FROM t1
  }
} {abc'123 5}
do_test enc3-1.3 {
  execsql {
    SELECT quote(x) || ' ' || quote(y) FROM t1
  }
} {{'abc''123' 5}}
ifcapable {bloblit} {
  do_test enc3-1.4 {
    execsql {
      DELETE FROM t1;
      INSERT INTO t1 VALUES(x'616263646566',NULL);
      SELECT * FROM t1
    }
  } {abcdef {}}
  do_test enc3-1.5 {
    execsql {
      SELECT quote(x) || ' ' || quote(y) FROM t1
    }
  } {{X'616263646566' NULL}}
}
ifcapable {bloblit && utf16} {
  do_test enc3-2.1 {
    execsql {
      PRAGMA encoding
    }
  } {UTF-16le}
  do_test enc3-2.2 {
    execsql {
      CREATE TABLE t2(a);
      INSERT INTO t2 VALUES(x'61006200630064006500');
      SELECT CAST(a AS text) FROM t2 WHERE CAST(a AS text) LIKE 'abc%';
    }
  } {abcde}
  do_test enc3-2.3 {
    execsql {
      SELECT CAST(x'61006200630064006500' AS text);
    }
  } {abcde}
  do_test enc3-2.4 {
    execsql {
      SELECT rowid FROM t2
       WHERE CAST(a AS text) LIKE CAST(x'610062002500' AS text);
    }
  } {1}
}
ifcapable {utf16 && shared_cache} {
  db close
  forcedelete test8.db test8.db-journal
  set ::enable_shared_cache [sqlite3_enable_shared_cache 1]
  sqlite3 dbaux test8.db
  sqlite3 db test.db
  db eval {SELECT 1 FROM sqlite_master LIMIT 1}
  do_test enc3-3.1 {
    dbaux eval {
      PRAGMA encoding='utf8';
      CREATE TABLE t1(x);
      PRAGMA encoding
    }
  } {UTF-8}
  do_test enc3-3.2 {
    catchsql {
      ATTACH 'test.db' AS utf16;
      SELECT 1 FROM utf16.sqlite_master LIMIT 1;
    } dbaux
  } {1 {attached databases must use the same text encoding as main database}}
  dbaux close
  forcedelete test8.db test8.db-journal
  sqlite3_enable_shared_cache $::enable_shared_cache
}
finish_test
"#,
    );
}
