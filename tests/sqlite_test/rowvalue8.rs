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

#[test]
fn test_persisted_row_case_keeps_subquery_step_boundaries() {
    sqliters_tcltest::run_test_script(
        "persisted_row_case_keeps_subquery_step_boundaries",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc view_case_result {x} {
    lappend ::calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding'; PRAGMA trusted_schema=ON;"
    db function view_case_result -argcount 1 view_case_result
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER);
        CREATE TABLE picks(id INTEGER PRIMARY KEY, ref INTEGER);
        INSERT INTO words VALUES(1,1,10),(2,2,20),(3,3,30);
        INSERT INTO picks VALUES(1,3),(2,1),(3,6),(4,2);
        CREATE VIEW picked_word AS
            SELECT p.id,CASE
                (SELECT a,view_case_result(b) FROM words w WHERE w.id=p.ref)
                WHEN (1,10) THEN 'one' WHEN (2,20) THEN 'two'
                WHEN (3,30) THEN 'three' ELSE '-' END AS word
            FROM picks p;
        CREATE VIEW displayed_word(key,text) AS SELECT id,word FROM picked_word;
    }
    set sql {SELECT key,text FROM displayed_word ORDER BY key}
    foreach {step row value calls} {1 1 three 30 2 2 one 10 3 3 - {} 4 4 two 20} {
        do_test view-row-case-$encoding.step.$step {
            if {$step == 1} {set s [sqlite3_prepare_v2 db $sql -1]}
            set ::calls {}
            set code [sqlite3_step $s]
            list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls
        } [list SQLITE_ROW $row $value $calls]
    }
    do_test view-row-case-$encoding.reprepare {
        set ::calls {}
        set before [list [sqlite3_step $s] $::calls]
        sqlite3_reset $s
        execsql {CREATE INDEX picks_ref ON picks(ref);}
        set code [sqlite3_step $s]
        set after [list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls]
        list $before $after [sqlite3_finalize $s]
    } {{SQLITE_DONE {}} {SQLITE_ROW 1 three 30} SQLITE_OK}
    do_execsql_test view-row-case-$encoding.shadow {
        WITH displayed_word(key,text) AS (VALUES(42,'local'))
        SELECT key,text FROM displayed_word;
    } {42 local}
}
finish_test
"#,
    );
}

#[test]
fn test_binary_grouped_row_case_steps() {
    sqliters_tcltest::run_test_script(
        "binary_grouped_row_case_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc mark_group {x} {
    lappend ::group_calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    db function mark_group -argcount 1 mark_group
    execsql {
        CREATE TABLE points(id INTEGER PRIMARY KEY, token TEXT, amount INTEGER);
        CREATE TABLE map(token TEXT, a INTEGER, b INTEGER);
        INSERT INTO points VALUES(1,'b',3),(2,'a',2),(3,'a',1),(4,'b',5);
        INSERT INTO map VALUES('a',1,11),('b',2,22);
    }
    set sql {
        SELECT p.token,
               CASE (SELECT a,b FROM map m WHERE m.token=p.token)
                 WHEN (1,11) THEN mark_group(10*min(p.amount)+1)
                 WHEN (2,22) THEN mark_group(10*min(p.amount)+2)
                 ELSE -1 END AS label,
               count(*) AS amount
          FROM points p
         WHERE p.amount >= ?1
         GROUP BY p.token COLLATE BINARY
    }
    do_test group-case-$encoding.prepare {
        set s [sqlite3_prepare_v2 db $sql -1]
        sqlite3_bind_int $s 1 0
        list [sqlite3_column_name $s 0] [sqlite3_column_name $s 1] [sqlite3_column_name $s 2]
    } {token label amount}
    foreach {part token label amount} {1 a 11 2 2 b 32 2} {
        do_test group-case-$encoding.step.$part {
            set ::group_calls {}
            set rc [sqlite3_step $s]
            list $rc [sqlite3_column_text $s 0] [sqlite3_column_int $s 1] [sqlite3_column_int $s 2] $::group_calls
        } [list SQLITE_ROW $token $label $amount $label]
    }
    do_test group-case-$encoding.finish {
        set ::group_calls {}
        list [sqlite3_step $s] $::group_calls [sqlite3_finalize $s]
    } {SQLITE_DONE {} SQLITE_OK}
    do_test group-case-$encoding.reprepare {
        set s [sqlite3_prepare_v2 db $sql -1]
        sqlite3_bind_int $s 1 0
        set ::group_calls {}
        set before [sqlite3_step $s]
        set first [list $before [sqlite3_column_text $s 0] $::group_calls]
        sqlite3_reset $s
        execsql {CREATE TABLE unrelated(id INTEGER PRIMARY KEY);}
        sqlite3_bind_int $s 1 3
        set ::group_calls {}
        set after [sqlite3_step $s]
        list $first $after [sqlite3_column_text $s 0] [sqlite3_column_int $s 2] $::group_calls [sqlite3_finalize $s]
    } {{SQLITE_ROW a 11} SQLITE_ROW b 2 32 SQLITE_OK}
}
finish_test
"#,
    );
}

