#[test]
fn test_rowvalue8() {
    sqliters_tcltest::run_test_script(
        "rowvalue8",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::testprefix rowvalue8
do_execsql_test 1.1 {
  CREATE TABLE t1(a INTEGER PRIMARY KEY,b,c,d);
  INSERT INTO t1(a,b,c,d) VALUES
     (1,1,2,3),
     (2,2,3,4),
     (3,1,2,4),
     (4,2,3,5),
     (5,3,4,6),
     (6,4,5,9);
  SELECT a, CASE (b,c) WHEN (1,2) THEN 'aleph'
                       WHEN (2,3) THEN 'bet'
                       WHEN (3,4) THEN 'gimel'
                       ELSE '-' END,
         '|'
    FROM t1
   ORDER BY a;
} {1 aleph | 2 bet | 3 aleph | 4 bet | 5 gimel | 6 - |}
do_execsql_test 1.2 {
  SELECT a, CASE (b,c,d) WHEN (1,2,3) THEN 'aleph'
                         WHEN (2,3,4) THEN 'bet'
                         WHEN (3,4,6) THEN 'gimel'
                         ELSE '-' END,
         '|'
    FROM t1
   ORDER BY a;
} {1 aleph | 2 bet | 3 - | 4 - | 5 gimel | 6 - |}
do_execsql_test 2.1 {
  CREATE TABLE t2(x INTEGER PRIMARY KEY, y);
  INSERT INTO t2(x,y) VALUES(1,6),(2,5),(3,4),(4,3),(5,2),(6,1);
  SELECT x, CASE (SELECT b,c FROM t1 WHERE a=y)
            WHEN (1,2) THEN 'aleph'
            WHEN (2,3) THEN 'bet'
            WHEN (3,4) THEN 'gimel'
            ELSE '-' END,
         '|'
    FROM t2
   ORDER BY +x;
} {1 - | 2 gimel | 3 bet | 4 aleph | 5 bet | 6 aleph |}
finish_test
"#,
    );
}

#[test]
fn test_literal_row_case_vm_evaluates_fields_per_step() {
    sqliters_tcltest::run_test_script(
        "literal_row_case_vm_evaluates_fields_per_step",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::called {}
proc mark_case {tag value} {
    lappend ::called $tag
    return [expr {int($value)}]
}
db function mark_case -argcount 2 mark_case
execsql {
    CREATE TABLE words(id INTEGER PRIMARY KEY, a, b);
    INSERT INTO words VALUES(1,1,2),(2,0,2),(3,NULL,2);
}
set sql {
    SELECT id,
           CASE (CASE WHEN a IS NULL THEN NULL ELSE mark_case('la',a) END,
                 mark_case('lb',b))
           WHEN (mark_case('w1a',1),mark_case('w1b',2)) THEN mark_case('t1',10)
           WHEN (mark_case('w2a',0),mark_case('w2b',2)) THEN mark_case('t2',20)
           ELSE mark_case('el',30)
           END
      FROM words ORDER BY id
}
do_test literal-row-case-step.1 {
    set s [sqlite3_prepare_v2 db $sql -1]
    set ::called {}
    set rc [sqlite3_step $s]
    list $rc [sqlite3_column_int $s 0] [sqlite3_column_int $s 1] $::called
} {SQLITE_ROW 1 10 {la lb w1a w1b t1}}
do_test literal-row-case-step.2 {
    set ::called {}
    set rc [sqlite3_step $s]
    list $rc [sqlite3_column_int $s 0] [sqlite3_column_int $s 1] $::called
} {SQLITE_ROW 2 20 {la lb w1a w2a w2b t2}}
do_test literal-row-case-step.3 {
    set ::called {}
    set rc [sqlite3_step $s]
    list $rc [sqlite3_column_int $s 0] [sqlite3_column_int $s 1] $::called
} {SQLITE_ROW 3 30 {lb w1a w1b w2a w2b el}}
do_test literal-row-case-step.4 {
    set ::called {}
    list [sqlite3_step $s] $::called
} {SQLITE_DONE {}}
do_test literal-row-case-step.5 {
    sqlite3_reset $s
    set ::called {}
    set rc [sqlite3_step $s]
    list $rc [sqlite3_column_int $s 0] [sqlite3_column_int $s 1] $::called
} {SQLITE_ROW 1 10 {la lb w1a w1b t1}}
do_test literal-row-case-step.6 {
    set ::called {}
    list [sqlite3_finalize $s] $::called
} {SQLITE_OK {}}
do_catchsql_test literal-row-case-prepare.1 {
    SELECT CASE (1,2) WHEN (1,2,3) THEN 8 END;
} {1 {row value misused}}
do_catchsql_test literal-row-case-prepare.2 {
    SELECT CASE (1,2) WHEN 1 THEN 8 END;
} {1 {row value misused}}
do_catchsql_test literal-row-case-prepare.3 {
    SELECT CASE (1,2) COLLATE BINARY WHEN (1,2) THEN 8 END;
} {1 {row value misused}}
do_catchsql_test literal-row-case-prepare.4 {
    SELECT CASE (1,2) WHEN (1,2) THEN 8 WHEN (1,2,3) THEN 9 END;
} {1 {row value misused}}
do_catchsql_test literal-row-case-prepare.5 {
    SELECT CASE 1 WHEN 1 THEN (1,2) ELSE 0 END;
} {1 {row value misused}}
finish_test
"#,
    );
}

