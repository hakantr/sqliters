#[test]
#[ignore = "34824/36716 subtests passed; pending: VIEW, EQP/optimizer"]
fn test_select9() {
    sqliters_tcltest::run_test_script(
        "select9",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc test_compound_select {testname sql result} {

  set nCol 1
  db eval $sql A {
    set nCol [llength $A(*)]
    break
  }
  set nRow [expr {[llength $result] / $nCol}]

  set ::compound_sql $sql
  do_test $testname { 
    execsql $::compound_sql
  } $result

  set iLimitIncr  1
  set iOffsetIncr 1
  if {[info exists ::G(isquick)] && $::G(isquick) && $nRow>=5} {
    set iOffsetIncr [expr $nRow / 5]
    set iLimitIncr [expr $nRow / 5]
  }

  set iLimitEnd   [expr $nRow+$iLimitIncr]
  set iOffsetEnd  [expr $nRow+$iOffsetIncr]

  for {set iOffset 0} {$iOffset < $iOffsetEnd} {incr iOffset $iOffsetIncr} {
    for {set iLimit 0} {$iLimit < $iLimitEnd} {incr iLimit} {
  
      set ::compound_sql "$sql LIMIT $iLimit"
      if {$iOffset != 0} {
        append ::compound_sql " OFFSET $iOffset"
      }
  
      set iStart [expr {$iOffset*$nCol}]
      set iEnd [expr {($iOffset*$nCol) + ($iLimit*$nCol) -1}]
  
      do_test $testname.limit=$iLimit.offset=$iOffset { 
        execsql $::compound_sql
      } [lrange $result $iStart $iEnd]
    }
  }
}
proc test_compound_select_flippable {testname sql result} {
  test_compound_select $testname $sql $result

  set select [string trim $sql]
  set RE {(.*)(UNION ALL|INTERSECT|UNION)(.*)(ORDER BY.*)}
  set rc [regexp $RE $select -> s1 op s2 order_by]
  if {!$rc} {error "Statement is unflippable: $select"}

  set flipsql "$s2 $op $s1 $order_by"
  test_compound_select $testname.flipped $flipsql $result
}
do_test select9-1.0 {
  execsql {
    CREATE TABLE t1(a, b, c);
    CREATE TABLE t2(d, e, f);
    BEGIN;
      INSERT INTO t1 VALUES(1,  'one',   'I');
      INSERT INTO t1 VALUES(3,  NULL,    NULL);
      INSERT INTO t1 VALUES(5,  'five',  'V');
      INSERT INTO t1 VALUES(7,  'seven', 'VII');
      INSERT INTO t1 VALUES(9,  NULL,    NULL);
      INSERT INTO t1 VALUES(2,  'two',   'II');
      INSERT INTO t1 VALUES(4,  'four',  'IV');
      INSERT INTO t1 VALUES(6,  NULL,    NULL);
      INSERT INTO t1 VALUES(8,  'eight', 'VIII');
      INSERT INTO t1 VALUES(10, 'ten',   'X');

      INSERT INTO t2 VALUES(1,  'two',      'IV');
      INSERT INTO t2 VALUES(2,  'four',     'VIII');
      INSERT INTO t2 VALUES(3,  NULL,       NULL);
      INSERT INTO t2 VALUES(4,  'eight',    'XVI');
      INSERT INTO t2 VALUES(5,  'ten',      'XX');
      INSERT INTO t2 VALUES(6,  NULL,       NULL);
      INSERT INTO t2 VALUES(7,  'fourteen', 'XXVIII');
      INSERT INTO t2 VALUES(8,  'sixteen',  'XXXII');
      INSERT INTO t2 VALUES(9,  NULL,       NULL);
      INSERT INTO t2 VALUES(10, 'twenty',   'XL');

    COMMIT;
  }
} {}
set iOuterLoop 1
foreach indexes [list {
} {
  CREATE INDEX i1 ON t1(a)
} {
  CREATE INDEX i2 ON t1(b)
} {
  CREATE INDEX i3 ON t2(d)
} {
  CREATE INDEX i4 ON t2(e)
}] {

  do_test select9-1.$iOuterLoop.1 {
    execsql $indexes
  } {}

  test_compound_select select9-1.$iOuterLoop.2 {
    SELECT a, b FROM t1 UNION ALL SELECT d, e FROM t2 
  } {1 one 3 {} 5 five 7 seven 9 {} 2 two 4 four 6 {} 8 eight 10 ten 1 two 2 four 3 {} 4 eight 5 ten 6 {} 7 fourteen 8 sixteen 9 {} 10 twenty}
  test_compound_select select9-1.$iOuterLoop.3 {
    SELECT a, b FROM t1 UNION ALL SELECT d, e FROM t2 ORDER BY 1 
  } {1 one 1 two 2 two 2 four 3 {} 3 {} 4 four 4 eight 5 five 5 ten 6 {} 6 {} 7 seven 7 fourteen 8 eight 8 sixteen 9 {} 9 {} 10 ten 10 twenty}
  test_compound_select select9-1.$iOuterLoop.4 {
    SELECT a, b FROM t1 UNION ALL SELECT d, e FROM t2 ORDER BY 2 
  } {3 {} 9 {} 6 {} 3 {} 6 {} 9 {} 8 eight 4 eight 5 five 4 four 2 four 7 fourteen 1 one 7 seven 8 sixteen 10 ten 5 ten 10 twenty 2 two 1 two}
  test_compound_select_flippable select9-1.$iOuterLoop.5 {
    SELECT a, b FROM t1 UNION ALL SELECT d, e FROM t2 ORDER BY 1, 2
  } {1 one 1 two 2 four 2 two 3 {} 3 {} 4 eight 4 four 5 five 5 ten 6 {} 6 {} 7 fourteen 7 seven 8 eight 8 sixteen 9 {} 9 {} 10 ten 10 twenty}
  test_compound_select_flippable select9-1.$iOuterLoop.6 {
    SELECT a, b FROM t1 UNION ALL SELECT d, e FROM t2 ORDER BY 2, 1
  } {3 {} 3 {} 6 {} 6 {} 9 {} 9 {} 4 eight 8 eight 5 five 2 four 4 four 7 fourteen 1 one 7 seven 8 sixteen 5 ten 10 ten 10 twenty 1 two 2 two}

  test_compound_select select9-1.$iOuterLoop.7 {
    SELECT a, b FROM t1 UNION SELECT d, e FROM t2 
  } {1 one 1 two 2 four 2 two 3 {} 4 eight 4 four 5 five 5 ten 6 {} 7 fourteen 7 seven 8 eight 8 sixteen 9 {} 10 ten 10 twenty}

  test_compound_select select9-1.$iOuterLoop.8 {
    SELECT a, b FROM t1 UNION SELECT d, e FROM t2 ORDER BY 1 
  } {1 one 1 two 2 four 2 two 3 {} 4 eight 4 four 5 five 5 ten 6 {} 7 fourteen 7 seven 8 eight 8 sixteen 9 {} 10 ten 10 twenty}

  test_compound_select select9-1.$iOuterLoop.9 {
    SELECT a, b FROM t1 UNION SELECT d, e FROM t2 ORDER BY 2 
  } {3 {} 6 {} 9 {} 4 eight 8 eight 5 five 2 four 4 four 7 fourteen 1 one 7 seven 8 sixteen 5 ten 10 ten 10 twenty 1 two 2 two}

  test_compound_select_flippable select9-1.$iOuterLoop.10 {
    SELECT a, b FROM t1 UNION SELECT d, e FROM t2 ORDER BY 1, 2
  } {1 one 1 two 2 four 2 two 3 {} 4 eight 4 four 5 five 5 ten 6 {} 7 fourteen 7 seven 8 eight 8 sixteen 9 {} 10 ten 10 twenty}

  test_compound_select_flippable select9-1.$iOuterLoop.11 {
    SELECT a, b FROM t1 UNION SELECT d, e FROM t2 ORDER BY 2, 1
  } {3 {} 6 {} 9 {} 4 eight 8 eight 5 five 2 four 4 four 7 fourteen 1 one 7 seven 8 sixteen 5 ten 10 ten 10 twenty 1 two 2 two}

  test_compound_select select9-1.$iOuterLoop.11 {
    SELECT a, b FROM t1 INTERSECT SELECT d, e FROM t2 
  } {3 {} 6 {} 9 {}}
  test_compound_select_flippable select9-1.$iOuterLoop.12 {
    SELECT a, b FROM t1 INTERSECT SELECT d, e FROM t2 ORDER BY 1
  } {3 {} 6 {} 9 {}}
  test_compound_select select9-1.$iOuterLoop.13 {
    SELECT a, b FROM t1 INTERSECT SELECT d, e FROM t2 ORDER BY 2
  } {3 {} 6 {} 9 {}}
  test_compound_select_flippable select9-1.$iOuterLoop.14 {
    SELECT a, b FROM t1 INTERSECT SELECT d, e FROM t2 ORDER BY 2, 1
  } {3 {} 6 {} 9 {}}
  test_compound_select_flippable select9-1.$iOuterLoop.15 {
    SELECT a, b FROM t1 INTERSECT SELECT d, e FROM t2 ORDER BY 1, 2
  } {3 {} 6 {} 9 {}}

  test_compound_select select9-1.$iOuterLoop.16 {
    SELECT a, b FROM t1 EXCEPT SELECT d, e FROM t2 
  } {1 one 2 two 4 four 5 five 7 seven 8 eight 10 ten}

  test_compound_select select9-1.$iOuterLoop.17 {
    SELECT a, b FROM t1 EXCEPT SELECT d, e FROM t2 ORDER BY 1 
  } {1 one 2 two 4 four 5 five 7 seven 8 eight 10 ten}

  test_compound_select select9-1.$iOuterLoop.18 {
    SELECT a, b FROM t1 EXCEPT SELECT d, e FROM t2 ORDER BY 2 
  } {8 eight 5 five 4 four 1 one 7 seven 10 ten 2 two}

  test_compound_select select9-1.$iOuterLoop.19 {
    SELECT a, b FROM t1 EXCEPT SELECT d, e FROM t2 ORDER BY 1, 2
  } {1 one 2 two 4 four 5 five 7 seven 8 eight 10 ten}

  test_compound_select select9-1.$iOuterLoop.20 {
    SELECT a, b FROM t1 EXCEPT SELECT d, e FROM t2 ORDER BY 2, 1
  } {8 eight 5 five 4 four 1 one 7 seven 10 ten 2 two}

  incr iOuterLoop
}
do_test select9-2.0 {
  execsql {
    DROP INDEX i1;
    DROP INDEX i2;
    DROP INDEX i3;
    DROP INDEX i4;
  }
} {}
proc reverse {lhs rhs} {
  return [string compare $rhs $lhs]
}
db collate reverse reverse
set iOuterLoop 1
foreach indexes [list {
} {
  CREATE INDEX i1 ON t1(a)
} {
  DROP INDEX i1;
  CREATE INDEX i1 ON t1(b, a)
} {
  CREATE INDEX i2 ON t2(d DESC, e COLLATE REVERSE ASC);
} {
  CREATE INDEX i3 ON t1(a DESC);
}] {
  do_test select9-2.$iOuterLoop.1 {
    execsql $indexes
  } {}

  test_compound_select_flippable select9-2.$iOuterLoop.2 {
    SELECT * FROM t1 WHERE a<5 UNION SELECT * FROM t2 WHERE d>=5 ORDER BY 1
  } {1 one I 2 two II 3 {} {} 4 four IV 5 ten XX 6 {} {} 7 fourteen XXVIII 8 sixteen XXXII 9 {} {} 10 twenty XL}

  test_compound_select_flippable select9-2.$iOuterLoop.2 {
    SELECT * FROM t1 WHERE a<5 UNION SELECT * FROM t2 WHERE d>=5 ORDER BY 2, 1
  } {3 {} {} 6 {} {} 9 {} {} 4 four IV 7 fourteen XXVIII 1 one I 8 sixteen XXXII 5 ten XX 10 twenty XL 2 two II}

  test_compound_select_flippable select9-2.$iOuterLoop.3 {
    SELECT * FROM t1 WHERE a<5 UNION SELECT * FROM t2 WHERE d>=5 
    ORDER BY 2 COLLATE reverse, 1
  } {3 {} {} 6 {} {} 9 {} {} 2 two II 10 twenty XL 5 ten XX 8 sixteen XXXII 1 one I 7 fourteen XXVIII 4 four IV}

  test_compound_select_flippable select9-2.$iOuterLoop.4 {
    SELECT * FROM t1 WHERE a<5 UNION ALL SELECT * FROM t2 WHERE d>=5 ORDER BY 1
  } {1 one I 2 two II 3 {} {} 4 four IV 5 ten XX 6 {} {} 7 fourteen XXVIII 8 sixteen XXXII 9 {} {} 10 twenty XL}

  test_compound_select_flippable select9-2.$iOuterLoop.5 {
    SELECT * FROM t1 WHERE a<5 UNION ALL SELECT * FROM t2 WHERE d>=5 ORDER BY 2, 1
  } {3 {} {} 6 {} {} 9 {} {} 4 four IV 7 fourteen XXVIII 1 one I 8 sixteen XXXII 5 ten XX 10 twenty XL 2 two II}

  test_compound_select_flippable select9-2.$iOuterLoop.6 {
    SELECT * FROM t1 WHERE a<5 UNION ALL SELECT * FROM t2 WHERE d>=5 
    ORDER BY 2 COLLATE reverse, 1
  } {3 {} {} 6 {} {} 9 {} {} 2 two II 10 twenty XL 5 ten XX 8 sixteen XXXII 1 one I 7 fourteen XXVIII 4 four IV}

  test_compound_select select9-2.$iOuterLoop.4 {
    SELECT a FROM t1 WHERE a<8 EXCEPT SELECT d FROM t2 WHERE d<=3 ORDER BY 1
  } {4 5 6 7}

  test_compound_select select9-2.$iOuterLoop.4 {
    SELECT a FROM t1 WHERE a<8 INTERSECT SELECT d FROM t2 WHERE d<=3 ORDER BY 1
  } {1 2 3}

}
do_test select9-2.X {
  execsql {
    DROP INDEX i1;
    DROP INDEX i2;
    DROP INDEX i3;
  }
} {}
proc cksort {sql} {
  set ::sqlite_sort_count 0
  set data [execsql $sql]
  if {$::sqlite_sort_count} {set x sort} {set x nosort}
  lappend data $x
  return $data
}
do_test select9-3.1 {
  cksort { SELECT a FROM t1 ORDER BY 1 }
} {1 2 3 4 5 6 7 8 9 10 sort}
do_test select9-3.2 {
  execsql { CREATE INDEX i1 ON t1(a) }
  cksort { SELECT a FROM t1 ORDER BY 1 }
} {1 2 3 4 5 6 7 8 9 10 nosort}
do_test select9-3.3 {
  cksort { SELECT a FROM t1 UNION ALL SELECT d FROM t2 ORDER BY 1 LIMIT 5 }
} {1 1 2 2 3 sort}
do_test select9-3.4 {
  execsql { CREATE INDEX i2 ON t2(d) }
  cksort { SELECT a FROM t1 UNION ALL SELECT d FROM t2 ORDER BY 1 LIMIT 5 }
} {1 1 2 2 3 nosort}
do_test select9-3.5 {
  execsql { CREATE VIEW v1 AS SELECT a FROM t1 UNION ALL SELECT d FROM t2 }
  cksort { SELECT a FROM v1 ORDER BY 1 LIMIT 5 }
} {1 1 2 2 3 nosort}
do_test select9-3.X {
  execsql {
    DROP INDEX i1;
    DROP INDEX i2;
    DROP VIEW v1;
  }
} {}
do_test select9-4.1 {
  cksort { SELECT a FROM t1 ORDER BY 1 }
} {1 2 3 4 5 6 7 8 9 10 sort}
do_test select9-4.2 {
  execsql { CREATE INDEX i1 ON t1(a) }
  cksort { SELECT a FROM t1 ORDER BY 1 }
} {1 2 3 4 5 6 7 8 9 10 nosort}
do_test select9-4.3 {
  cksort { SELECT a FROM t1 UNION SELECT d FROM t2 ORDER BY 1 LIMIT 5 }
} {1 2 3 4 5 sort}
do_test select9-4.4 {
  execsql { CREATE INDEX i2 ON t2(d) }
  cksort { SELECT a FROM t1 UNION SELECT d FROM t2 ORDER BY 1 LIMIT 5 }
} {1 2 3 4 5 nosort}
do_test select9-4.5 {
  execsql { CREATE VIEW v1 AS SELECT a FROM t1 UNION SELECT d FROM t2 }
  cksort { SELECT a FROM v1 ORDER BY 1 LIMIT 5 }
} {1 2 3 4 5 nosort}
do_test select9-4.X {
  execsql {
    DROP INDEX i1;
    DROP INDEX i2;
    DROP VIEW v1;
  }
} {}
do_test select9-5.1 {
  db eval {
    CREATE TABLE t51(x, y);
    CREATE TABLE t52(x, y);
    CREATE VIEW v5 as
       SELECT x, y FROM t51
       UNION ALL
       SELECT x, y FROM t52;
    CREATE INDEX t51x ON t51(x);
    CREATE INDEX t52x ON t52(x);
    EXPLAIN QUERY PLAN
       SELECT * FROM v5 WHERE x='12345' ORDER BY y;
  }
} {~/SCAN/}  ;# Uses indices with "*"
do_test select9-5.2 {
  db eval {
    EXPLAIN QUERY PLAN
       SELECT x, y FROM v5 WHERE x='12345' ORDER BY y;
  }
} {~/SCAN/}  ;# Uses indices with "x, y"
do_test select9-5.3 {
  db eval {
    EXPLAIN QUERY PLAN
       SELECT x, y FROM v5 WHERE +x='12345' ORDER BY y;
  }
} {/SCAN/}   ;# Full table scan if the "+x" prevents index usage.
do_execsql_test select9-6.1 {
  CREATE TABLE t61(a);
  CREATE TABLE t62(b);
  INSERT INTO t61 VALUES(111);
  INSERT INTO t62 VALUES(222);
  SELECT a FROM t61 WHERE 0 UNION SELECT b FROM t62;
} {222}
do_execsql_test select9-6.2 {
  SELECT a FROM t61 WHERE 0 UNION ALL SELECT b FROM t62;
} {222}
do_execsql_test select9-6.3 {
  SELECT a FROM t61 UNION SELECT b FROM t62 WHERE 0;
} {111}
finish_test
"#,
    );
}
