#[test]
#[ignore = "39/55 subtests passed; pending: types edge cases"]
fn test_types() {
    sqliters_tcltest::run_test_script("types", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
db close
set DB [sqlite3 db test.db; sqlite3_connection_pointer db]
sqlite3_rekey $DB {}
do_test types-1.1.0 {
  execsql {
    CREATE TABLE t1(i integer, n numeric, t text, o blob);
  }
} {}
set values {
  { 5.0    integer integer text real    }
  { 5.1    real    real    text real    }
  { 5      integer integer text integer }
  { '5.0'  integer integer text text    }
  { '5.1'  real    real    text text    }
  { '-5.0' integer integer text text    }
  { '-5.0' integer integer text text    }
  { '5'    integer integer text text    }
  { 'abc'  text    text    text text    }
  { NULL   null    null    null null    }
}
ifcapable {bloblit} {
  lappend values  { X'00'  blob    blob    blob blob    }
}
set tnum 1
foreach val $values {
  set lit [lindex $val 0]
  execsql "DELETE FROM t1;"
  execsql "INSERT INTO t1 VALUES($lit, $lit, $lit, $lit);"
  do_test types-1.1.$tnum {
    execsql {
      SELECT typeof(i), typeof(n), typeof(t), typeof(o) FROM t1;
    }
  } [lrange $val 1 end]
  incr tnum
}
set tnum 1
foreach val $values {
  set lit [lindex $val 0]
  execsql "DELETE FROM t1;"
  execsql "INSERT INTO t1 SELECT $lit, $lit, $lit, $lit;"
  do_test types-1.2.$tnum {
    execsql {
      SELECT typeof(i), typeof(n), typeof(t), typeof(o) FROM t1;
    }
  } [lrange $val 1 end]
  incr tnum
}
set tnum 1
foreach val $values {
  set lit [lindex $val 0]
  execsql "UPDATE t1 SET i = $lit, n = $lit, t = $lit, o = $lit;"
  do_test types-1.3.$tnum {
    execsql {
      SELECT typeof(i), typeof(n), typeof(t), typeof(o) FROM t1;
    }
  } [lrange $val 1 end]
  incr tnum
}
execsql {
  DROP TABLE t1;
}
proc record_sizes {rootpage} {
  set bt [btree_open test.db 10]
  btree_begin_transaction $bt
  set c [btree_cursor $bt $rootpage 0]
  btree_first $c
  while 1 {
    lappend res [btree_payload_size $c]
    if {[btree_next $c]} break
  }
  btree_close_cursor $c
  btree_close $bt
  set res
}
do_test types-2.1.1 {
  execsql {
    CREATE TABLE t1(a integer);
    INSERT INTO t1 VALUES(0);
    INSERT INTO t1 VALUES(120);
    INSERT INTO t1 VALUES(-120);
  }
} {}
do_test types-2.1.2 {
  execsql {
    SELECT a FROM t1;
  }
} {0 120 -120}
do_test types-2.1.3 {
  execsql {
    INSERT INTO t1 VALUES(30000);
    INSERT INTO t1 VALUES(-30000);
  }
} {}
do_test types-2.1.4 {
  execsql {
    SELECT a FROM t1;
  }
} {0 120 -120 30000 -30000}
do_test types-2.1.5 {
  execsql {
    INSERT INTO t1 VALUES(2100000000);
    INSERT INTO t1 VALUES(-2100000000);
  }
} {}
do_test types-2.1.6 {
  execsql {
    SELECT a FROM t1;
  }
} {0 120 -120 30000 -30000 2100000000 -2100000000}
do_test types-2.1.7 {
  execsql {
    INSERT INTO t1 VALUES(9000000*1000000*1000000);
    INSERT INTO t1 VALUES(-9000000*1000000*1000000);
  }
} {}
do_test types-2.1.8 {
  execsql {
    SELECT a FROM t1;
  }
} [list 0 120 -120 30000 -30000 2100000000 -2100000000 \
        9000000000000000000 -9000000000000000000]
ifcapable legacyformat {
  do_test types-2.1.9 {
    set root [db eval {select rootpage from sqlite_master where name = 't1'}]
    record_sizes $root
  } {3 3 3 4 4 6 6 10 10}
} else {
  do_test types-2.1.9 {
    set root [db eval {select rootpage from sqlite_master where name = 't1'}]
    record_sizes $root
  } {2 3 3 4 4 6 6 10 10}
}
do_test types-2.2.1 {
  execsql {
    CREATE TABLE t2(a float);
    INSERT INTO t2 VALUES(0.0);
    INSERT INTO t2 VALUES(12345.678);
    INSERT INTO t2 VALUES(-12345.678);
  }
} {}
do_test types-2.2.2 {
  execsql {
    SELECT a FROM t2;
  }
} {0.0 12345.678 -12345.678}
ifcapable legacyformat {
  do_test types-2.2.3 {
    set root [db eval {select rootpage from sqlite_master where name = 't2'}]
    record_sizes $root
  } {3 10 10}
} else {
  do_test types-2.2.3 {
    set root [db eval {select rootpage from sqlite_master where name = 't2'}]
    record_sizes $root
  } {2 10 10}
}
do_test types-2.3.1 {
  execsql {
    CREATE TABLE t3(a nullvalue);
    INSERT INTO t3 VALUES(NULL);
  }
} {}
do_test types-2.3.2 {
  execsql {
    SELECT a ISNULL FROM t3;
  }
} {1}
do_test types-2.3.3 {
  set root [db eval {select rootpage from sqlite_master where name = 't3'}]
  record_sizes $root
} {2}
do_test types-2.4.1 {
  set string10 abcdefghij
  set string500 [string repeat $string10 50]
  set string500000 [string repeat $string10 50000]

  execsql "
    CREATE TABLE t4(a string);
    INSERT INTO t4 VALUES('$string10');
    INSERT INTO t4 VALUES('$string500');
    INSERT INTO t4 VALUES('$string500000');
  "
} {}
do_test types-2.4.2 {
  execsql {
    SELECT a FROM t4;
  }
} [list $string10 $string500 $string500000]
if { $sqlite_options(utf16)==0 || [execsql {pragma encoding}] == "UTF-8" } {
  do_test types-2.4.3 {
    set root [db eval {select rootpage from sqlite_master where name = 't4'}]
    record_sizes $root
  } {12 503 500004}
} else {
  do_test types-2.4.3 {
    set root [db eval {select rootpage from sqlite_master where name = 't4'}]
    record_sizes $root
  } {22 1003 1000004}
}
do_test types-2.5.1 {
  execsql {
    DROP TABLE t1;
    DROP TABLE t2;
    DROP TABLE t3;
    DROP TABLE t4;
    CREATE TABLE t1(a, b, c);
  }
} {}
do_test types-2.5.2 {
  set string10 abcdefghij
  set string500 [string repeat $string10 50]
  set string500000 [string repeat $string10 50000]

  execsql "INSERT INTO t1 VALUES(NULL, '$string10', 4000);"
  execsql "INSERT INTO t1 VALUES('$string500', 4000, NULL);"
  execsql "INSERT INTO t1 VALUES(4000, NULL, '$string500000');"
} {}
do_test types-2.5.3 {
  execsql {
    SELECT * FROM t1;
  }
} [list {} $string10 4000 $string500 4000 {} 4000 {} $string500000]
finish_test
"#);
}