#[test]
fn test_correlated_row_case_keeps_subquery_step_boundaries() {
    sqliters_tcltest::run_test_script(
        "correlated_row_case_keeps_subquery_step_boundaries",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc case_result {x} {
    lappend ::calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
reset_db
execsql "PRAGMA encoding='$encoding';"
db function case_result -argcount 1 case_result
execsql {
    CREATE TABLE words(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER);
    CREATE TABLE picks(id INTEGER PRIMARY KEY, ref INTEGER);
    INSERT INTO words VALUES(1,1,10),(2,2,20),(3,3,30);
    INSERT INTO picks VALUES(1,3),(2,1),(3,6),(4,2);
}
set sql {
    SELECT p.id,
           CASE (SELECT a,case_result(b) FROM words w WHERE w.id=p.ref)
             WHEN (1,10) THEN 'one' WHEN (2,20) THEN 'two'
             WHEN (3,30) THEN 'three' ELSE '-' END
      FROM picks p ORDER BY p.id
}
foreach {step row value calls} {1 1 three 30 2 2 one 10 3 3 - {} 4 4 two 20} {
    do_test correlated-row-case-$encoding.step.$step {
        if {$step == 1} {set s [sqlite3_prepare_v2 db $sql -1]}
        set ::calls {}
        set code [sqlite3_step $s]
        list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls
    } [list SQLITE_ROW $row $value $calls]
}
do_test correlated-row-case-$encoding.step.5 {
    set ::calls {}
    list [sqlite3_step $s] $::calls [sqlite3_finalize $s]
} {SQLITE_DONE {} SQLITE_OK}
do_test correlated-row-case-$encoding.reprepare {
    set bounded [string map [list "ORDER BY p.id" "WHERE p.id >= ?1 ORDER BY p.id"] $sql]
    set s [sqlite3_prepare_v2 db $bounded -1]
    sqlite3_bind_int $s 1 2
    set ::calls {}
    set code [sqlite3_step $s]
    set before [list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls]
    sqlite3_reset $s
    execsql {CREATE INDEX picks_ref ON picks(ref);}
    sqlite3_bind_int $s 1 4
    set ::calls {}
    set code [sqlite3_step $s]
    set after [list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls]
    list $before $after [sqlite3_finalize $s]
} {{SQLITE_ROW 2 one 10} {SQLITE_ROW 4 two 20} SQLITE_OK}
do_catchsql_test correlated-row-case-$encoding.width {
    SELECT CASE (SELECT a,b FROM words) WHEN (1,10,0) THEN 'bad' END;
} {1 {row value misused}}
}
finish_test
"#,
    );
}

#[test]
fn test_sorted_row_case_keeps_binary_keys_and_step_boundaries() {
    sqliters_tcltest::run_test_script(
        "sorted_row_case_keeps_binary_keys_and_step_boundaries",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc sorted_case_result {x} {
    lappend ::sorted_calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    db function sorted_case_result -argcount 1 sorted_case_result
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER);
        CREATE TABLE picks(id INTEGER PRIMARY KEY, ref INTEGER, token TEXT);
        INSERT INTO words VALUES(1,1,10),(2,2,20),(3,3,30);
        INSERT INTO picks VALUES
            (1,3,'a'),(2,1,char(256)),(3,6,'a'||char(0)||'x'),
            (4,2,char(128512)),(5,NULL,NULL),(6,1,'a'),(7,2,'A');
    }
    set sql {
        SELECT p.id, CASE (SELECT a,sorted_case_result(b) FROM words WHERE id=p.ref)
            WHEN (1,10) THEN 'one' WHEN (2,20) THEN 'two'
            WHEN (3,30) THEN 'three' ELSE '-' END
        FROM picks p ORDER BY p.token COLLATE BINARY DESC NULLS LAST, p.id
    }
    if {$encoding == "UTF-16le"} {
        set expected {3 - 1 three 6 one 7 two 4 two 2 one 5 -}
    } else {
        set expected {4 two 2 one 3 - 1 three 6 one 7 two 5 -}
    }
    do_test sorted-row-case-$encoding.rows {
        set s [sqlite3_prepare_v2 db $sql -1]
        set ::sorted_calls {}
        set rows {}
        set schedule {}
        while {[set code [sqlite3_step $s]] eq "SQLITE_ROW"} {
            lappend rows [sqlite3_column_int $s 0] [sqlite3_column_text $s 1]
            lappend schedule $::sorted_calls
            set ::sorted_calls {}
        }
        list $rows $schedule $code $::sorted_calls [sqlite3_finalize $s]
    } [list $expected [list {30 10 20 10 20} {} {} {} {} {} {}] SQLITE_DONE {} SQLITE_OK]
    do_test sorted-row-case-$encoding.reprepare {
        set s [sqlite3_prepare_v2 db $sql -1]
        set ::sorted_calls {}
        set code [sqlite3_step $s]
        set before [list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::sorted_calls]
        sqlite3_reset $s
        execsql {CREATE INDEX picks_ref ON picks(ref);}
        set ::sorted_calls {}
        set code [sqlite3_step $s]
        set after [list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::sorted_calls]
        list $before $after [sqlite3_finalize $s]
    } [list [concat SQLITE_ROW [lrange $expected 0 1] [list {30 10 20 10 20}]] \
            [concat SQLITE_ROW [lrange $expected 0 1] [list {30 10 20 10 20}]] SQLITE_OK]
}
finish_test
"#,
    );
}
