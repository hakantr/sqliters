#[test]
fn test_collate7() {
    sqliters_tcltest::run_test_script(
        "collate7",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::caseless_del 0
proc caseless_cmp {zLeft zRight} {
  string compare -nocase $zLeft $zRight
}
do_test collate7-1.1 {
  set cmd [list incr ::caseless_del]
  sqlite3_create_collation_v2 db CASELESS caseless_cmp $cmd
  set ::caseless_del
} {0}
do_test collate7-1.2 {
  sqlite_delete_collation db CASELESS
  set ::caseless_del
} {1}
do_test collate7-1.3 {
  catchsql {
    CREATE TABLE abc(a COLLATE CASELESS, b, c);
  }
} {1 {no such collation sequence: CASELESS}}
do_test collate7-1.4 {
  sqlite3_create_collation_v2 db CASELESS caseless_cmp {incr ::caseless_del}
  db close
  set ::caseless_del
} {2}
do_test collate7-2.1 {
  forcedelete test.db test.db-journal
  sqlite3 db test.db
  sqlite3_create_collation_v2 db CASELESS caseless_cmp {incr ::caseless_del}
  execsql {
    PRAGMA encoding='utf-16';
    CREATE TABLE abc16(a COLLATE CASELESS, b, c);
  } db
  set ::caseless_del
} {2}
do_test collate7-2.2 {
  execsql {
    SELECT * FROM abc16 WHERE a < 'abc';
  }
  set ::caseless_del
} {2}
do_test collate7-2.3 {
  sqlite_delete_collation db CASELESS
  set ::caseless_del
} {3}
do_test collate7-2.4 {
  catchsql {
    SELECT * FROM abc16 WHERE a < 'abc';
  }
} {1 {no such collation sequence: CASELESS}}
finish_test
"#,
    );
}

#[test]
fn test_removed_column_collation_reports_prepare_error() {
    sqliters_tcltest::run_test_script(
        "removed_column_collation_reports_prepare_error",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc caseless_cmp {left right} { string compare -nocase $left $right }
foreach encoding {UTF-8 UTF-16le UTF-16be} {
  reset_db
  execsql "PRAGMA encoding='$encoding';"
  db collate CASELESS caseless_cmp
  execsql {CREATE TABLE words(value TEXT COLLATE CASELESS, source TEXT)}
  do_execsql_test removed-column-collation-$encoding.1 {
    SELECT value FROM words WHERE value<'abc';
  } {}
  sqlite_delete_collation db CASELESS
  set n 1
  foreach select_sql {
    {SELECT value FROM words WHERE value<'abc'}
    {SELECT value FROM words WHERE 'abc'>value}
    {SELECT value FROM words ORDER BY value}
    {SELECT value FROM words GROUP BY value}
    {SELECT DISTINCT value FROM words}
  } {
    incr n
    do_catchsql_test removed-column-collation-$encoding.$n $select_sql       {1 {no such collation sequence: CASELESS}}
  }
  do_execsql_test removed-column-collation-$encoding.7 {
    SELECT value FROM words;
    SELECT value FROM words WHERE value<'abc' COLLATE BINARY;
    SELECT value FROM words ORDER BY value COLLATE BINARY;
    INSERT INTO words VALUES('A','test');
  } {}
  do_catchsql_test removed-column-collation-$encoding.8 {
    SELECT value FROM words WHERE value<'abc';
  } {1 {no such collation sequence: CASELESS}}
  db collate CASELESS caseless_cmp
  do_execsql_test removed-column-collation-$encoding.9 {
    SELECT value FROM words WHERE value<'abc';
    PRAGMA integrity_check;
  } {A ok}
}
finish_test
"#,
    );
}

#[test]
fn test_collation_v2_comparator_and_destructor_order() {
    sqliters_tcltest::run_test_script(
        "collation_v2_comparator_and_destructor_order",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl

proc ci_compare {left right} { string compare -nocase $left $right }
proc reverse_compare {left right} { string compare $right $left }
set ::collation_events {}

foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    set ::collation_events {}
    execsql "PRAGMA encoding='$encoding'"
    sqlite3_create_collation_v2 db custom_order ci_compare {lappend ::collation_events first}
    execsql {CREATE TABLE words(v TEXT COLLATE custom_order); INSERT INTO words VALUES('a'),('B');}
    do_execsql_test collation-v2-order-$encoding.1 {
        SELECT v FROM words WHERE v='A';
        SELECT v FROM words ORDER BY v;
    } {a a B}
    do_test collation-v2-order-$encoding.2 {
        sqlite3_create_collation_v2 db CUSTOM_ORDER reverse_compare {lappend ::collation_events second}
        set ::collation_events
    } {first}
    do_execsql_test collation-v2-order-$encoding.3 {
        SELECT v FROM words ORDER BY v;
    } {a B}
    do_test collation-v2-order-$encoding.4 {
        sqlite_delete_collation db custom_order
        set ::collation_events
    } {first second}
    do_catchsql_test collation-v2-order-$encoding.5 {
        SELECT v FROM words ORDER BY v;
    } {1 {no such collation sequence: custom_order}}
    do_test collation-v2-order-$encoding.6 {
        sqlite3_create_collation_v2 db custom_order ci_compare {lappend ::collation_events third}
        db close
        set ::collation_events
    } {first second third}
    do_test collation-v2-order-$encoding.7 {
        sqlite3 db :memory:
        sqlite3_create_collation_v2 db custom_order ci_compare {lappend ::collation_events fourth}
        rename db {}
        set ::collation_events
    } {first second third fourth}
    do_test collation-v2-order-$encoding.8 {
        sqlite3 db :memory:
        sqlite3_create_collation_v2 db custom_order ci_compare {lappend ::collation_events fifth}
        sqlite3 db :memory:
        db close
        set ::collation_events
    } {first second third fourth fifth}
}
finish_test
"#,
    );
}