#[test]
fn test_binary_distinct_row_case_steps() {
    sqliters_tcltest::run_test_script(
        "binary_distinct_row_case_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc mark_distinct {x} {
    lappend ::callbacks $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    db function mark_distinct -argcount 1 mark_distinct
    execsql {
        CREATE TABLE message(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER);
        CREATE TABLE feed(id INTEGER PRIMARY KEY, ref INTEGER, label TEXT);
        INSERT INTO message VALUES(1,1,10),(2,2,20),(3,3,30);
        INSERT INTO feed VALUES(1,3,'other'),(2,1,'A'),(3,3,'other'),(4,2,'a'),(5,2,'a'),
                               (6,9,NULL),(7,9,NULL);
    }
    set sql {
        SELECT DISTINCT CASE
          (SELECT a,mark_distinct(b) FROM message m WHERE m.id=f.ref)
          WHEN (3,30) THEN ?1 ELSE f.label END COLLATE BINARY
        FROM feed f
    }
    set s [sqlite3_prepare_v2 db $sql -1]
    sqlite3_bind_text $s 1 A -1
    foreach {step expected callback} {1 A {30} 2 a {10 30 20} 3 {} {20}} {
        do_test binary-distinct-case-$encoding.step.$step {
            set ::callbacks {}
            set code [sqlite3_step $s]
            list $code [sqlite3_column_text $s 0] $::callbacks
        } [list SQLITE_ROW $expected $callback]
    }
    do_test binary-distinct-case-$encoding.done {
        set ::callbacks {}
        list [sqlite3_step $s] $::callbacks
    } {SQLITE_DONE {}}
    do_test binary-distinct-case-$encoding.reprepare {
        sqlite3_reset $s
        execsql {CREATE INDEX feed_ref ON feed(ref);}
        sqlite3_bind_text $s 1 Z -1
        set ::callbacks {}
        set c1 [sqlite3_step $s]
        set first [list $c1 [sqlite3_column_text $s 0] $::callbacks]
        set ::callbacks {}
        set c2 [sqlite3_step $s]
        set second [list $c2 [sqlite3_column_text $s 0] $::callbacks]
        list $first $second [sqlite3_finalize $s]
    } {{SQLITE_ROW Z 30} {SQLITE_ROW A 10} SQLITE_OK}
    do_execsql_test binary-distinct-case-$encoding.added-default {
        CREATE TABLE notes(id INTEGER PRIMARY KEY, ref INTEGER);
        INSERT INTO notes VALUES(1,2),(2,2),(3,3),(4,2);
        ALTER TABLE notes ADD COLUMN label TEXT DEFAULT 'A';
        INSERT INTO notes VALUES(5,2,'A'),(6,3,'A'),(7,2,'other'),(8,2,'other');
        SELECT DISTINCT CASE (SELECT a,b FROM message m WHERE m.id=n.ref)
            WHEN (3,30) THEN 'marker' ELSE n.label END COLLATE BINARY
            FROM notes n;
    } {A marker other}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_view_cte_expression_steps() {
    sqliters_tcltest::run_test_script(
        "row_case_view_cte_expression_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc note_case {x} {
    lappend ::calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding'; PRAGMA trusted_schema=ON;"
    db function note_case -argcount 1 note_case
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        CREATE TABLE picks(id INTEGER PRIMARY KEY,ref INTEGER);
        INSERT INTO words VALUES(1,1,10),(2,2,20),(3,3,30);
        INSERT INTO picks VALUES(1,3),(2,1),(3,6),(4,2);
        CREATE VIEW chosen AS SELECT p.id,
          CASE (SELECT a,note_case(b) FROM words w WHERE w.id=p.ref)
          WHEN (1,10) THEN 'one' WHEN (2,20) THEN 'two' WHEN (3,30) THEN 'three'
          ELSE '-' END AS label FROM picks p;
    }
    set sql {
        WITH source AS (SELECT id,label FROM chosen),
             consumer AS (SELECT id,label FROM source)
        SELECT p.id,
            (WITH source AS (VALUES(99,'wrong'))
             SELECT label FROM consumer WHERE id=p.id)
        FROM picks p WHERE p.id >= ?1 ORDER BY p.id
    }
    set s [sqlite3_prepare_v2 db $sql -1]
    sqlite3_bind_int $s 1 1
    foreach {step int value calls} {1 1 three {30} 2 2 one {10} 3 3 - {} 4 4 two {20}} {
        do_test view-reference-$encoding.step.$step {
            set ::calls {}
            set code [sqlite3_step $s]
            list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls
        } [list SQLITE_ROW $int $value $calls]
    }
    do_test view-reference-$encoding.reprepare {
        set ::calls {}
        set before [list [sqlite3_step $s] $::calls]
        sqlite3_reset $s
        execsql {CREATE INDEX picks_ref ON picks(ref);}
        sqlite3_bind_int $s 1 2
        set code [sqlite3_step $s]
        set after [list $code [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls]
        list $before $after [sqlite3_finalize $s]
    } {{SQLITE_DONE {}} {SQLITE_ROW 2 one 10} SQLITE_OK}
    do_execsql_test view-reference-$encoding.unused {
        WITH unused AS (SELECT * FROM chosen) SELECT 99;
    } {99}
    do_execsql_test view-reference-$encoding.shadow {
        WITH chosen(id,label) AS (VALUES(99,'local'))
        SELECT (SELECT label FROM chosen);
    } {local}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_default_text_keeps_database_encoding() {
    sqliters_tcltest::run_test_script(
        "row_case_default_text_keeps_database_encoding",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach {encoding expected} {UTF-8 C480 UTF-16le 0001 UTF-16be 0100} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    do_execsql_test column-default-encoding-$encoding {
        CREATE TABLE listing(id INTEGER PRIMARY KEY);
        INSERT INTO listing(id) VALUES(1);
        ALTER TABLE listing ADD COLUMN label TEXT DEFAULT 'Ā';
        ALTER TABLE listing ADD COLUMN amount INTEGER DEFAULT 5;
        ALTER TABLE listing ADD COLUMN data BLOB DEFAULT X'0102';
        INSERT INTO listing(id,label) VALUES(2,'Ā');
        INSERT INTO listing VALUES(3,NULL,6,X'03');
        SELECT id,
               CASE (SELECT 7,11)
                 WHEN (7,11) THEN hex(CAST(label AS BLOB)) ELSE '?' END,
               amount, hex(data), typeof(label), typeof(data)
          FROM listing ORDER BY id;
    } [list 1 $expected 5 0102 text blob 2 $expected 5 0102 text blob 3 {} 6 03 null blob]
}
finish_test
"#,
    );
}

#[test]
fn test_temp_row_case_view_shadowing_and_reprepare() {
    sqliters_tcltest::run_test_script(
        "temp_row_case_view_shadowing_and_reprepare",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc temp_case_note {x} {
    lappend ::calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding'; PRAGMA trusted_schema=ON;"
    db function temp_case_note -argcount 1 temp_case_note
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        CREATE TABLE picks(id INTEGER PRIMARY KEY,ref INTEGER);
        INSERT INTO words VALUES(1,1,10),(2,2,20);
        INSERT INTO picks VALUES(1,1),(2,2),(3,9);
        CREATE VIEW labels AS
          SELECT p.id,CASE (SELECT a,b FROM words w WHERE w.id=p.ref)
             WHEN (1,10) THEN 'permanent' ELSE '-' END AS label
          FROM picks p;
        CREATE TEMP VIEW labels AS
          SELECT p.id,CASE (SELECT a,temp_case_note(b) FROM words w WHERE w.id=p.ref)
             WHEN (1,10) THEN 'temporary' ELSE '-' END AS label
          FROM picks p;
    }
    db config enable_view off
    set sql {SELECT id,label FROM labels WHERE id>=?1 ORDER BY id}
    do_test temp-row-case-$encoding.prepare {
        set s [sqlite3_prepare_v2 db $sql -1]
        sqlite3_bind_int $s 1 1
        set ::calls {}
        set rc [sqlite3_step $s]
        list $rc [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls
    } {SQLITE_ROW 1 temporary 10}
    do_test temp-row-case-$encoding.next {
        set ::calls {}
        set rc [sqlite3_step $s]
        list $rc [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls
    } {SQLITE_ROW 2 - 20}
    do_catchsql_test temp-row-case-$encoding.main-blocked {
        SELECT label FROM main.labels WHERE id=1
    } {1 {access to view "labels" prohibited}}
    do_test temp-row-case-$encoding.reprepare {
        sqlite3_reset $s
        db config enable_view on
        execsql {DROP VIEW temp.labels}
        sqlite3_bind_int $s 1 1
        set ::calls {}
        set rc [sqlite3_step $s]
        list $rc [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls [sqlite3_finalize $s]
    } {SQLITE_ROW 1 permanent {} SQLITE_OK}
    db config enable_view off
    do_catchsql_test temp-row-case-$encoding.temp-relay-blocked {
        CREATE TEMP VIEW relay AS SELECT * FROM main.labels;
        SELECT label FROM relay WHERE id=1
    } {1 {access to view "labels" prohibited}}
    db config enable_view on
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_binary_physical_index_probe_encoding() {
    sqliters_tcltest::run_test_script(
        "row_case_binary_physical_index_probe_encoding",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc mark_index_lookup {x} {
    lappend ::index_calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    db function mark_index_lookup -argcount 1 mark_index_lookup
    execsql {
        CREATE TABLE list(id INTEGER PRIMARY KEY, label TEXT COLLATE BINARY, slot, payload TEXT);
        CREATE INDEX list_key ON list(label,slot DESC);
        CREATE TABLE detail(label TEXT COLLATE BINARY, slot, payload TEXT, PRIMARY KEY(label,slot DESC)) WITHOUT ROWID;
        CREATE TABLE marks(slot INTEGER PRIMARY KEY, a, b);
        INSERT INTO list VALUES(1,'A',1,'first'),(2,'A',2,'second'),(3,char(256),1,'wide');
        INSERT INTO detail SELECT label,slot,payload FROM list;
        INSERT INTO marks VALUES(1,1,10),(2,2,20);
    }
    set selected {
        SELECT i.payload,
               CASE (SELECT a,mark_index_lookup(b) FROM marks WHERE slot=i.slot)
                 WHEN (1,10) THEN 'one' WHEN (2,20) THEN 'two' ELSE '-' END AS tag
          FROM list i INDEXED BY list_key
         WHERE i.label=?1
         ORDER BY i.slot DESC, i.id
    }
    do_test binary-index-case-$encoding.unbound {
        set s [sqlite3_prepare_v2 db $selected -1]
        set ::index_calls {}
        list [sqlite3_column_name $s 0] [sqlite3_column_name $s 1] [sqlite3_step $s] $::index_calls
    } {payload tag SQLITE_DONE {}}
    do_test binary-index-case-$encoding.bind {
        sqlite3_reset $s
        sqlite3_bind_text $s 1 A 1
        set ::index_calls {}
        set code [sqlite3_step $s]
        list $code [sqlite3_column_text $s 0] [sqlite3_column_text $s 1] $::index_calls
    } {SQLITE_ROW second two 20}
    do_test binary-index-case-$encoding.range-next {
        set ::index_calls {}
        set code [sqlite3_step $s]
        list $code [sqlite3_column_text $s 0] [sqlite3_column_text $s 1] $::index_calls [sqlite3_step $s] [sqlite3_finalize $s]
    } {SQLITE_ROW first one 10 SQLITE_DONE SQLITE_OK}
    do_test binary-index-case-$encoding.without-rowid-reprepare {
        set lookup [string map [list "list i INDEXED BY list_key" "detail i" "WHERE i.label=?1" "WHERE i.label=?1 AND i.slot=?2" "ORDER BY i.slot DESC, i.id" ""] $selected]
        set s [sqlite3_prepare_v2 db $lookup -1]
        sqlite3_bind_text $s 1 A 1
        sqlite3_bind_int $s 2 1
        set ::index_calls {}
        set code [sqlite3_step $s]
        set before [list $code [sqlite3_column_text $s 0] [sqlite3_column_text $s 1] $::index_calls]
        sqlite3_reset $s
        execsql {CREATE TABLE unrelated(k INTEGER);}
        sqlite3_bind_text $s 1 A 1
        sqlite3_bind_int $s 2 2
        set ::index_calls {}
        set code [sqlite3_step $s]
        list $before $code [sqlite3_column_text $s 0] [sqlite3_column_text $s 1] $::index_calls [sqlite3_finalize $s]
    } {{SQLITE_ROW first one 10} SQLITE_ROW second two 20 SQLITE_OK}
    do_execsql_test binary-index-case-$encoding.mixed-probes {
        CREATE TABLE stock(id INTEGER PRIMARY KEY);
        INSERT INTO stock(id) VALUES(1),(2);
        ALTER TABLE stock ADD COLUMN label TEXT DEFAULT 'old';
        INSERT INTO stock VALUES(3,'old'),(4,'new'),(5,NULL);
        CREATE TABLE details(prefix INTEGER, label TEXT, a, b, PRIMARY KEY(prefix,label)) WITHOUT ROWID;
        INSERT INTO details VALUES(1,'old',1,10),(1,'new',2,20);
        SELECT s.id,
               CASE (SELECT a,b FROM details WHERE prefix=1 AND label=s.label)
                 WHEN (1,10) THEN 'one' WHEN (2,20) THEN 'two' ELSE '-' END
          FROM stock s ORDER BY s.id;
        SELECT s.id,
               CASE (SELECT a,b FROM details WHERE prefix=1 AND label=(
                        SELECT group_concat(p.label,'') FROM stock p WHERE p.id=s.id
                    ))
                 WHEN (1,10) THEN 'one' WHEN (2,20) THEN 'two' ELSE '-' END
          FROM stock s ORDER BY s.id;
    } {1 one 2 one 3 one 4 two 5 - 1 one 2 one 3 one 4 two 5 -}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_aggregate_text_keeps_database_encoding() {
    sqliters_tcltest::run_test_script(
        "row_case_aggregate_text_keeps_database_encoding",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach {encoding first second combined repeated} {
    UTF-8 41 42 412D42 542D54
    UTF-16le 4100 4200 41002D004200 54002D005400
    UTF-16be 0041 0042 0041002D0042 0054002D0054
} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE entries(id INTEGER PRIMARY KEY, label TEXT, amount INTEGER, data BLOB);
        INSERT INTO entries VALUES(1,'A',4,X'0102'),(2,'B',3,X'AA'),(3,NULL,NULL,NULL);
    }
    do_test aggregate-text-$encoding.bound {
        set s [sqlite3_prepare_v2 db {
            SELECT CASE (SELECT 7,11)
                WHEN (7,11) THEN hex(CAST(group_concat(?1,'-') AS BLOB)) ELSE '?' END
              FROM entries WHERE id<3
        } -1]
        sqlite3_bind_text $s 1 T 1
        set first_rc [sqlite3_step $s]
        list $first_rc [sqlite3_column_text $s 0] [sqlite3_step $s] [sqlite3_finalize $s]
    } [list SQLITE_ROW $repeated SQLITE_DONE SQLITE_OK]
    set window_sql {
        SELECT id, CASE (SELECT 7,11)
          WHEN (7,11) THEN hex(CAST(
            group_concat(label,?2) OVER (ORDER BY id ROWS UNBOUNDED PRECEDING)
            AS BLOB)) ELSE '?' END AS encoded
        FROM entries WHERE id>=?1 ORDER BY id
    }
    do_test aggregate-text-$encoding.prepare-window {
        set s [sqlite3_prepare_v2 db $window_sql -1]
        sqlite3_bind_int $s 1 1
        sqlite3_bind_text $s 2 - 1
        list [sqlite3_column_name $s 0] [sqlite3_column_name $s 1]
    } {id encoded}
    foreach {id bytes} [list 1 $first 2 $combined 3 $combined] {
        do_test aggregate-text-$encoding.window.$id {
            list [sqlite3_step $s] [sqlite3_column_int $s 0] [sqlite3_column_text $s 1]
        } [list SQLITE_ROW $id $bytes]
    }
    do_test aggregate-text-$encoding.done {
        list [sqlite3_step $s] [sqlite3_finalize $s]
    } {SQLITE_DONE SQLITE_OK}
    do_test aggregate-text-$encoding.reprepare-window {
        set s [sqlite3_prepare_v2 db $window_sql -1]
        sqlite3_bind_int $s 1 1
        sqlite3_bind_text $s 2 - 1
        set before [list [sqlite3_step $s] [sqlite3_column_int $s 0] [sqlite3_column_text $s 1]]
        sqlite3_reset $s
        execsql {CREATE TABLE unrelated(id INTEGER PRIMARY KEY);}
        sqlite3_bind_int $s 1 2
        sqlite3_bind_text $s 2 : 1
        set after [list [sqlite3_step $s] [sqlite3_column_int $s 0] [sqlite3_column_text $s 1]]
        list $before $after [sqlite3_finalize $s]
    } [list [list SQLITE_ROW 1 $first] [list SQLITE_ROW 2 $second] SQLITE_OK]
    do_execsql_test aggregate-text-$encoding.non-text {
        SELECT CASE (SELECT 7,11) WHEN (7,11) THEN sum(amount) ELSE 0 END,
               CASE (SELECT 7,11) WHEN (7,11) THEN hex(max(data)) ELSE '?' END,
               CASE (SELECT 7,11) WHEN (7,11) THEN typeof(max(data)) ELSE '?' END
          FROM entries;
        SELECT CASE (SELECT 7,11) WHEN (7,11) THEN typeof(group_concat(label)) ELSE '?' END
          FROM entries WHERE id=3;
    } {7 AA blob null}
}
finish_test
"#,
    );
}

#[test]
fn test_scalar_when_row_subquery_width_diagnostic() {
    sqliters_tcltest::run_test_script(
        "scalar_when_row_subquery_width_diagnostic",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc width_scalar_call {x} {
    lappend ::calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    db function width_scalar_call -argcount 1 width_scalar_call
    execsql {
        CREATE TABLE detail(id INTEGER PRIMARY KEY,a INT,b INT);
        INSERT INTO detail VALUES(1,1,10),(2,2,20);
    }
    do_catchsql_test scalar-when-width-$encoding.explicit {
        SELECT CASE (SELECT a,b FROM detail WHERE id=1) WHEN 1 THEN 'yes' END;
    } {1 {row value misused}}
    do_catchsql_test scalar-when-width-$encoding.star {
        SELECT CASE (SELECT * FROM (SELECT a,b FROM detail WHERE id=1)) WHEN 1 THEN 'yes' END;
    } {1 {row value misused}}
    do_catchsql_test scalar-when-width-$encoding.qualified-star {
        SELECT CASE (SELECT d.* FROM detail d WHERE id=1) WHEN 1 THEN 'yes' END;
    } {1 {row value misused}}
    do_catchsql_test scalar-when-width-$encoding.values {
        SELECT CASE (VALUES(1,10)) WHEN 1 THEN 'yes' WHEN 2 THEN 'other' END;
    } {1 {row value misused}}
    do_catchsql_test scalar-when-width-$encoding.projection {
        SELECT (SELECT a,b FROM detail WHERE id=1);
    } {1 {sub-select returns 2 columns - expected 1}}
    do_execsql_test scalar-when-width-$encoding.controls {
        SELECT CASE (SELECT a FROM detail WHERE id=1) WHEN 1 THEN 'single' END;
        SELECT CASE (SELECT * FROM (SELECT a FROM detail WHERE id=1)) WHEN 1 THEN 'star' END;
        SELECT CASE (SELECT d.* FROM (SELECT a FROM detail WHERE id=1) d) WHEN 1 THEN 'qualified' END;
        SELECT CASE (SELECT a,b FROM detail WHERE id=1) WHEN (1,10) THEN 'row' END;
    } {single star qualified row}
    do_test scalar-when-width-$encoding.callback {
        set ::calls {}
        set q [execsql {SELECT CASE (SELECT width_scalar_call(a) FROM detail WHERE id=1) WHEN 1 THEN 'yes' END;}]
        list $q $::calls
    } {yes 1}
    do_test scalar-when-width-$encoding.reprepare {
        execsql {CREATE TABLE evolving(a INT); INSERT INTO evolving VALUES(1);}
        set s [sqlite3_prepare_v2 db {SELECT CASE (SELECT * FROM evolving) WHEN 1 THEN 8 END;} -1]
        set first [list [sqlite3_step $s] [sqlite3_column_int $s 0]]
        sqlite3_reset $s
        execsql {ALTER TABLE evolving ADD COLUMN b INT;}
        set rc [sqlite3_step $s]
        set message [sqlite3_errmsg db]
        list $first $rc $message [sqlite3_finalize $s]
    } {{SQLITE_ROW 8} SQLITE_ERROR {row value misused} SQLITE_ERROR}
}
finish_test
"#,
    );
}

#[test]
fn test_recursive_row_case_view_steps() {
    sqliters_tcltest::run_test_script(
        "recursive_row_case_view_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc recursive_case_note {x} {
    lappend ::calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding'; PRAGMA trusted_schema=ON;"
    db function recursive_case_note -argcount 1 recursive_case_note
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        CREATE TABLE picks(id INTEGER PRIMARY KEY,ref INTEGER);
        INSERT INTO words VALUES(1,1,10),(2,2,20),(3,3,30);
        INSERT INTO picks VALUES(1,3),(2,1),(3,9),(4,2);
        CREATE VIEW choices AS
          SELECT p.id,CASE (SELECT a,recursive_case_note(b) FROM words w WHERE w.id=p.ref)
             WHEN (1,10) THEN 'one' WHEN (2,20) THEN 'two' WHEN (3,30) THEN 'three'
             ELSE '-' END AS label
          FROM picks p;
    }
    set sql {
        WITH RECURSIVE walk(id,label) AS (
            SELECT id,label FROM choices WHERE id=?1
            UNION ALL
            SELECT v.id,v.label FROM walk w JOIN choices v ON v.id=w.id+1 WHERE w.id<?2
        )
        SELECT id,label FROM walk
    }
    set s [sqlite3_prepare_v2 db $sql -1]
    sqlite3_bind_int $s 1 1
    sqlite3_bind_int $s 2 4
    foreach {step id label calls} {1 1 three {30} 2 2 one {10} 3 3 - {} 4 4 two {20}} {
        do_test recursive-row-case-$encoding.step.$step {
            set ::calls {}
            set rc [sqlite3_step $s]
            list $rc [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls
        } [list SQLITE_ROW $id $label $calls]
    }
    do_test recursive-row-case-$encoding.reprepare {
        set ::calls {}
        set before [list [sqlite3_step $s] $::calls]
        sqlite3_reset $s
        execsql {CREATE INDEX picks_ref ON picks(ref);}
        sqlite3_bind_int $s 1 2
        set rc [sqlite3_step $s]
        set after [list $rc [sqlite3_column_int $s 0] [sqlite3_column_text $s 1] $::calls]
        list $before $after [sqlite3_finalize $s]
    } {{SQLITE_DONE {}} {SQLITE_ROW 2 one 10} SQLITE_OK}
    do_execsql_test recursive-row-case-$encoding.expression-subquery {
        WITH RECURSIVE seq(id,label) AS (
            SELECT 1,(SELECT label FROM choices WHERE id=1)
            UNION ALL
            SELECT seq.id+1,(SELECT label FROM choices WHERE id=seq.id+1)
            FROM seq WHERE seq.id<4
        )
        SELECT id,label FROM seq;
    } {1 three 2 one 3 - 4 two}
    do_execsql_test recursive-row-case-$encoding.shadow {
        WITH RECURSIVE choices(id,label) AS (
            VALUES(1,'lexical') UNION ALL SELECT id+1,label FROM choices WHERE id<2
        ) SELECT id,label FROM choices;
    } {1 lexical 2 lexical}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_binary_membership_and_null_semantics() {
    sqliters_tcltest::run_test_script(
        "row_case_binary_membership_and_null_semantics",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE facts(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER);
        INSERT INTO facts VALUES(1,1,10),(2,2,20),(3,3,30);
        CREATE TABLE items(id INTEGER PRIMARY KEY, ref INTEGER, label TEXT);
        INSERT INTO items VALUES(1,1,'A'),(2,2,'a'),(3,3,NULL),
            (4,9,'tail'),(5,1,char(256)),(6,1,'A'||char(0)||'B');
        CREATE VIEW selected AS
            SELECT i.id, CASE (SELECT a,b FROM facts WHERE id=i.ref)
              WHEN (2,20) THEN 'new' ELSE i.label END AS token
            FROM items i;
    }
    do_execsql_test row-case-in-$encoding.1 {
        SELECT 2 IN (SELECT id FROM selected WHERE token='new'),
               'A' COLLATE BINARY IN (SELECT token FROM selected),
               'a' COLLATE BINARY IN (SELECT token FROM selected),
               char(256) IN (SELECT token FROM selected),
               ('A'||char(0)||'B') IN (SELECT token FROM selected);
    } {1 1 {} 1 1}
    do_execsql_test row-case-in-$encoding.2 {
        SELECT (2,'new') IN (SELECT id,token FROM selected),
               (2,'a') IN (SELECT id,token FROM selected),
               (3,'x') NOT IN (SELECT id,token FROM selected),
               (99,NULL) NOT IN (SELECT id,token FROM selected);
    } {1 0 {} 1}
    do_execsql_test row-case-in-$encoding.3 {
        SELECT 'absent' NOT IN (SELECT token FROM selected),
               'absent' NOT IN (SELECT token FROM selected WHERE token IS NOT NULL),
               NULL IN (SELECT token FROM selected WHERE id < 0),
               NULL NOT IN (SELECT token FROM selected WHERE id < 0);
    } {{} 1 0 1}
    do_test row-case-in-$encoding.reprepare {
        set s [sqlite3_prepare_v2 db {
            SELECT ?1 COLLATE BINARY IN (SELECT token FROM selected WHERE id<>3)
        } -1]
        sqlite3_bind_text $s 1 new -1
        set before [list [sqlite3_step $s] [sqlite3_column_int $s 0]]
        sqlite3_reset $s
        execsql {CREATE INDEX item_ref ON items(ref);}
        sqlite3_bind_text $s 1 missing -1
        set after [list [sqlite3_step $s] [sqlite3_column_int $s 0]]
        list $before $after [sqlite3_step $s] [sqlite3_finalize $s]
    } {{SQLITE_ROW 1} {SQLITE_ROW 0} SQLITE_DONE SQLITE_OK}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_builtin_group_collations_and_steps() {
    sqliters_tcltest::run_test_script(
        "row_case_builtin_group_collations_and_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc observe_builtin_group {x} {
    lappend ::group_calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    db function observe_builtin_group -argcount 1 observe_builtin_group
    execsql {
        CREATE TABLE points(id INTEGER PRIMARY KEY, token TEXT, pad TEXT, bucket INTEGER);
        CREATE TABLE map(token TEXT,a INTEGER,b INTEGER,c INTEGER);
        INSERT INTO points VALUES(1,'A','z ',1),(2,'a','z',1),(3,'B','z',1),(4,'b','z ',1);
        INSERT INTO map VALUES('A',1,11,0),('B',2,22,0);
    }
    set sql {
        SELECT p.token,
               CASE (SELECT a,observe_builtin_group(b) FROM map m WHERE m.token=p.token)
                   WHEN (1,11) THEN 'one' WHEN (2,22) THEN 'two' ELSE '-' END AS tag,
               count(*) AS n
          FROM points p
         WHERE p.id>=?1
         GROUP BY p.token COLLATE NOCASE
    }
    do_test builtin-group-$encoding.prepare {
        set s [sqlite3_prepare_v2 db $sql -1]
        sqlite3_bind_int $s 1 1
        list [sqlite3_column_name $s 0] [sqlite3_column_name $s 1] [sqlite3_column_name $s 2]
    } {token tag n}
    foreach {key token tag calls} {1 A one 11 2 B two 22} {
        do_test builtin-group-$encoding.step.$key {
            set ::group_calls {}
            list [sqlite3_step $s] [sqlite3_column_text $s 0] [sqlite3_column_text $s 1] [sqlite3_column_int $s 2] $::group_calls
        } [list SQLITE_ROW $token $tag 2 $calls]
    }
    do_test builtin-group-$encoding.finish {
        set ::group_calls {}
        list [sqlite3_step $s] $::group_calls [sqlite3_finalize $s]
    } {SQLITE_DONE {} SQLITE_OK}
    do_test builtin-group-$encoding.reprepare {
        set s [sqlite3_prepare_v2 db $sql -1]
        sqlite3_bind_int $s 1 1
        set ::group_calls {}
        set before [list [sqlite3_step $s] [sqlite3_column_text $s 0] $::group_calls]
        sqlite3_reset $s
        execsql {CREATE TABLE unused(id INTEGER PRIMARY KEY);}
        sqlite3_bind_int $s 1 3
        set ::group_calls {}
        set after [list [sqlite3_step $s] [sqlite3_column_text $s 0] $::group_calls]
        list $before $after [sqlite3_finalize $s]
    } {{SQLITE_ROW A 11} {SQLITE_ROW B 22} SQLITE_OK}
    do_execsql_test builtin-group-$encoding.mixed {
        SELECT min(id),count(*) FROM points
         GROUP BY (CASE (SELECT 7,11) WHEN (7,11) THEN token ELSE '' END) COLLATE NOCASE,
                  pad COLLATE RTRIM,bucket COLLATE BINARY
         ORDER BY min(id);
    } {1 2 3 2}
    do_execsql_test builtin-group-$encoding.rtrim {
        INSERT INTO points VALUES(5,'A ','z',1),(6,'A  ','z',1);
        SELECT min(id),count(*) FROM points
         GROUP BY (CASE (SELECT 7,11) WHEN (7,11) THEN token ELSE '' END) COLLATE RTRIM
         ORDER BY min(id);
    } {1 3 2 1 3 1 4 1}
    do_catchsql_test builtin-group-$encoding.width {
        SELECT count(*) FROM points
         GROUP BY (CASE (SELECT a,b,c FROM map) WHEN (1,11) THEN token END) COLLATE NOCASE;
    } {1 {row value misused}}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_binary_ordered_limit_and_offset() {
    sqliters_tcltest::run_test_script(
        "row_case_binary_ordered_limit_and_offset",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER);
        INSERT INTO words VALUES (1,1,10),(2,2,20);
        CREATE TABLE picks(id INTEGER PRIMARY KEY, ref INTEGER, label TEXT);
        INSERT INTO picks VALUES (1,1,'A'),(2,1,'B'),(3,1,char(256)),
            (4,1,'A'),(5,2,'unused'),(6,1,NULL),(7,1,'C'),(8,1,char(128512));
        CREATE VIEW ranked AS
            SELECT p.id, CASE (SELECT a,b FROM words WHERE id=p.ref)
                WHEN (2,20) THEN 'Z' ELSE p.label END AS label
              FROM picks p;
    }
    if {$encoding eq "UTF-16le"} {
        set asc {8 1 4}
        set desc {7 2 4}
    } else {
        set asc {4 2 7}
        set desc {3 5 7}
    }
    do_execsql_test row-case-limit-$encoding.asc {
        SELECT id FROM ranked ORDER BY label COLLATE BINARY ASC NULLS LAST,id LIMIT 3 OFFSET 1;
    } $asc
    do_execsql_test row-case-limit-$encoding.desc {
        SELECT id FROM ranked ORDER BY label COLLATE BINARY DESC NULLS FIRST,id DESC LIMIT 3 OFFSET 2;
    } $desc
    do_execsql_test row-case-limit-$encoding.zero {
        SELECT id FROM ranked ORDER BY label COLLATE BINARY,id LIMIT 0;
    } {}
    do_test row-case-limit-$encoding.reprepare {
        set s [sqlite3_prepare_v2 db {
            SELECT id FROM ranked ORDER BY label COLLATE BINARY NULLS LAST,id LIMIT ?1 OFFSET 1
        } -1]
        sqlite3_bind_int $s 1 1
        set first [list [sqlite3_step $s] [sqlite3_column_int $s 0]]
        set done1 [sqlite3_step $s]
        sqlite3_reset $s
        execsql {CREATE INDEX picks_ref ON picks(ref);}
        sqlite3_bind_int $s 1 2
        set second [list [sqlite3_step $s] [sqlite3_column_int $s 0]]
        set third [list [sqlite3_step $s] [sqlite3_column_int $s 0]]
        list $first $done1 $second $third [sqlite3_step $s] [sqlite3_finalize $s]
    } [list [list SQLITE_ROW [lindex $asc 0]] SQLITE_DONE \
            [list SQLITE_ROW [lindex $asc 0]] [list SQLITE_ROW [lindex $asc 1]] SQLITE_DONE SQLITE_OK]
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_nocase_index_probe() {
    sqliters_tcltest::run_test_script(
        "row_case_nocase_index_probe",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE tuple_word(k TEXT COLLATE NOCASE PRIMARY KEY,a INTEGER,b INTEGER) WITHOUT ROWID;
        INSERT INTO tuple_word VALUES ('alpha',1,10),('beta',2,20),
            (char(256)||'a',3,30),(char(257)||'A',4,40);
        CREATE TABLE lookup(id INTEGER PRIMARY KEY,k TEXT COLLATE NOCASE,s TEXT COLLATE BINARY,a INTEGER,b INTEGER);
        INSERT INTO lookup VALUES (1,'alpha','x',1,10),(2,'ALPHA','y',2,20),
            (3,'beta','x',3,30),(4,'beta','Y',4,40);
        CREATE INDEX lookup_key ON lookup(k,s,a,b);
    }
    do_execsql_test row-case-nocase-index-$encoding.primary {
        SELECT CASE (SELECT a,b FROM tuple_word WHERE k='ALPHA')
                    WHEN (1,10) THEN 'first' ELSE '-' END,
               CASE (SELECT a,b FROM tuple_word WHERE k=char(256)||'A')
                    WHEN (3,30) THEN 'upper' ELSE '-' END,
               CASE (SELECT a,b FROM tuple_word WHERE k='absent')
                    WHEN (1,10) THEN 'first' ELSE '-' END;
    } {first upper -}
    do_execsql_test row-case-nocase-index-$encoding.composite {
        SELECT CASE (SELECT a,b FROM lookup INDEXED BY lookup_key WHERE k='AlPhA' AND s='y')
                   WHEN (2,20) THEN 'hit' ELSE 'miss' END,
               CASE (SELECT a,b FROM lookup INDEXED BY lookup_key WHERE k='AlPhA' AND s='Y')
                   WHEN (2,20) THEN 'hit' ELSE 'miss' END;
    } {hit miss}
    do_test row-case-nocase-index-$encoding.reprepare {
        set s [sqlite3_prepare_v2 db {
            SELECT CASE (SELECT a,b FROM tuple_word WHERE k=?1)
                        WHEN (1,10) THEN 'first' WHEN (2,20) THEN 'second' ELSE '-' END
        } -1]
        sqlite3_bind_text $s 1 ALPHA -1
        set rc [sqlite3_step $s]
        set first [list $rc [sqlite3_column_text $s 0]]
        sqlite3_reset $s
        execsql {CREATE TABLE reprepare_control(id INTEGER PRIMARY KEY);}
        sqlite3_bind_text $s 1 BETA -1
        set rc [sqlite3_step $s]
        set second [list $rc [sqlite3_column_text $s 0]]
        list $first $second [sqlite3_step $s] [sqlite3_finalize $s]
    } {{SQLITE_ROW first} {SQLITE_ROW second} SQLITE_DONE SQLITE_OK}
}
finish_test
"#,
    );
}

#[test]
fn test_recursive_union_row_case_duplicate_steps() {
    sqliters_tcltest::run_test_script(
        "recursive_union_row_case_duplicate_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl

proc fifo_case_note {v} {
    lappend ::calls $v
    return $v
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding'; PRAGMA trusted_schema=ON;"
    db function fifo_case_note -argcount 1 fifo_case_note
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        CREATE TABLE picks(id INTEGER PRIMARY KEY,ref INTEGER,token);
        INSERT INTO words VALUES(1,1,10),(2,2,20);
        INSERT INTO picks VALUES(1,1,'A'),(2,1,char(256)),(3,1,'A'),(4,999,NULL),
            (5,1,1),(6,1,1.0),(7,1,X'410042'),(8,1,'A'||char(0)||'B'),
            (9,1,char(66376)),(10,2,'obsolete'),(11,999,NULL);
        CREATE VIEW choices AS
          SELECT p.id,CASE (SELECT a,fifo_case_note(b) FROM words w WHERE w.id=p.ref)
            WHEN (2,20) THEN 'replacement' ELSE p.token END AS token
          FROM picks p;
    }
    set sql {
        WITH RECURSIVE wave(value) AS (
            SELECT token FROM choices WHERE id<=?1
            UNION
            SELECT value FROM wave
        )
        SELECT typeof(value),CASE typeof(value) WHEN 'text'
            THEN printf('%d:%d:%d',unicode(value),length(value),instr(value,char(0)))
            ELSE quote(value) END FROM wave
    }
    set s [sqlite3_prepare_v2 db $sql -1]
    sqlite3_bind_int $s 1 11
    foreach {step type value calls} {
        1 text 65:1:0 {10 10 10 10 10 10 10 10 20}
        2 text 256:1:0 {}
        3 null NULL {}
        4 integer 1 {}
        5 blob X'410042' {}
        6 text 65:1:2 {}
        7 text 66376:1:0 {}
        8 text 114:11:0 {}
    } {
        do_test fifo-row-case-$encoding.step.$step {
            set ::calls {}
            set rc [sqlite3_step $s]
            list $rc [sqlite3_column_text $s 0] [sqlite3_column_text $s 1] $::calls
        } [list SQLITE_ROW $type $value $calls]
    }
    do_test fifo-row-case-$encoding.reprepare {
        set ::calls {}
        set before [list [sqlite3_step $s] $::calls]
        sqlite3_reset $s
        execsql {CREATE INDEX picks_ref ON picks(ref);}
        sqlite3_bind_int $s 1 2
        set rc [sqlite3_step $s]
        set after [list $rc [sqlite3_column_text $s 1] $::calls]
        list $before $after [sqlite3_finalize $s]
    } {{SQLITE_DONE {}} {SQLITE_ROW 65:1:0 {10 10}} SQLITE_OK}
    do_test fifo-row-case-$encoding.empty {
        set ::calls {}
        set s [sqlite3_prepare_v2 db $sql -1]
        sqlite3_bind_int $s 1 0
        list [sqlite3_step $s] $::calls [sqlite3_finalize $s]
    } {SQLITE_DONE {} SQLITE_OK}
    do_execsql_test fifo-row-case-$encoding.changes {
        SELECT changes(),total_changes();
    } {11 13}
}
finish_test

"#,
    );
}

#[test]
fn test_row_case_builtin_distinct_collations_and_steps() {
    sqliters_tcltest::run_test_script(
        "row_case_builtin_distinct_collations_and_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc observe_builtin_distinct {x} {
    lappend ::distinct_calls $x
    return [expr {int($x)}]
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    db function observe_builtin_distinct -argcount 1 observe_builtin_distinct
    execsql {
        CREATE TABLE message(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        CREATE TABLE feed(id INTEGER PRIMARY KEY,ref INTEGER,label TEXT);
        INSERT INTO message VALUES(1,1,10),(2,2,20),(3,3,30);
        INSERT INTO feed VALUES(1,3,'other'),(2,1,'a'),(3,3,'other'),(4,2,'B'),(5,2,'b'),(6,9,NULL),(7,9,NULL);
    }
    set sql {
        SELECT DISTINCT CASE (SELECT a,observe_builtin_distinct(b) FROM message m WHERE m.id=f.ref)
                        WHEN (3,30) THEN ?1 ELSE f.label END COLLATE NOCASE AS label
          FROM feed f
    }
    set s [sqlite3_prepare_v2 db $sql -1]
    sqlite3_bind_text $s 1 A -1
    foreach {step label calls} {1 A 30 2 B {10 30 20} 3 {} 20} {
        do_test builtin-distinct-$encoding.step.$step {
            set ::distinct_calls {}
            list [sqlite3_step $s] [sqlite3_column_name $s 0] [sqlite3_column_text $s 0] $::distinct_calls
        } [list SQLITE_ROW label $label $calls]
    }
    do_test builtin-distinct-$encoding.done {
        set ::distinct_calls {}
        list [sqlite3_step $s] $::distinct_calls
    } {SQLITE_DONE {}}
    do_test builtin-distinct-$encoding.reprepare {
        sqlite3_reset $s
        execsql {CREATE INDEX feed_ref ON feed(ref);}
        sqlite3_bind_text $s 1 Z -1
        set ::distinct_calls {}
        set first [list [sqlite3_step $s] [sqlite3_column_text $s 0] $::distinct_calls]
        set ::distinct_calls {}
        set second [list [sqlite3_step $s] [sqlite3_column_text $s 0] $::distinct_calls]
        list $first $second [sqlite3_finalize $s]
    } {{SQLITE_ROW Z 30} {SQLITE_ROW a 10} SQLITE_OK}
    do_execsql_test builtin-distinct-$encoding.default {
        CREATE TABLE notes(id INTEGER PRIMARY KEY,ref INTEGER);
        INSERT INTO notes VALUES(1,2),(2,3),(3,2);
        ALTER TABLE notes ADD COLUMN label TEXT DEFAULT 'A ';
        INSERT INTO notes VALUES(4,2,'A'),(5,2,'a'),(6,3,'other');
        SELECT DISTINCT CASE (SELECT a,b FROM message m WHERE m.id=n.ref)
                        WHEN (3,30) THEN 'marker' ELSE n.label END COLLATE RTRIM FROM notes n;
    } {{A } marker a}
    do_execsql_test builtin-distinct-$encoding.mixed {
        CREATE TABLE mixed(id INTEGER PRIMARY KEY,label TEXT,pad TEXT,code INTEGER);
        INSERT INTO mixed VALUES(1,'A','x ',1),(2,'a','x',1),(3,'a','x',2),(4,'A','x ',2),(5,'b','x',1);
        SELECT DISTINCT code COLLATE BINARY,
                        (CASE (SELECT 7,11) WHEN (7,11) THEN label END) COLLATE NOCASE,
                        pad COLLATE RTRIM FROM mixed;
    } {1 A {x } 2 a x 1 b x}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_builtin_order_sort_and_limit() {
    sqliters_tcltest::run_test_script(
        "row_case_builtin_order_sort_and_limit",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        INSERT INTO words VALUES(1,1,10),(2,2,20);
        CREATE TABLE picks(id INTEGER PRIMARY KEY,ref INTEGER,label TEXT);
        INSERT INTO picks VALUES(1,1,'a'),(2,1,'A'),(3,1,'b  '),(4,1,'b'),(5,2,'unused'),
            (6,1,NULL),(7,1,'a '),(8,1,char(256)),(9,1,char(128512)),(10,1,'b '),(11,1,'');
        CREATE VIEW ranked AS
            SELECT p.id, CASE (SELECT a,b FROM words WHERE id=p.ref)
                WHEN (2,20) THEN 'z' ELSE p.label END AS label
              FROM picks p;
    }
    do_execsql_test builtin-order-$encoding.full-nocase {
        SELECT id FROM ranked ORDER BY label COLLATE NOCASE ASC NULLS LAST,id;
    } {11 1 2 7 4 10 3 5 8 9 6}
    do_execsql_test builtin-order-$encoding.full-rtrim {
        SELECT id FROM ranked ORDER BY label COLLATE RTRIM DESC NULLS FIRST,id DESC;
    } {6 9 8 5 10 4 3 7 1 2 11}
    do_execsql_test builtin-order-$encoding.final-sort {
        CREATE TABLE grouped(id INTEGER PRIMARY KEY,g INTEGER,t TEXT);
        INSERT INTO grouped VALUES(1,1,'b '),(2,1,'b '),(3,2,'a'),(4,2,'a'),(5,3,'A '),
            (6,3,'A '),(7,4,NULL),(8,4,NULL),(9,5,'Z'),(10,5,'Z');
        SELECT g,count(*),min(id) FROM grouped
          GROUP BY g
          ORDER BY (CASE (SELECT 1,10) WHEN (1,10) THEN min(t) END) COLLATE NOCASE DESC NULLS FIRST,g;
    } {4 2 7 5 2 9 1 2 1 3 2 5 2 2 3}
    do_execsql_test builtin-order-$encoding.limit-nocase {
        SELECT id FROM ranked ORDER BY label COLLATE NOCASE NULLS LAST,id LIMIT 3 OFFSET 1;
    } {1 2 7}
    do_execsql_test builtin-order-$encoding.limit-rtrim {
        SELECT id FROM ranked ORDER BY label COLLATE RTRIM DESC NULLS FIRST,id DESC LIMIT 4 OFFSET 2;
    } {8 5 10 4}
    do_execsql_test builtin-order-$encoding.zero {
        SELECT id FROM ranked ORDER BY label COLLATE NOCASE NULLS LAST,id LIMIT 0;
    } {}
    do_execsql_test builtin-order-$encoding.negative {
        SELECT id FROM ranked ORDER BY label COLLATE NOCASE NULLS LAST,id LIMIT -1 OFFSET 8;
        SELECT id FROM ranked ORDER BY label COLLATE NOCASE NULLS LAST,id LIMIT 2 OFFSET -4;
    } {8 9 6 11 1}
    do_catchsql_test builtin-order-$encoding.limit-error {
        SELECT id FROM ranked ORDER BY label COLLATE NOCASE,id LIMIT 'three';
    } {1 {datatype mismatch}}
    do_test builtin-order-$encoding.reprepare {
        set s [sqlite3_prepare_v2 db {
            SELECT id FROM ranked WHERE id>=?1
              ORDER BY label COLLATE NOCASE NULLS LAST,id LIMIT ?2 OFFSET ?3
        } -1]
        sqlite3_bind_int $s 1 1
        sqlite3_bind_int $s 2 2
        sqlite3_bind_int $s 3 1
        set first [list [sqlite3_step $s] [sqlite3_column_name $s 0] [sqlite3_column_int $s 0]]
        set second [list [sqlite3_step $s] [sqlite3_column_int $s 0]]
        set done1 [sqlite3_step $s]
        sqlite3_reset $s
        execsql {CREATE INDEX picks_ref ON picks(ref);}
        sqlite3_bind_int $s 1 3
        sqlite3_bind_int $s 2 1
        sqlite3_bind_int $s 3 2
        set third [list [sqlite3_step $s] [sqlite3_column_int $s 0]]
        list $first $second $done1 $third [sqlite3_step $s] [sqlite3_finalize $s]
    } {{SQLITE_ROW id 1} {SQLITE_ROW 2} SQLITE_DONE {SQLITE_ROW 4} SQLITE_DONE SQLITE_OK}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_streaming_index_groups() {
    sqliters_tcltest::run_test_script(
        "row_case_streaming_index_groups",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY,k TEXT COLLATE NOCASE,bucket INTEGER,route INTEGER);
        CREATE INDEX words_key ON words(k,bucket DESC);
        INSERT INTO words VALUES (1,'a',1,1),(2,'A',1,2),(3,'B',2,2),
            (4,'b',1,1),(5,NULL,1,2),(6,NULL,1,1),(7,'Ā',2,1),(8,'Ā',2,2),
            (9,'ā',2,1),(10,'a',2,2);
        CREATE TABLE kinds(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        INSERT INTO kinds VALUES (1,1,10),(2,2,20);
    }
    do_execsql_test row-case-index-group-$encoding.prefix {
        SELECT quote(k),count(*),min(id),
            max(CASE (SELECT a,b FROM kinds d WHERE d.id=i.route) WHEN (2,20) THEN 2 ELSE 1 END)
        FROM words i INDEXED BY words_key GROUP BY k;
    } {NULL 2 5 2 'a' 3 1 2 'B' 2 3 2 'Ā' 2 7 2 'ā' 1 9 1}
    do_execsql_test row-case-index-group-$encoding.composite {
        SELECT quote(k),bucket,count(*),min(id),
            max(CASE (SELECT a,b FROM kinds d WHERE d.id=i.route) WHEN (2,20) THEN 2 ELSE 1 END)
        FROM words i INDEXED BY words_key GROUP BY k,bucket;
    } {NULL 1 2 5 2 'a' 2 1 10 2 'A' 1 2 1 2 'B' 2 1 3 2 'b' 1 1 4 1 'Ā' 2 2 7 2 'ā' 2 1 9 1}
    do_execsql_test row-case-index-group-$encoding.reverse {
        SELECT quote(k),bucket,count(*),min(id),
            max(CASE (SELECT a,b FROM kinds d WHERE d.id=i.route) WHEN (2,20) THEN 2 ELSE 1 END)
        FROM words i INDEXED BY words_key WHERE bucket>=1
            GROUP BY k,bucket ORDER BY k DESC,bucket ASC;
    } {'ā' 2 1 9 1 'Ā' 2 2 7 2 'b' 1 1 4 1 'B' 2 1 3 2 'A' 1 2 1 2 'a' 2 1 10 2 NULL 1 2 5 2}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_builtin_membership_collations_and_steps() {
    sqliters_tcltest::run_test_script(
        "row_case_builtin_membership_collations_and_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl

foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        INSERT INTO words VALUES(1,1,10),(2,2,20);
        CREATE TABLE items(id INTEGER PRIMARY KEY,ref INTEGER,label,region TEXT);
        INSERT INTO items VALUES(1,1,'Alpha','north '),(2,1,'ALPHA','south'),
            (3,1,'trail ','north '),(4,1,'trail','north'),(5,1,NULL,'north '),
            (6,2,'swap','west'),(7,1,'Beta','west'),(8,1,'trail'||char(160),'north'),
            (9,1,'A'||char(0)||'Z','east'),(10,1,'a'||char(0)||'x','east'),
            (11,9,NULL,NULL),(12,1,'A'||char(9),'east'),
            (13,1,char(256),'east'),(14,1,char(257),'east'),(15,1,X'416C706861','east');
        CREATE VIEW chosen AS
          SELECT i.id,CASE (SELECT a,b FROM words w WHERE w.id=i.ref)
             WHEN (2,20) THEN 'Beta  ' ELSE i.label END AS token,i.region
          FROM items i;
        CREATE TABLE probes(id INTEGER PRIMARY KEY,token TEXT,region TEXT);
        INSERT INTO probes VALUES(1,'alpha','north'),(2,'ALPHA','south'),
            (3,'alpha','NORTH'),(4,NULL,'north'),(5,'absent','north');
    }
    do_execsql_test builtin-row-case-in-$encoding.scalar {
        SELECT 'alpha' COLLATE NOCASE IN (SELECT token FROM chosen WHERE id<=2),
               'absent' COLLATE NOCASE NOT IN (SELECT token FROM chosen WHERE id<=2),
               'Beta' COLLATE RTRIM IN (SELECT token FROM chosen WHERE id=6),
               ('trail'||char(32)) COLLATE RTRIM IN (SELECT token FROM chosen WHERE id=4),
               ('trail'||char(160)) COLLATE RTRIM IN (SELECT token FROM chosen WHERE id=4),
               char(257) COLLATE NOCASE IN (SELECT token FROM chosen WHERE id=13),
               ('A'||char(0)||'u') COLLATE NOCASE IN (SELECT token FROM chosen WHERE id=9),
               X'416C706861' COLLATE NOCASE IN (SELECT token FROM chosen WHERE id<=2);
    } {1 1 1 1 0 0 1 0}
    do_execsql_test builtin-row-case-in-$encoding.row-mixed {
        SELECT ('alpha' COLLATE NOCASE,'north' COLLATE RTRIM) IN
                   (SELECT token,region FROM chosen WHERE id<=4),
               ('ALPHA' COLLATE BINARY,'north' COLLATE RTRIM) IN
                   (SELECT token,region FROM chosen WHERE id<=4),
               ('Beta' COLLATE RTRIM,'WEST' COLLATE NOCASE) IN
                   (SELECT token,region FROM chosen WHERE id BETWEEN 6 AND 7),
               ('beta' COLLATE RTRIM,'west' COLLATE NOCASE) NOT IN
                   (SELECT token,region FROM chosen WHERE id BETWEEN 6 AND 7);
    } {1 0 1 1}
    do_execsql_test builtin-row-case-in-$encoding.null {
        SELECT 'missing' COLLATE NOCASE IN (SELECT token FROM chosen WHERE id<=5),
               'missing' COLLATE RTRIM NOT IN (SELECT token FROM chosen WHERE id<=5),
               ('missing' COLLATE NOCASE,'north' COLLATE RTRIM) IN
                   (SELECT token,region FROM chosen WHERE id<=5),
               ('missing' COLLATE NOCASE,'west' COLLATE RTRIM) NOT IN
                   (SELECT token,region FROM chosen WHERE id<=5),
               (NULL COLLATE NOCASE,'north' COLLATE RTRIM) IN
                   (SELECT token,region FROM chosen WHERE id<0),
               (NULL COLLATE NOCASE,'north' COLLATE RTRIM) NOT IN
                   (SELECT token,region FROM chosen WHERE id<0);
    } {{} {} {} 1 0 1}
    do_test builtin-row-case-in-$encoding.cached {
        set s [sqlite3_prepare_v2 db {
            SELECT p.id,(p.token COLLATE NOCASE,p.region COLLATE RTRIM) IN
              (SELECT token,region FROM chosen WHERE id<=?1) FROM probes p ORDER BY p.id
        } -1]
        sqlite3_bind_int $s 1 2
        set before {}
        while {[set rc [sqlite3_step $s]] eq "SQLITE_ROW"} {
            lappend before [sqlite3_column_int $s 0] [sqlite3_column_text $s 1]
        }
        lappend before $rc
        sqlite3_reset $s
        execsql {CREATE INDEX items_ref ON items(ref);}
        sqlite3_bind_int $s 1 1
        set after {}
        while {[set rc [sqlite3_step $s]] eq "SQLITE_ROW"} {
            lappend after [sqlite3_column_int $s 0] [sqlite3_column_text $s 1]
        }
        lappend after $rc
        list $before $after [sqlite3_finalize $s]
    } {{1 1 2 1 3 0 4 {} 5 0 SQLITE_DONE} {1 1 2 0 3 0 4 {} 5 0 SQLITE_DONE} SQLITE_OK}
}
finish_test

"#,
    );
}

#[test]
fn test_row_case_recursive_fifo_builtin_collations() {
    sqliters_tcltest::run_test_script(
        "row_case_recursive_fifo_builtin_collations",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE pairings(id INTEGER PRIMARY KEY,a,b);
        INSERT INTO pairings VALUES (1,1,10),(2,2,20);
        CREATE TABLE items(id INTEGER PRIMARY KEY,ref INTEGER,token TEXT);
        INSERT INTO items VALUES (1,1,'A'),(2,1,'a'),(3,1,'A  '),(4,1,'b'),
          (5,1,'b '),(6,1,NULL),(7,1,'Ā'),(8,1,'ā'),(9,2,'unused');
        CREATE VIEW rendered AS SELECT i.id,CASE
           (SELECT a,b FROM pairings WHERE id=i.ref) WHEN (2,20) THEN 'A' ELSE i.token END AS token
           FROM items i;
    }
    do_execsql_test row-case-recursive-builtin-$encoding.nocase {
        WITH RECURSIVE visit(value,level) AS (
            SELECT token COLLATE NOCASE,1 FROM rendered
            UNION SELECT value,level+1 FROM visit WHERE level<2
        ) SELECT quote(value),level FROM visit;
    } {'A' 1 {'A  '} 1 'b' 1 {'b '} 1 NULL 1 'Ā' 1 'ā' 1 'A' 2 {'A  '} 2 'b' 2 {'b '} 2 NULL 2 'Ā' 2 'ā' 2}
    do_execsql_test row-case-recursive-builtin-$encoding.rtrim {
        WITH RECURSIVE visit(value,level) AS (
            SELECT token COLLATE RTRIM,1 FROM rendered
            UNION SELECT value,level+1 FROM visit WHERE level<2
        ) SELECT quote(value),level FROM visit;
    } {'A' 1 'a' 1 'b' 1 NULL 1 'Ā' 1 'ā' 1 'A' 2 'a' 2 'b' 2 NULL 2 'Ā' 2 'ā' 2}
    do_execsql_test row-case-recursive-builtin-$encoding.mixed {
        WITH RECURSIVE visit(value,suffix,level) AS (
            SELECT token COLLATE NOCASE,(CASE WHEN id<5 THEN 'x ' ELSE 'x' END) COLLATE RTRIM,1
              FROM rendered
            UNION SELECT value,suffix,level+1 FROM visit WHERE level<2
        ) SELECT quote(value),quote(suffix),level FROM visit;
    } {'A' {'x '} 1 {'A  '} {'x '} 1 'b' {'x '} 1 {'b '} 'x' 1 NULL 'x' 1 'Ā' 'x' 1 'ā' 'x' 1 'A' {'x '} 2 {'A  '} {'x '} 2 'b' {'x '} 2 {'b '} 'x' 2 NULL 'x' 2 'Ā' 'x' 2 'ā' 'x' 2}
    do_test row-case-recursive-builtin-$encoding.reset {
        set stmt [sqlite3_prepare_v2 db {
            WITH RECURSIVE visit(value,level) AS (
                SELECT token COLLATE NOCASE,1 FROM rendered WHERE id<=?1
                UNION SELECT value,level+1 FROM visit WHERE level<2
            ) SELECT quote(value),level FROM visit;
        } -1]
        sqlite3_bind_int $stmt 1 2
        set first [list [sqlite3_step $stmt] [sqlite3_column_text $stmt 0] [sqlite3_column_int $stmt 1]]
        set second [list [sqlite3_step $stmt] [sqlite3_column_text $stmt 0] [sqlite3_column_int $stmt 1]]
        set done [sqlite3_step $stmt]
        sqlite3_reset $stmt
        sqlite3_bind_int $stmt 1 4
        set again [list [sqlite3_step $stmt] [sqlite3_column_text $stmt 0] [sqlite3_column_int $stmt 1]]
        list $first $second $done $again [sqlite3_finalize $stmt]
    } {{SQLITE_ROW 'A' 1} {SQLITE_ROW 'A' 2} SQLITE_DONE {SQLITE_ROW 'A' 1} SQLITE_OK}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_builtin_partial_order_steps_and_limit() {
    sqliters_tcltest::run_test_script(
        "row_case_builtin_partial_order_steps_and_limit",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc mark_partial {x} {
    lappend ::partial_calls $x
    return [expr {int($x)}]
}
proc collect_partial_steps {s} {
    set rows {}
    while {1} {
        set ::partial_calls {}
        set code [sqlite3_step $s]
        if {$code ne "SQLITE_ROW"} {
            lappend rows [list $code $::partial_calls]
            return $rows
        }
        lappend rows [list [sqlite3_column_int $s 0] $::partial_calls]
    }
}
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    db function mark_partial -argcount 1 mark_partial
    execsql {
        CREATE TABLE picks(id INTEGER PRIMARY KEY,prefix TEXT,label TEXT);
        CREATE INDEX picks_prefix ON picks(prefix COLLATE NOCASE);
        INSERT INTO picks VALUES(1,'A','alpha '),(2,'a','z'),(3,'a',NULL),
            (4,'b','b  '),(5,'B','a'),(6,'b','b'),(7,'C','Y'),(8,'c','X'),(9,'C','Y  '),
            (10,NULL,'c'),(11,NULL,NULL);
    }
    set sql {
        SELECT i.id FROM picks i INDEXED BY picks_prefix
        ORDER BY i.prefix COLLATE NOCASE,
                 (CASE (SELECT 1,mark_partial(i.id)) WHEN (1,i.id) THEN i.label END) COLLATE RTRIM DESC NULLS FIRST,
                 i.id
    }
    do_test row-case-partial-$encoding.full {
        set s [sqlite3_prepare_v2 db $sql -1]
        list [sqlite3_column_name $s 0] [collect_partial_steps $s] [sqlite3_finalize $s]
    } {id {{11 {10 11 1}} {10 {}} {3 {2 3 4}} {2 {}} {1 {}} {4 {5 6 7}} {6 {}} {5 {}} {7 {8 9}} {9 {}} {8 {}} {SQLITE_DONE {}}} SQLITE_OK}
    append sql " LIMIT ?1 OFFSET ?2"
    do_test row-case-partial-$encoding.limit-reset-reprepare {
        set s [sqlite3_prepare_v2 db $sql -1]
        sqlite3_bind_int $s 1 3
        sqlite3_bind_int $s 2 1
        set first [collect_partial_steps $s]
        set reset [sqlite3_reset $s]
        execsql {CREATE TABLE reprepare(id INTEGER PRIMARY KEY);}
        sqlite3_bind_int $s 1 2
        sqlite3_bind_int $s 2 4
        set second [collect_partial_steps $s]
        list $first $reset $second [sqlite3_finalize $s]
    } {{{10 {10 11 1}} {3 {2 3 4}} {2 {}} {SQLITE_DONE {}}} SQLITE_OK {{1 {10 11 1 2 3 4}} {4 {5 6 7}} {SQLITE_DONE {}}} SQLITE_OK}
    do_test row-case-partial-$encoding.zero {
        set s [sqlite3_prepare_v2 db $sql -1]
        sqlite3_bind_int $s 1 0
        sqlite3_bind_int $s 2 1
        list [collect_partial_steps $s] [sqlite3_finalize $s]
    } {{{SQLITE_DONE {}}} SQLITE_OK}
    do_execsql_test row-case-partial-$encoding.payload {
        SELECT i.id,i.label FROM picks i INDEXED BY picks_prefix
        ORDER BY i.prefix COLLATE NOCASE,
                 (CASE (SELECT 1,mark_partial(i.id)) WHEN (1,i.id) THEN i.label END) COLLATE RTRIM DESC NULLS FIRST,
                 i.id LIMIT 4 OFFSET 2;
    } {3 {} 2 z 1 {alpha } 4 {b  }}
}
finish_test
"#,
    );
}

#[test]
fn test_row_case_literal_rowid_list_steps() {
    sqliters_tcltest::run_test_script(
        "row_case_literal_rowid_list_steps",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl

foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE words(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER);
        INSERT INTO words VALUES(1,1,10),(2,2,20);
        CREATE TABLE picks(id INTEGER PRIMARY KEY,ref INTEGER,label TEXT);
        INSERT INTO picks VALUES(-9223372036854775808,1,'min'),(-1,1,'minus'),
           (0,1,'zero'),(2,1,'two'),(3,2,'unused'),(4,1,'four'),(8,1,'eight'),
           (9223372036854775807,1,'max');
        CREATE VIEW chosen AS
          SELECT p.id,CASE (SELECT a,b FROM words w WHERE w.id=p.ref)
             WHEN (2,20) THEN 'Three ' ELSE p.label END AS token
          FROM picks p;
    }
    do_execsql_test literal-rowid-row-case-$encoding.mixed {
        SELECT id,token FROM chosen
          WHERE id IN (2,3,3.0,'003','3e0','  4 ',NULL,3.75,'0x0',X'30',-1)
          ORDER BY id;
    } {-1 minus 2 two 3 {Three } 4 four}
    do_execsql_test literal-rowid-row-case-$encoding.reverse {
        SELECT id,token FROM chosen WHERE id IN (2,3.0,3,'4',NULL,-1) ORDER BY id DESC;
    } {4 four 3 {Three } 2 two -1 minus}
    do_execsql_test literal-rowid-row-case-$encoding.limits {
        SELECT token FROM chosen WHERE id IN (-9223372036854775808,
           '9223372036854775807',9223372036854775807,9223372036854775808.0,NULL)
           ORDER BY id;
    } {min max}
    do_execsql_test literal-rowid-row-case-$encoding.null-empty {
        SELECT 'missing' COLLATE NOCASE IN
          (SELECT token FROM chosen WHERE id IN (NULL,NULL,'bad',X'30',3.75)),
          NULL COLLATE RTRIM NOT IN (SELECT token FROM chosen WHERE id IN ());
    } {0 1}
    do_execsql_test literal-rowid-row-case-$encoding.nested {
        SELECT ('Three' COLLATE RTRIM,'THREE ' COLLATE NOCASE) IN
          (SELECT token,token FROM chosen WHERE id IN (2,3,'003',NULL)),
          'THREE ' COLLATE NOCASE NOT IN
          (SELECT token FROM chosen WHERE id IN (2,3.0,'3e0',NULL));
    } {1 0}
    do_test literal-rowid-row-case-$encoding.reprepare {
        set s [sqlite3_prepare_v2 db {
            SELECT id,token FROM chosen WHERE id IN (?1,?2,3,NULL) ORDER BY id
        } -1]
        sqlite3_bind_text $s 1 004 -1
        sqlite3_bind_double $s 2 2.0
        set before {}
        while {[set rc [sqlite3_step $s]] eq "SQLITE_ROW"} {
            lappend before [sqlite3_column_int $s 0] [sqlite3_column_text $s 1]
        }
        lappend before $rc
        sqlite3_reset $s
        execsql {CREATE INDEX picks_ref ON picks(ref);}
        sqlite3_bind_null $s 1
        sqlite3_bind_int $s 2 8
        set after {}
        while {[set rc [sqlite3_step $s]] eq "SQLITE_ROW"} {
            lappend after [sqlite3_column_int $s 0] [sqlite3_column_text $s 1]
        }
        lappend after $rc
        list $before $after [sqlite3_finalize $s]
    } {{2 two 3 {Three } 4 four SQLITE_DONE} {3 {Three } 8 eight SQLITE_DONE} SQLITE_OK}
    do_execsql_test literal-rowid-row-case-$encoding.changes {
        SELECT changes(),total_changes();
    } {8 10}
}
finish_test

"#,
    );
}

#[test]
fn test_row_case_recursive_priority_queue() {
    sqliters_tcltest::run_test_script(
        "row_case_recursive_priority_queue",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
foreach encoding {UTF-8 UTF-16le UTF-16be} {
    reset_db
    execsql "PRAGMA encoding='$encoding';"
    execsql {
        CREATE TABLE pairings(id INTEGER PRIMARY KEY,a,b);
        INSERT INTO pairings VALUES (1,1,10),(2,2,20);
        CREATE TABLE items(id INTEGER PRIMARY KEY,ref INTEGER,token TEXT);
        INSERT INTO items VALUES (1,1,'A'),(2,1,'a'),(3,1,'A  '),(4,1,'b'),
          (5,1,'b '),(6,1,NULL),(7,1,'Ā'),(8,1,'ā'),(9,2,'unused');
        CREATE VIEW rendered AS SELECT i.id,CASE
           (SELECT a,b FROM pairings WHERE id=i.ref) WHEN (2,20) THEN 'A' ELSE i.token END AS token
           FROM items i;
    }
    do_execsql_test row-case-recursive-priority-$encoding.all {
        WITH RECURSIVE visits(id,token) AS (
            SELECT id,token FROM rendered WHERE id BETWEEN 1 AND 2
            UNION ALL SELECT id+2,token FROM visits WHERE id<4
            ORDER BY 1 DESC
        ) SELECT id,quote(token) FROM visits;
    } {2 'a' 4 'a' 1 'A' 3 'A' 5 'A'}
    do_execsql_test row-case-recursive-priority-$encoding.distinct {
        WITH RECURSIVE visits(token,depth) AS (
            SELECT token COLLATE NOCASE,1 FROM rendered WHERE id<=5
            UNION SELECT token,depth+1 FROM visits WHERE depth<2
            ORDER BY 1 COLLATE NOCASE DESC NULLS FIRST,2 DESC
        ) SELECT quote(token),depth FROM visits;
    } {{'b '} 1 {'b '} 2 'b' 1 'b' 2 {'A  '} 1 {'A  '} 2 'A' 1 'A' 2}
    do_execsql_test row-case-recursive-priority-$encoding.limits {
        WITH RECURSIVE visits(token,depth) AS (
            SELECT token COLLATE RTRIM,1 FROM rendered WHERE id>=3
            UNION SELECT token,depth+1 FROM visits WHERE depth<2
            ORDER BY 1 COLLATE RTRIM DESC NULLS LAST,2 ASC LIMIT 7 OFFSET 2
        ) SELECT quote(token),depth FROM visits;
    } {'Ā' 1 'Ā' 2 'b' 1 'b' 2 {'A  '} 1 {'A  '} 2 NULL 1}
    do_test row-case-recursive-priority-$encoding.reset {
        set stmt [sqlite3_prepare_v2 db {
            WITH RECURSIVE visits(id,token) AS (
                SELECT id,token FROM rendered WHERE id BETWEEN 1 AND 2
                UNION SELECT id+2,token FROM visits WHERE id<4
                ORDER BY 1 DESC LIMIT ?1 OFFSET ?2
            ) SELECT id,quote(token) FROM visits;
        } -1]
        sqlite3_bind_int $stmt 1 2
        sqlite3_bind_int $stmt 2 1
        set first [list [sqlite3_step $stmt] [sqlite3_column_int $stmt 0] [sqlite3_column_text $stmt 1]]
        set second [list [sqlite3_step $stmt] [sqlite3_column_int $stmt 0] [sqlite3_column_text $stmt 1]]
        set done [sqlite3_step $stmt]
        sqlite3_reset $stmt
        sqlite3_bind_int $stmt 1 1
        sqlite3_bind_int $stmt 2 3
        set again [list [sqlite3_step $stmt] [sqlite3_column_int $stmt 0] [sqlite3_column_text $stmt 1]]
        list $first $second $done $again [sqlite3_step $stmt] [sqlite3_finalize $stmt]
    } {{SQLITE_ROW 4 'a'} {SQLITE_ROW 1 'A'} SQLITE_DONE {SQLITE_ROW 3 'A'} SQLITE_DONE SQLITE_OK}
}
finish_test
"#,
    );
}
