#![forbid(unsafe_code)]

#[test]
fn test_between() {
    sqliters_tcltest::run_test_script(
        "between",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test between-1.0 {
  execsql {
    BEGIN;
    CREATE TABLE t1(w int, x int, y int, z int);
  }
  for {set i 1} {$i<=100} {incr i} {
    set w $i
    set x [expr {int(log($i)/log(2))}]
    set y [expr {$i*$i + 2*$i + 1}]
    set z [expr {$x+$y}]
    ifcapable tclvar {
      execsql {INSERT INTO t1 VALUES($::w,$::x,$::y,$::z)}
    } else {
      execsql {INSERT INTO t1 VALUES(:w,:x,:y,:z)}
    }
  }
  execsql {
    CREATE UNIQUE INDEX i1w ON t1(w);
    CREATE INDEX i1xy ON t1(x,y);
    CREATE INDEX i1zyx ON t1(z,y,x);
    COMMIT;
  }
} {}
proc queryplan {sql} {
  set ::sqlite_sort_count 0
  set data [execsql $sql]
  if {$::sqlite_sort_count} {set x sort} {set x nosort}
  lappend data $x
  set eqp [execsql "EXPLAIN QUERY PLAN $sql"]
  foreach {a b c x} $eqp {
    if {[regexp {(SCAN|SEARCH) (\w+ AS )?(\w+) USING.* INDEX (\w+)\y} \
        $x all ss as tab idx]} {
      lappend data $tab $idx
    } elseif {[regexp {(SCAN|SEARCH) (\w+ AS )?(\w+)\y} $x all ss as tab]} {
      lappend data $tab *
    }
  }
  return $data   
}
do_test between-1.1.1 {
  queryplan {
    SELECT * FROM t1 WHERE w BETWEEN 5 AND 6 ORDER BY +w
  }
} {5 2 36 38 6 2 49 51 sort t1 i1w}
do_test between-1.1.2 {
  queryplan {
    SELECT * FROM t1 WHERE +w BETWEEN 5 AND 6 ORDER BY +w
  }
} {5 2 36 38 6 2 49 51 sort t1 *}
do_test between-1.2.1 {
  queryplan {
    SELECT * FROM t1 WHERE w BETWEEN 5 AND 65-y ORDER BY +w
  }
} {5 2 36 38 6 2 49 51 sort t1 i1w}
do_test between-1.2.2 {
  queryplan {
    SELECT * FROM t1 WHERE +w BETWEEN 5 AND 65-y ORDER BY +w
  }
} {5 2 36 38 6 2 49 51 sort t1 *}
do_test between-1.3.1 {
  queryplan {
    SELECT * FROM t1 WHERE w BETWEEN 41-y AND 6 ORDER BY +w
  }
} {5 2 36 38 6 2 49 51 sort t1 i1w}
do_test between-1.3.2 {
  queryplan {
    SELECT * FROM t1 WHERE +w BETWEEN 41-y AND 6 ORDER BY +w
  }
} {5 2 36 38 6 2 49 51 sort t1 *}
do_test between-1.4 {
  queryplan {
    SELECT * FROM t1 WHERE w BETWEEN 41-y AND 65-y ORDER BY +w
  }
} {5 2 36 38 6 2 49 51 sort t1 *}
do_test between-1.5.1 {
  queryplan {
    SELECT * FROM t1 WHERE 26 BETWEEN y AND z ORDER BY +w
  }
} {4 2 25 27 sort t1 i1zyx}
do_test between-1.5.2 {
  queryplan {
    SELECT * FROM t1 WHERE 26 BETWEEN +y AND z ORDER BY +w
  }
} {4 2 25 27 sort t1 i1zyx}
do_test between-1.5.3 {
  queryplan {
    SELECT * FROM t1 WHERE 26 BETWEEN y AND +z ORDER BY +w
  }
} {4 2 25 27 sort t1 *}
reset_db
do_execsql_test between-2.0 {
  CREATE TABLE t1(x TEXT, y TEXT COLLATE nocase);
  INSERT INTO t1 VALUES('0', 'abc');
}
foreach {tn expr res} {
  1 "x                BETWEEN 1 AND '5'" 0
  2 "x COLLATE binary BETWEEN 1 AND '5'" 0
  3 "x COLLATE nocase BETWEEN 1 AND '5'" 0

  4 "y                  BETWEEN 'A' AND 'B'" 1
  5 "y COLLATE nocase   BETWEEN 'A' AND 'B'" 1
  6 "y COLLATE binary   BETWEEN 'A' AND 'B'" 0
  7 "(y COLLATE binary) BETWEEN 'A' AND 'B'" 0
} {
  set sql "SELECT $expr FROM t1"
  do_execsql_test between-2.1.$tn $sql $res
}
reset_db
do_execsql_test between-3.0 {
  CREATE TABLE t1(x, y);
  CREATE INDEX i1 ON t1(x);
  INSERT INTO t1 VALUES(4, 4);
  CREATE TABLE t2(a, b);
}
do_execsql_test between-3.1 {
  SELECT * FROM t1 LEFT JOIN t2 ON (x BETWEEN 1 AND 3);
} {4 4 {} {}}
do_execsql_test between-3.2 {
  SELECT * FROM t1 LEFT JOIN t2 ON (x BETWEEN 5 AND 7);
} {4 4 {} {}}
finish_test
"#,
    );
}
