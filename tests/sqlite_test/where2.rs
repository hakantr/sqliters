#![forbid(unsafe_code)]

#[test]
#[ignore = "61/106 subtests passed; pending: EQP/optimizer"]
fn test_where2() {
    sqliters_tcltest::run_test_script(
        "where2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test where2-1.0 {
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
proc count sql {
  set ::sqlite_search_count 0
  return [concat [execsql $sql] $::sqlite_search_count]
}
proc cksort {sql} {
  set data [execsql $sql]
  if {[db status sort]} {set x sort} {set x nosort}
  lappend data $x
  return $data
}
proc queryplan {sql} {
  set ::sqlite_sort_count 0
  set data [execsql $sql]
  if {$::sqlite_sort_count} {set x sort} {set x nosort}
  lappend data $x
  set eqp [execsql "EXPLAIN QUERY PLAN $sql"]
  foreach {a b c x} $eqp {
    if {[regexp {SCAN CONSTANT} $x]} {
    } elseif {[regexp {(SCAN|SEARCH) (\w+ AS )?(\w+) USING.* INDEX (\w+)\y} \
        $x all ss as tab idx]} {
      lappend data $tab $idx
    } elseif {[regexp {(SCAN|SEARCH) (\w+ AS )?(\w+)\y} $x all ss as tab]} {
      lappend data $tab *
    }
  }
  return $data   
}
do_test where2-1.1 {
  queryplan {
    SELECT * FROM t1 WHERE w=85 AND x=6 AND y=7396
  }
} {85 6 7396 7402 nosort t1 i1w}
do_test where2-1.3 {
  queryplan {
    SELECT * FROM t1 WHERE w=85 AND x=6 AND y=7396 AND rowid=85
  }
} {85 6 7396 7402 nosort t1 *}
do_test where2-2.1 {
  queryplan {
    SELECT * FROM t1 WHERE w=85 ORDER BY random();
  }
} {85 6 7396 7402 nosort t1 i1w}
do_test where2-2.2 {
  queryplan {
    SELECT * FROM t1 WHERE x=6 AND y=7396 ORDER BY random();
  }
} {85 6 7396 7402 sort t1 i1xy}
do_test where2-2.3 {
  queryplan {
    SELECT * FROM t1 WHERE rowid=85 AND x=6 AND y=7396 ORDER BY random();
  }
} {85 6 7396 7402 nosort t1 *}
do_test where2-2.4 {
  db eval {
    CREATE TABLE x1(a INTEGER PRIMARY KEY, b DEFAULT 1);
    WITH RECURSIVE
       cnt(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM cnt WHERE x<50)
    INSERT INTO x1 SELECT x, 1 FROM cnt;
    CREATE TABLE x2(x INTEGER PRIMARY KEY);
    INSERT INTO x2 VALUES(1);
  }
  set sql {SELECT * FROM x1, x2 WHERE x=1 ORDER BY random()}
  set out1 [db eval $sql]
  set out2 [db eval $sql]
  set out3 [db eval $sql]
  expr {$out1!=$out2 && $out2!=$out3}
} {1}
do_execsql_test where2-2.5 {
  -- random() is not optimized out
  EXPLAIN SELECT * FROM x1, x2 WHERE x=1 ORDER BY random();
} {/ random/}
do_execsql_test where2-2.5b {
  -- random() is not optimized out
  EXPLAIN SELECT * FROM x1, x2 WHERE x=1 ORDER BY random();
} {/ SorterOpen /}
do_execsql_test where2-2.6 {
  -- other constant functions are optimized out
  EXPLAIN SELECT * FROM x1, x2 WHERE x=1 ORDER BY abs(5);
} {~/ abs/}
do_execsql_test where2-2.6b {
  -- other constant functions are optimized out
  EXPLAIN SELECT * FROM x1, x2 WHERE x=1 ORDER BY abs(5);
} {~/ SorterOpen /}
do_test where2-3.1 {
  queryplan {
    SELECT * FROM t1 ORDER BY rowid LIMIT 2
  }
} {1 0 4 4 2 1 9 10 nosort t1 *}
do_test where2-3.2 {
  queryplan {
    SELECT * FROM t1 ORDER BY rowid DESC LIMIT 2
  }
} {100 6 10201 10207 99 6 10000 10006 nosort t1 *}
ifcapable subquery {
  do_test where2-4.1 {
    queryplan {
      SELECT * FROM t1 WHERE z IN (10207,10006) AND y IN (10000,10201)
                       AND x>0 AND x<10
      ORDER BY w
    }
  } {99 6 10000 10006 100 6 10201 10207 sort t1 i1zyx}
  do_test where2-4.2 {
    queryplan {
      SELECT * FROM t1 WHERE z IN (10207,10006) AND y=10000
                       AND x>0 AND x<10
      ORDER BY w
    }
  } {99 6 10000 10006 sort t1 i1zyx}
  do_test where2-4.3 {
    queryplan {
      SELECT * FROM t1 WHERE z=10006 AND y IN (10000,10201)
                       AND x>0 AND x<10
      ORDER BY w
    }
  } {99 6 10000 10006 sort t1 i1zyx}
  ifcapable compound {
    do_test where2-4.4 {
      queryplan {
        SELECT * FROM t1 WHERE z IN (SELECT 10207 UNION SELECT 10006)
                         AND y IN (10000,10201)
                         AND x>0 AND x<10
        ORDER BY w
      }
    } {99 6 10000 10006 100 6 10201 10207 sort t1 i1zyx}
    do_test where2-4.5 {
      queryplan {
        SELECT * FROM t1 WHERE z IN (SELECT 10207 UNION SELECT 10006)
                         AND y IN (SELECT 10000 UNION SELECT 10201)
                         AND x>0 AND x<10
        ORDER BY w
      }
    } {99 6 10000 10006 100 6 10201 10207 sort t1 i1zyx}
  }
  do_test where2-4.6a {
    queryplan {
      SELECT * FROM t1
       WHERE x IN (1,2,3,4,5,6,7,8)
         AND y IN (10000,10001,10002,10003,10004,10005)
       ORDER BY x
    }
  } {99 6 10000 10006 nosort t1 i1xy}
  do_test where2-4.6b {
    queryplan {
      SELECT * FROM t1
       WHERE x IN (1,2,3,4,5,6,7,8)
         AND y IN (10000,10001,10002,10003,10004,10005)
       ORDER BY x DESC
    }
  } {99 6 10000 10006 nosort t1 i1xy}
  do_test where2-4.6c {
    queryplan {
      SELECT * FROM t1
       WHERE x IN (1,2,3,4,5,6,7,8)
         AND y IN (10000,10001,10002,10003,10004,10005)
       ORDER BY x, y
    }
  } {99 6 10000 10006 nosort t1 i1xy}
  do_test where2-4.6d {
    queryplan {
      SELECT * FROM t1
       WHERE x IN (1,2,3,4,5,6,7,8)
         AND y IN (10000,10001,10002,10003,10004,10005)
       ORDER BY x, y DESC
    }
  } {99 6 10000 10006 sort t1 i1xy}

  do_test where2-4.6x {
    queryplan {
      SELECT * FROM t1 WHERE z IN (10207,10006,10006,10207)
      ORDER BY w
    }
  } {99 6 10000 10006 100 6 10201 10207 sort t1 i1zyx}
  do_test where2-4.6y {
    queryplan {
      SELECT * FROM t1 WHERE z IN (10207,10006,10006,10207)
      ORDER BY w DESC
    }
  } {100 6 10201 10207 99 6 10000 10006 sort t1 i1zyx}
  ifcapable compound {
    do_test where2-4.7 {
      queryplan {
        SELECT * FROM t1 WHERE z IN (
           SELECT 10207 UNION ALL SELECT 10006
           UNION ALL SELECT 10006 UNION ALL SELECT 10207)
        ORDER BY w
      }
    } {99 6 10000 10006 100 6 10201 10207 sort t1 i1zyx}
  }

} ;# ifcapable subquery
do_test where2-5.1 {
  queryplan {
    SELECT * FROM t1 WHERE w=99 ORDER BY w
  }
} {99 6 10000 10006 nosort t1 i1w}
ifcapable subquery {
  do_test where2-5.2a {
    queryplan {
      SELECT * FROM t1 WHERE w IN (99) ORDER BY w
    }
  } {99 6 10000 10006 nosort t1 i1w}
  do_test where2-5.2b {
    queryplan {
      SELECT * FROM t1 WHERE w IN (99) ORDER BY w DESC
    }
  } {99 6 10000 10006 nosort t1 i1w}
}
set ::idx {}
ifcapable subquery {set ::idx i1w}
do_test where2-6.1.1 {
  queryplan {
    SELECT * FROM t1 WHERE w=99 OR w=100 ORDER BY +w
  }
} [list 99 6 10000 10006 100 6 10201 10207 sort t1 $::idx]
do_test where2-6.1.2 {
  queryplan {
    SELECT * FROM t1 WHERE 99=w OR 100=w ORDER BY +w
  }
} [list 99 6 10000 10006 100 6 10201 10207 sort t1 $::idx]
do_test where2-6.2 {
  queryplan {
    SELECT * FROM t1 WHERE w=99 OR w=100 OR 6=w ORDER BY +w
  }
} [list 6 2 49 51 99 6 10000 10006 100 6 10201 10207 sort t1 $::idx]
do_test where2-6.3 {
  queryplan {
    SELECT * FROM t1 WHERE w=99 OR w=100 OR 6=+w ORDER BY +w
  }
} {6 2 49 51 99 6 10000 10006 100 6 10201 10207 sort t1 *}
do_test where2-6.4 {
  queryplan {
    SELECT *, '|' FROM t1 WHERE w=99 OR +w=100 OR 6=w ORDER BY +w
  }
} {6 2 49 51 | 99 6 10000 10006 | 100 6 10201 10207 | sort t1 *}
do_test where2-6.5 {
  queryplan {
    SELECT *, '|' FROM t1 WHERE w=99 OR y=10201 OR 6=w ORDER BY +w
  }
} {6 2 49 51 | 99 6 10000 10006 | 100 6 10201 10207 | sort t1 *}
set ::idx {}
ifcapable subquery {set ::idx i1zyx}
do_test where2-6.5 {
  queryplan {
    SELECT b.* FROM t1 a, t1 b
     WHERE a.w=1 AND (a.y=b.z OR b.z=10)
     ORDER BY +b.w
  }
} [list 1 0 4 4 2 1 9 10 sort a i1w b $::idx]
do_test where2-6.6 {
  queryplan {
    SELECT b.* FROM t1 a, t1 b
     WHERE a.w=1 AND (b.z=10 OR a.y=b.z OR b.z=10)
     ORDER BY +b.w
  }
} [list 1 0 4 4 2 1 9 10 sort a i1w b $::idx]
if {[permutation] != "no_optimization"} {

do_test where2-6.7 {
  execsql {
    CREATE TABLE t2249a(a TEXT UNIQUE, x CHAR(100));
    CREATE TABLE t2249b(b INTEGER);
    INSERT INTO t2249a(a) VALUES('0123');
    INSERT INTO t2249b VALUES(123);
  }
  queryplan {
    -- Because a is type TEXT and b is type INTEGER, both a and b
    -- will attempt to convert to NUMERIC before the comparison.
    -- They will thus compare equal.
    --
    SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE a=b;
  }
} {123 0123 nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
do_test where2-6.9 {
  queryplan {
    -- The + operator removes affinity from the rhs.  No conversions
    -- occur and the comparison is false.  The result is an empty set.
    --
    SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE a=+b;
  }
} {nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
do_test where2-6.9.2 {
  queryplan {
    SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE +b=a
  }
} {nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
do_test where2-6.10 {
  queryplan {
    -- Use + on both sides of the comparison to disable indices
    -- completely.  Make sure we get the same result.
    --
    SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE +a=+b;
  }
} {nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
do_test where2-6.11 {
  queryplan {
    SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE a=b OR a='hello';
  }
} {123 0123 nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
do_test where2-6.11.2 {
  queryplan {
    SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE b=a OR a='hello';
  }
} {123 0123 nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
do_test where2-6.11.3 {
  queryplan {
    SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE 'hello'=a OR b=a;
  }
} {123 0123 nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
do_test where2-6.11.4 {
  queryplan {
    SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE a='hello' OR b=a;
  }
} {123 0123 nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
ifcapable explain&&subquery {
  do_test where2-6.12 {
    queryplan {
      SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE a=+b OR a='hello';
    }
  } {nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
  do_test where2-6.12.2 {
    queryplan {
      SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE a='hello' OR +b=a;
    }
  } {nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
  do_test where2-6.12.3 {
    queryplan {
      SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE +b=a OR a='hello';
    }
  } {nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
  do_test where2-6.13 {
    queryplan {
      SELECT b,a FROM t2249b CROSS JOIN t2249a WHERE a=+b OR +a='hello';
    }
  } {nosort t2249b * t2249a sqlite_autoindex_t2249a_1}
}

do_execsql_test where2-6.14.1 {
  CREATE TABLE t614a(a TEXT COLLATE NOCASE, b TEXT COLLATE NOCASE);
  INSERT INTO t614a VALUES('AAA','BBB');
  CREATE TABLE t614b(x,y,c TEXT);
  INSERT INTO t614b(c) VALUES('aaa'),('bbb');
  CREATE INDEX t614b_c ON t614b(c);
  SELECT c FROM t614a, t614b WHERE a=c OR b=c;
} {aaa bbb}
do_execsql_test where2-6.14.2 {
  SELECT c FROM t614a, t614b WHERE c=a OR c=b;
} {}
do_execsql_test where2-6.14.3 {
  SELECT c FROM t614a, t614b WHERE c=a OR b=c;
} {bbb}
do_execsql_test where2-6.14.4 {
  SELECT c FROM t614a, t614b WHERE a=c OR c=b;
} {aaa}
do_execsql_test where2-6.15.1 {
  CREATE TABLE t615a(a TEXT, b TEXT);
  INSERT INTO t615a VALUES('AAA','BBB');
  CREATE TABLE t615b(x,y,c TEXT COLLATE NOCASE);
  INSERT INTO t615b(c) VALUES('aaa'),('bbb');
  CREATE INDEX t615b_c ON t615b(c);
  SELECT c FROM t615a, t615b WHERE c=a OR c=b;
} {aaa bbb}
do_execsql_test where2-6.15.2 {
  SELECT c FROM t615a, t615b WHERE a=c OR b=c;
} {}
do_execsql_test where2-6.15.3 {
  SELECT c FROM t615a, t615b WHERE c=a OR b=c;
} {aaa}
do_execsql_test where2-6.15.4 {
  SELECT c FROM t615a, t615b WHERE a=c OR c=b;
} {bbb}
do_execsql_test where2-6.16.1 {
  CREATE TABLE t616a(a TEXT COLLATE NOCASE, b TEXT COLLATE NOCASE);
  INSERT INTO t616a VALUES('AAA','BBB');
  CREATE TABLE t616b(x,y,c TEXT COLLATE NOCASE);
  INSERT INTO t616b(c) VALUES('aaa'),('bbb');
  CREATE INDEX t616b_c ON t616b(c);
  SELECT c FROM t616a, t616b WHERE c=a OR c=b;
} {aaa bbb}
do_execsql_test where2-6.16.2 {
  SELECT c FROM t616a, t616b WHERE a=c OR b=c;
} {aaa bbb}
do_execsql_test where2-6.16.3 {
  SELECT c FROM t616a, t616b WHERE c=a OR b=c;
} {aaa bbb}
do_execsql_test where2-6.16.4 {
  SELECT c FROM t616a, t616b WHERE a=c OR c=b;
} {aaa bbb}

do_execsql_test where2-6.17.1 {
  CREATE TABLE t617_a(a INTEGER PRIMARY KEY, b TEXT);
  CREATE TABLE t617_b(c INTEGER PRIMARY KEY, d TEXT);
  CREATE VIEW t617 AS SELECT a, b FROM t617_a UNION ALL SELECT c, c FROM t617_b;
}
do_execsql_test where2-6.17.2 {
  SELECT EXISTS(
    SELECT 1
      FROM (SELECT 1 FROM t617 WHERE a='two' OR b='i')
     WHERE a='ex' OR b='iv'
  ) FROM t617;
} {}
do_execsql_test where2-6.17.3 {
  SELECT * FROM t617 JOIN t617 USING(a,a,b) WHERE (b=0) OR a=10;
} {}


do_test where2-6.20 {
  queryplan {
    SELECT x.a, y.a FROM t2249a x CROSS JOIN t2249a y WHERE x.a=y.a
  }
} {0123 0123 nosort x sqlite_autoindex_t2249a_1 y sqlite_autoindex_t2249a_1}
ifcapable explain&&subquery {
  do_test where2-6.21 {
    queryplan {
      SELECT x.a,y.a FROM t2249a x CROSS JOIN t2249a y
       WHERE x.a=y.a OR y.a='hello'
    }
  } {0123 0123 nosort x sqlite_autoindex_t2249a_1 y sqlite_autoindex_t2249a_1}
  do_test where2-6.22 {
    queryplan {
      SELECT x.a,y.a FROM t2249a x CROSS JOIN t2249a y
       WHERE y.a=x.a OR y.a='hello'
    }
  } {0123 0123 nosort x sqlite_autoindex_t2249a_1 y sqlite_autoindex_t2249a_1}
  do_test where2-6.23 {
    queryplan {
      SELECT x.a,y.a FROM t2249a x CROSS JOIN t2249a y
       WHERE y.a='hello' OR x.a=y.a
    }
  } {0123 0123 nosort x sqlite_autoindex_t2249a_1 y sqlite_autoindex_t2249a_1}
}

do_test where2-7.1 {
  cksort {
    create table t8(a unique, b, c);
    insert into t8 values(1,2,3);
    insert into t8 values(2,3,4);
    create table t9(x,y);
    insert into t9 values(2,4);
    insert into t9 values(2,3);
    select y from t8, t9 where a=1 order by a, y;
  }
} {3 4 sort}
do_test where2-7.2 {
  cksort {
    select * from t8 where a=1 order by b, c
  }
} {1 2 3 nosort}
do_test where2-7.3 {
  cksort {
    select * from t8, t9 where a=1 and y=3 order by b, x
  }
} {1 2 3 2 3 sort}
do_test where2-7.4 {
  cksort {
    create unique index i9y on t9(y);
    select * from t8, t9 where a=1 and y=3 order by b, x
  }
} {1 2 3 2 3 nosort}

} ;# if {[permutation] != "no_optimization"}
ifcapable subquery {
  do_test where2-8.1 {
    execsql {
      SELECT * FROM t1 WHERE x IN (20,21) AND y IN (1,2)
    }
  } {}
  do_test where2-8.2 {
    execsql {
      SELECT * FROM t1 WHERE x IN (1,2) AND y IN (-5,-6)
    }
  } {}
  execsql {CREATE TABLE tx AS SELECT * FROM t1}
  do_test where2-8.3 {
    execsql {
      SELECT w FROM t1
       WHERE x IN (SELECT x FROM tx WHERE rowid<0)
         AND +y IN (SELECT y FROM tx WHERE rowid=1)
    }
  } {}
  do_test where2-8.4 {
    execsql {
      SELECT w FROM t1
       WHERE x IN (SELECT x FROM tx WHERE rowid=1)
         AND y IN (SELECT y FROM tx WHERE rowid<0)
    }
  } {}
  do_test where2-8.5 {
    execsql {
      CREATE INDEX tx_xyz ON tx(x, y, z, w);
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 12 AND 14)
    }
  } {12 13 14}
  do_test where2-8.6 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 12 AND 14)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {12 13 14}
  do_test where2-8.7 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 12 AND 14)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {10 11 12 13 14 15}
  do_test where2-8.8 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {10 11 12 13 14 15 16 17 18 19 20}
  do_test where2-8.9 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 2 AND 4)
    }
  } {}
  do_test where2-8.10 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 2 AND 4)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {}
  do_test where2-8.11 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 2 AND 4)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {}
  do_test where2-8.12 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN -4 AND -2)
    }
  } {}
  do_test where2-8.13 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN -4 AND -2)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {}
  do_test where2-8.14 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN -4 AND -2)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {}
  do_test where2-8.15 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 200 AND 300)
    }
  } {}
  do_test where2-8.16 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 200 AND 300)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {}
  do_test where2-8.17 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE w BETWEEN 200 AND 300)
         AND y IN (SELECT y FROM t1 WHERE w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE w BETWEEN 10 AND 20)
    }
  } {}
  do_test where2-8.18 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE +w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE +w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE +w BETWEEN 200 AND 300)
    }
  } {}
  do_test where2-8.19 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE +w BETWEEN 10 AND 20)
         AND y IN (SELECT y FROM t1 WHERE +w BETWEEN 200 AND 300)
         AND z IN (SELECT z FROM t1 WHERE +w BETWEEN 10 AND 20)
    }
  } {}
  do_test where2-8.20 {
    execsql {
      SELECT w FROM tx
       WHERE x IN (SELECT x FROM t1 WHERE +w BETWEEN 200 AND 300)
         AND y IN (SELECT y FROM t1 WHERE +w BETWEEN 10 AND 20)
         AND z IN (SELECT z FROM t1 WHERE +w BETWEEN 10 AND 20)
    }
  } {}
}  
ifcapable or_opt&&tclvar {
  do_test where2-9.1 {
    execsql {
      BEGIN;
      CREATE TABLE t10(a,b,c);
      INSERT INTO t10 VALUES(1,1,1);
      INSERT INTO t10 VALUES(1,2,2);
      INSERT INTO t10 VALUES(1,3,3);
    }
    for {set i 4} {$i<=1000} {incr i} {
      execsql {INSERT INTO t10 VALUES(1,$i,$i)}
    }
    execsql {
      CREATE INDEX i10 ON t10(a,b);
      COMMIT;
      SELECT count(*) FROM t10;
    }
  } 1000
  ifcapable subquery {
    do_test where2-9.2 {
      count {
        SELECT * FROM t10 WHERE a=1 AND (b=2 OR b=3)
      }
    } {1 2 2 1 3 3 7}
  }
}
do_test where2-11.1 {
  execsql {
    CREATE TABLE t11(a,b,c,d);
    CREATE INDEX i11aba ON t11(a,b,a,c); -- column A occurs twice.
    INSERT INTO t11 VALUES(1,2,3,4);
    INSERT INTO t11 VALUES(5,6,7,8);
    INSERT INTO t11 VALUES(1,2,9,10);
    INSERT INTO t11 VALUES(5,11,12,13);
    SELECT c FROM t11 WHERE a=1 AND b=2 ORDER BY c;
  }
} {3 9}
do_test where2-11.2 {
  execsql {
    CREATE INDEX i11cccccccc ON t11(c,c,c,c,c,c,c,c); -- repeated column
    SELECT d FROM t11 WHERE c=9;
  }
} {10}
do_test where2-11.3 {
  execsql {
    SELECT d FROM t11 WHERE c IN (1,2,3,4,5);
  }
} {4}
do_test where2-11.4 {
  execsql {
    SELECT d FROM t11 WHERE c=7 OR (a=1 AND b=2) ORDER BY d;
  }
} {4 8 10}
if {[permutation] != "no_optimization"} {
do_execsql_test where2-12.1 {
  CREATE TABLE t12(x INTEGER PRIMARY KEY, y INT, z CHAR(100));
  CREATE INDEX t12y ON t12(y);
  EXPLAIN QUERY PLAN
    SELECT a.x, b.x
      FROM t12 AS a JOIN t12 AS b ON a.y=b.x
     WHERE (b.x=$abc OR b.y=$abc);
} {/SEARCH b .*SEARCH b /}
}
do_execsql_test where2-13.1 {
  CREATE TABLE t13(a,b);
  CREATE INDEX t13a ON t13(a);
  INSERT INTO t13 VALUES(4,5);
  SELECT * FROM t13 WHERE (1=2 AND a=3) OR a=4;
} {4 5}
do_execsql_test where2-14.1 {
  CREATE TABLE t14a(x INTEGER PRIMARY KEY);
  INSERT INTO t14a(x) VALUES(1),(2),(3),(4);
  CREATE TABLE t14b(y INTEGER PRIMARY KEY);
  INSERT INTO t14b(y) VALUES(1);
  SELECT x FROM t14a WHERE x NOT IN (SELECT x FROM t14b);
} {}
do_execsql_test where2-15.1 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(a INTEGER PRIMARY KEY,b INT);
  INSERT INTO t1 VALUES(12,34),(56,78),(90,12);
  DROP TABLE IF EXISTS t2;
  CREATE TABLE t2(x INT UNIQUE);
  INSERT INTO t2 VALUES(78),(12);
  SELECT a,b,quote(x),'|'
    FROM t1 LEFT JOIN (SELECT x FROM t2) AS s1 ON t1.b=s1.x
   ORDER BY a,EXISTS(SELECT 1 FROM t1 LEFT JOIN (SELECT x AS y FROM t2) AS s2 ON t1.b=s2.y),x;
} {12 34 NULL | 56 78 78 | 90 12 12 |}
reset_db
do_execsql_test where2-16.1 {
  CREATE TABLE t1(a INTEGER PRIMARY KEY, b INT);
  CREATE TABLE t2(c INTEGER PRIMARY KEY, d INT);
  CREATE TABLE t3(e INTEGER PRIMARY KEY, f INT);
  CREATE TABLE t4(g INTEGER PRIMARY KEY, h INT);
}
do_execsql_test where2-16.2 {
  EXPLAIN QUERY PLAN
  SELECT *
    FROM t1, t2 CROSS JOIN t3, t4
   WHERE t4.g=1
     AND t1.a=t4.h
     AND t2.c=t1.b
     AND t3.e=t2.d;
} {~/.* t4 .* t[12] .*/}
do_execsql_test where2-16.2 {
  EXPLAIN QUERY PLAN
  SELECT *
    FROM t1, t2 CROSS JOIN t3, t4
   WHERE t2.c=1
     AND t3.e=t2.d
     AND t4.g=t3.f
     AND t1.a=t4.h;
} {~/.* t[34] .* t1 .*/}
do_execsql_test where2-17.0 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(a TEXT);
  INSERT INTO t1 VALUES('A'),('b');
  SELECT * FROM t1 WHERE (a='a' COLLATE nocase) OR a='b' ORDER BY +a;
} {A b}
do_execsql_test where2-17.1 {
  CREATE INDEX t1_a ON t1(a);
  SELECT * FROM t1 WHERE (a='a' COLLATE nocase) OR a='b' ORDER BY +a;
} {A b}
finish_test
"#,
    );
}
