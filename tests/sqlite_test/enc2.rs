#[test]
#[ignore = "59/89 subtests passed; pending: ATTACH, TEMP TABLE"]
fn test_enc2() {
    sqliters_tcltest::run_test_script(
        "enc2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable {!utf16} {
  finish_test
  return
}
db close
proc utf8 {str} {
  binary scan $str \c* vals
  if {[lindex $vals end]==0 && [lindex $vals end-1]==0} {
    set str [binary format \c* [lrange $vals 0 end-2]]
  }

  set r [encoding convertfrom unicode $str]
  return $r
}
set dbcontents {
  CREATE TABLE t1(a PRIMARY KEY, b, c);
  INSERT INTO t1 VALUES('one', 'I', 1);
}
proc run_test_script {t enc} {

do_test $t.1 {
  sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
  execsql {SELECT * FROM t1}
} {one I 1}

do_test $t.2 {
  execsql {INSERT INTO t1 VALUES('two', 'II', 2);}
  execsql {SELECT * FROM t1}
} {one I 1 two II 2}

do_test $t.3 {
  execsql {
    INSERT INTO t1 VALUES('three','III',3);
    INSERT INTO t1 VALUES('four','IV',4);
    INSERT INTO t1 VALUES('five','V',5);
  }
  execsql {SELECT * FROM t1}
} {one I 1 two II 2 three III 3 four IV 4 five V 5}

do_test $t.4 {
  execsql {
    SELECT * FROM t1 WHERE a = 'one';
  }
} {one I 1}
do_test $t.5 {
  execsql {
    SELECT * FROM t1 WHERE a = 'four';
  }
} {four IV 4}
ifcapable subquery {
  do_test $t.6 {
    execsql {
      SELECT * FROM t1 WHERE a IN ('one', 'two');
    }
  } {one I 1 two II 2}
}

do_test $t.7 {
  set STMT [sqlite3_prepare $DB "SELECT a FROM t1 WHERE c>3;" -1 TAIL]
  sqlite3_step $STMT
  sqlite3_column_text $STMT 0
} {four}

do_test $t.8 {
  sqlite3_step $STMT
  utf8 [sqlite3_column_text16 $STMT 0]
} {five}

do_test $t.9 {
  sqlite3_finalize $STMT
} SQLITE_OK

ifcapable vacuum {
  execsql VACUUM
}

do_test $t.10 {
  db eval {PRAGMA encoding}
} $enc

}
set encodings [list UTF-8 UTF-16le UTF-16be]
set sqlite_os_trace 0
set i 1
foreach enc $encodings {
  forcedelete test.db
  sqlite3 db test.db
  db eval "PRAGMA encoding = \"$enc\""
  execsql $dbcontents
  do_test enc2-$i.0.1 {
    db eval {PRAGMA encoding}
  } $enc
  do_test enc2-$i.0.2 {
    db eval {PRAGMA encoding=UTF8}
    db eval {PRAGMA encoding}
  } $enc
  do_test enc2-$i.0.3 {
    db eval {PRAGMA encoding=UTF16le}
    db eval {PRAGMA encoding}
  } $enc
  do_test enc2-$i.0.4 {
    db eval {PRAGMA encoding=UTF16be}
    db eval {PRAGMA encoding}
  } $enc

  db close
  run_test_script enc2-$i $enc
  db close
  incr i
}
ifcapable attach {
  do_test enc2-4.1 {
    forcedelete test.db
    sqlite3 db test.db
    db eval "PRAGMA encoding = 'UTF-8'"
    db eval "CREATE TABLE abc(a, b, c);"
  } {}
  do_test enc2-4.2 {
    forcedelete test2.db
    sqlite3 db2 test2.db
    db2 eval "PRAGMA encoding = 'UTF-16'"
    db2 eval "CREATE TABLE abc(a, b, c);"
  } {}
  do_test enc2-4.3 {
    catchsql {
      ATTACH 'test2.db' as aux;
    }
  } {1 {attached databases must use the same text encoding as main database}}
  db2 close
  db close
}
set ::values [list one two three four five]
set ::test_collate_enc INVALID
proc test_collate {enc lhs rhs} {
  set ::test_collate_enc $enc
  set l [lsearch -exact $::values $lhs]
  set r [lsearch -exact $::values $rhs]
  set res [expr $l - $r]
  # puts "enc=$enc lhs=$lhs/$l rhs=$rhs/$r res=$res"
  return $res
}
forcedelete test.db
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
do_test enc2-5.0 {
  execsql {
    CREATE TABLE t5(a);
    INSERT INTO t5 VALUES('one');
    INSERT INTO t5 VALUES('two');
    INSERT INTO t5 VALUES('five');
    INSERT INTO t5 VALUES('three');
    INSERT INTO t5 VALUES('four');
  }
} {}
do_test enc2-5.1 {
  add_test_collate $DB 1 1 1
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate;}]
  lappend res $::test_collate_enc
} {one two three four five UTF-8}
do_test enc2-5.2 {
  add_test_collate $DB 0 1 0
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate}]
  lappend res $::test_collate_enc
} {one two three four five UTF-16LE}
do_test enc2-5.3 {
  add_test_collate $DB 0 0 1
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate}]
  lappend res $::test_collate_enc
} {one two three four five UTF-16BE}
db close
forcedelete test.db
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
execsql {pragma encoding = 'UTF-16LE'}
do_test enc2-5.4 {
  execsql {
    CREATE TABLE t5(a);
    INSERT INTO t5 VALUES('one');
    INSERT INTO t5 VALUES('two');
    INSERT INTO t5 VALUES('five');
    INSERT INTO t5 VALUES('three');
    INSERT INTO t5 VALUES('four');
  }
} {}
do_test enc2-5.5 {
  add_test_collate $DB 1 1 1
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate}]
  lappend res $::test_collate_enc
} {one two three four five UTF-16LE}
do_test enc2-5.6 {
  add_test_collate $DB 1 0 1
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate}]
  lappend res $::test_collate_enc
} {one two three four five UTF-16BE}
do_test enc2-5.7 {
  add_test_collate $DB 1 0 0
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate}]
  lappend res $::test_collate_enc
} {one two three four five UTF-8}
db close
forcedelete test.db
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
execsql {pragma encoding = 'UTF-16BE'}
do_test enc2-5.8 {
  execsql {
    CREATE TABLE t5(a);
    INSERT INTO t5 VALUES('one');
    INSERT INTO t5 VALUES('two');
    INSERT INTO t5 VALUES('five');
    INSERT INTO t5 VALUES('three');
    INSERT INTO t5 VALUES('four');
  }
} {}
do_test enc2-5.9 {
  add_test_collate $DB 1 1 1
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate}]
  lappend res $::test_collate_enc
} {one two three four five UTF-16BE}
do_test enc2-5.10 {
  add_test_collate $DB 1 1 0
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate}]
  lappend res $::test_collate_enc
} {one two three four five UTF-16LE}
do_test enc2-5.11 {
  add_test_collate $DB 1 0 0
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate}]
  lappend res $::test_collate_enc
} {one two three four five UTF-8}
do_test enc2-5-12 {
  add_test_collate $DB 0 0 0
  catchsql {
    SELECT * FROM t5 ORDER BY 1 COLLATE test_collate
  }
} {1 {no such collation sequence: test_collate}}
do_test enc2-5.13 {
  add_test_collate_needed $DB 
  set res [execsql {SELECT * FROM t5 ORDER BY 1 COLLATE test_collate; }]
  lappend res $::test_collate_enc
} {one two three four five UTF-16BE}
do_test enc2-5.14 {
  set ::sqlite_last_needed_collation
} test_collate
db close
forcedelete test.db
do_test enc2-5.15 {
  sqlite3 db test.db; set ::DB [sqlite3_connection_pointer db]
  add_test_collate_needed $::DB
  set ::sqlite_last_needed_collation
} {}
do_test enc2-5.16 {
  execsql {CREATE TABLE t1(a varchar collate test_collate);}
} {}
do_test enc2-5.17 {
  set ::sqlite_last_needed_collation
} {test_collate}
proc test_function {enc arg} {
  return "$enc $arg"
}
db close
forcedelete test.db
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
execsql {pragma encoding = 'UTF-8'}
do_test enc2-6.0 {
  execsql {
    CREATE TABLE t5(a);
    INSERT INTO t5 VALUES('one');
  }
} {}
do_test enc2-6.1 {
  add_test_function $DB 1 1 1
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-8 sqlite}}
db close
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
do_test enc2-6.2 {
  add_test_function $DB 0 1 0
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-16LE sqlite}}
db close
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
do_test enc2-6.3 {
  add_test_function $DB 0 0 1
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-16BE sqlite}}
db close
forcedelete test.db
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
execsql {pragma encoding = 'UTF-16LE'}
do_test enc2-6.3 {
  execsql {
    CREATE TABLE t5(a);
    INSERT INTO t5 VALUES('sqlite');
  }
} {}
do_test enc2-6.4 {
  add_test_function $DB 1 1 1
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-16LE sqlite}}
db close
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
do_test enc2-6.5 {
  add_test_function $DB 0 1 0
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-16LE sqlite}}
db close
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
do_test enc2-6.6 {
  add_test_function $DB 0 0 1
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-16BE sqlite}}
db close
forcedelete test.db
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
execsql {pragma encoding = 'UTF-16BE'}
do_test enc2-6.7 {
  execsql {
    CREATE TABLE t5(a);
    INSERT INTO t5 VALUES('sqlite');
  }
} {}
do_test enc2-6.8 {
  add_test_function $DB 1 1 1
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-16BE sqlite}}
db close
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
do_test enc2-6.9 {
  add_test_function $DB 0 1 0
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-16LE sqlite}}
db close
sqlite3 db test.db; set DB [sqlite3_connection_pointer db]
do_test enc2-6.10 {
  add_test_function $DB 0 0 1
  execsql {
    SELECT test_function('sqlite')
  }
} {{UTF-16BE sqlite}}
db close
forcedelete test.db
do_test enc2-7.1 {
  sqlite3 db test.db
  execsql {
    PRAGMA encoding = 'UTF-16';
    SELECT * FROM sqlite_master;
  }
} {}
do_test enc2-7.2 {
  set enc [execsql {
    PRAGMA encoding;
  }]
  string range $enc 0 end-2 ;# Chop off the "le" or "be"
} {UTF-16}
do_test enc2-7.3 {
  sqlite3 db2 test.db
  execsql {
    PRAGMA encoding = 'UTF-8';
    CREATE TABLE abc(a, b, c);
  } db2
} {}
do_test enc2-7.4 {
  execsql {
    SELECT * FROM sqlite_master;
  }
} "table abc abc [expr $AUTOVACUUM?3:2] {CREATE TABLE abc(a, b, c)}"
do_test enc2-7.5 {
  execsql {
    PRAGMA encoding;
  }
} {UTF-8}
db close
db2 close
proc utf16 {utf8} {
  set utf16 [encoding convertto unicode $utf8]
  append utf16 "\x00\x00"
  return $utf16
}
ifcapable {complete} {
  do_test enc2-8.1 {
    sqlite3_complete16 [utf16 "SELECT * FROM t1;"]
  } {1}
  do_test enc2-8.2 {
    sqlite3_complete16 [utf16 "SELECT * FROM"]
  } {0}
}
forcedelete test.db
do_test enc2-9.1 {
  sqlite3 db test.db
  execsql {
    PRAGMA encoding = 'UTF-8';
    PRAGMA encoding;
  }
} {UTF-8}
do_test enc2-9.2 {
  sqlite3 db test.db
  execsql {
    PRAGMA encoding = 'UTF-16le';
    PRAGMA encoding;
  }
} {UTF-16le}
do_test enc2-9.3 {
  sqlite3 db test.db
  execsql {
    SELECT * FROM sqlite_master;
    PRAGMA encoding = 'UTF-8';
    PRAGMA encoding;
  }
} {UTF-8}
do_test enc2-9.4 {
  sqlite3 db test.db
  execsql {
    PRAGMA encoding = 'UTF-16le';
    CREATE TABLE abc(a, b, c);
    PRAGMA encoding;
  }
} {UTF-16le}
do_test enc2-9.5 {
  sqlite3 db test.db
  execsql {
    PRAGMA encoding = 'UTF-8';
    PRAGMA encoding;
  }
} {UTF-16le}
do_test enc2-10.1 {
  db close
  forcedelete test.db test.db-journal
  sqlite3 db test.db
  db eval {
    PRAGMA encoding=UTF16;
    CREATE TABLE t1(a);
    PRAGMA encoding=UTF8;
    CREATE TABLE t2(b);
  }
  db close
  sqlite3 db test.db
  db eval {
    SELECT name FROM sqlite_master
  }
} {t1 t2}
reset_db
do_execsql_test enc2-11.10 {
  PRAGMA encoding=UTF8;
  CREATE TEMP TABLE t1(x);
  INSERT INTO t1 VALUES('this is a test');
  PRAGMA encoding=UTF16;
  SELECT * FROM t1;
} {{this is a test}}
finish_test
"#,
    );
}
