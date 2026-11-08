#[test]
fn test_rowvalueA() {
    sqliters_tcltest::run_test_script("rowvalueA", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix rowvalueA
do_execsql_test 1.0 {
  SELECT (1, 2) IN ( (3, 4), (5, 6), (1, 3) );
} {0}
do_execsql_test 1.1 {
  SELECT (1, 2) IN ( (3, 4), (5, 6), (1, 2) );
} {1}
do_execsql_test 1.2 {
  SELECT (1, 2) IN ( (3, 2) );
} {0}
do_execsql_test 1.3 {
  SELECT (1, 2) IN ( (1, 2) );
} {1}
do_execsql_test 1.4 {
  SELECT (1, 2) IN ( );
} {0}
do_execsql_test 1.5 {
  SELECT (1, 2) NOT IN ( );
} {1}
for {set ii 0} {$ii < 2000} {incr ii} {
  lappend L "($ii, $ii)"
}
do_execsql_test 1.6.1 "
  SELECT (400,400) IN ( [join $L ,] )
" 1
do_execsql_test 1.6.2 "
  SELECT (1500,1500) IN ( [join $L ,] )
" 1
do_execsql_test 1.6.2 "
  SELECT (1500,1499) IN ( [join $L ,] )
" 0
do_catchsql_test 2.0 {
  SELECT (1, 2) IN ( (1, 2), (3, 4, 5), (5, 6) )
} {1 {IN(...) element has 3 terms - expected 2}}
do_catchsql_test 2.1 {
  SELECT (1, 2) IN ( (1, 2), 4, (5, 6) )
} {1 {IN(...) element has 1 term - expected 2}}
do_catchsql_test 2.2 {
  SELECT (1, 2, 3) IN ( (1, 2), (3, 4), (5, 6) )
} {1 {IN(...) element has 2 terms - expected 3}}
do_catchsql_test 2.3 {
  SELECT 2 IN ( (1, 2), (3, 4), (5, 6) )
} {1 {row value misused}}
do_execsql_test 3.0 {
  CREATE TABLE x2 (x, y);
  INSERT INTO x2 VALUES (1234, 'abc');

  CREATE TABLE x1 (a, b PRIMARY KEY COLLATE NOCASE) WITHOUT ROWID;
  INSERT INTO x1 VALUES (1234, 'ABCD');
}
do_execsql_test 3.1 {
  SELECT * FROM x2 CROSS JOIN x1 WHERE (1234, x2.y) > (x1.a, x1.b);
} {1234 abc 1234 ABCD}
do_execsql_test 3.2 {
  CREATE INDEX x1a ON x1(a);
}
do_execsql_test 3.3 {
  SELECT * FROM x2 CROSS JOIN x1 WHERE (1234, x2.y) > (x1.a, x1.b);
} {1234 abc 1234 ABCD}
do_execsql_test 4.0 {
  CREATE TABLE t1 (a PRIMARY KEY, b COLLATE NOCASE) WITHOUT ROWID;
  INSERT INTO t1 VALUES ('BBB', 'a');
}
do_execsql_test 4.1 {
  SELECT * FROM t1 WHERE (t1.a, t1.b) <= ('BBB', 'CCC');
} {BBB a}
do_execsql_test 4.2 {
  DROP TABLE t1;
  CREATE TABLE t0 (c0);
  INSERT INTO t0 VALUES ('True');

  CREATE TABLE t1 (c0 COLLATE NOCASE, c1 PRIMARY KEY) WITHOUT ROWID;
  INSERT INTO t1 VALUES ('a', 1);
  INSERT INTO t1 VALUES ('a', 'True');
}
do_execsql_test 4.3 {
  SELECT * FROM t0, t1 WHERE (t1.c1, t1.c0) <= (t0.c0, t0.c0);
} {True a 1 True a True}
do_execsql_test 5.1 {
  DROP TABLE t1;
  CREATE TABLE t1(a TEXT, b TEXT);
  CREATE INDEX t1_ab ON t1(a COLLATE NOCASE, b COLLATE NOCASE);
  INSERT INTO t1 VALUES('a',null),('b', 'ABCD');
}
db null NULL
do_execsql_test 5.2 {
  SELECT * FROM t1
   WHERE ('B' COLLATE nocase,'abc')>(a,b)
   ORDER BY +a;
} {a NULL b ABCD}
do_execsql_test 5.3 {
  SELECT * FROM t1 NOT INDEXED
   WHERE ('B' COLLATE nocase,'abc')>(a,b)
   ORDER BY +a;
} {a NULL b ABCD}
do_execsql_test 6.1 {
  SELECT 'abc' = (SELECT 'ABC' COLLATE nocase);
} {0} 
do_execsql_test 6.2 {
  SELECT 'abc' IN (SELECT 'ABC' COLLATE nocase);
} {1}
do_execsql_test 6.3 {
  SELECT ('abc', 1) = (SELECT 'ABC' COLLATE nocase, 1);
} {0}
do_execsql_test 6.4 {
  SELECT ( SELECT CAST(123 AS INTEGER) )=='123'
} {1}
do_execsql_test 6.5 {
  SELECT (SELECT CAST(123 AS INTEGER), 1)==('123', 1)
} {1}
do_execsql_test 6.6 {
  SELECT ( SELECT 123 )=='123'
} {0}
do_execsql_test 6.7 {
  CREATE TABLE t33(a, b, c);
  INSERT INTO t33 VALUES ('abc', 1, 'i');
  INSERT INTO t33 VALUES ('ABC', 1, 'ii');
}
do_execsql_test 6.8 {
  SELECT c FROM t33 WHERE (a, b) = (SELECT 'abc' COLLATE nocase, 1);
} {i}
do_execsql_test 6.9 {
  SELECT (a, b) = (SELECT 'abc' COLLATE nocase, 1) FROM t33;
} {1 0}
do_execsql_test 6.10 {
    SELECT c FROM t33 WHERE CASE WHEN 1
      THEN (a, b) = (SELECT 'abc' COLLATE nocase, 1)
      ELSE (a, b) = (SELECT 'abc' COLLATE nocase, 1)
    END;
} {i}
finish_test
"#);
}
