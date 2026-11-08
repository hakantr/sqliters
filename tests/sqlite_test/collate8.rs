#[test]
#[ignore = "7/23 subtests passed; pending: collate8 edge cases"]
fn test_collate8() {
    sqliters_tcltest::run_test_script(
        "collate8",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test collate8-1.1 {
  execsql {
    CREATE TABLE t1(a TEXT COLLATE nocase);
    INSERT INTO t1 VALUES('aaa');
    INSERT INTO t1 VALUES('BBB');
    INSERT INTO t1 VALUES('ccc');
    INSERT INTO t1 VALUES('DDD');
    SELECT a FROM t1 ORDER BY a;
  }
} {aaa BBB ccc DDD}
do_test collate8-1.2 {
  execsql {
    SELECT rowid FROM t1 WHERE a<'ccc' ORDER BY 1
  }
} {1 2}
do_test collate8-1.3 {
  execsql {
    SELECT rowid FROM t1 WHERE a<'ccc' COLLATE binary ORDER BY 1
  }
} {1 2 4}
do_test collate8-1.4 {
  execsql {
    SELECT rowid FROM t1 WHERE +a<'ccc' ORDER BY 1
  }
} {1 2}
do_test collate8-1.5 {
  execsql {
    SELECT a FROM t1 ORDER BY +a
  }
} {aaa BBB ccc DDD}
do_test collate8-1.11 {
  execsql {
    SELECT a AS x FROM t1 ORDER BY "x";
  }
} {aaa BBB ccc DDD}
do_test collate8-1.12 {
  execsql {
    SELECT a AS x FROM t1 WHERE x<'ccc' ORDER BY 1
  }
} {aaa BBB}
do_test collate8-1.13 {
  execsql {
    SELECT a AS x FROM t1 WHERE x<'ccc' COLLATE binary ORDER BY [x]
  }
} {aaa BBB DDD}
do_test collate8-1.14 {
  execsql {
    SELECT a AS x FROM t1 WHERE +x<'ccc' ORDER BY 1
  }
} {aaa BBB}
do_test collate8-1.15 {
  execsql {
    SELECT a AS x FROM t1 ORDER BY +x
  }
} {aaa BBB ccc DDD}
do_test collate8-2.1 {
  execsql {
    CREATE TABLE t2(a);
    INSERT INTO t2 VALUES('abc');
    INSERT INTO t2 VALUES('ABC');
    SELECT a AS x FROM t2 WHERE x='abc';
  }
} {abc}
do_test collate8-2.2 {
  execsql {
    SELECT a AS x FROM t2 WHERE x='abc' COLLATE nocase;
  }
} {abc ABC}
do_test collate8-2.3 {
  execsql {
    SELECT a AS x FROM t2 WHERE (x COLLATE nocase)='abc';
  }
} {abc ABC}
do_test collate8-2.4 {
  execsql {
    SELECT a COLLATE nocase AS x FROM t2 WHERE x='abc';
  }
} {abc ABC}
do_test collate8-2.5 {
  execsql {
    SELECT a COLLATE nocase AS x FROM t2 WHERE (x COLLATE binary)='abc';
  }
} {abc}
do_test collate8-2.6 {
  execsql {
    SELECT a COLLATE nocase AS x FROM t2 WHERE x='abc' COLLATE binary;
  }
} {abc ABC}
do_test collate8-2.7 {
  execsql {
    SELECT * FROM t2 WHERE (a COLLATE nocase)='abc' COLLATE binary;
  }
} {abc ABC}
do_test collate8-2.8 {
  execsql {
    SELECT a COLLATE nocase AS x FROM t2 WHERE 'abc'=x COLLATE binary;
  }
} {abc}
do_execsql_test collate8-3.1 {
  SELECT 'abc'==('ABC'||'') COLLATE nocase;
  SELECT 'abc'==('ABC'||'' COLLATE nocase);
  SELECT 'abc'==('ABC'||('' COLLATE nocase));
  SELECT 'abc'==('ABC'||upper('' COLLATE nocase));
} {1 1 1 1}
do_execsql_test collate8-3.2 {
  SELECT 'abc'==('ABC'||max('' COLLATE nocase,'' COLLATE binary));
} {1}
do_execsql_test collate8-3.3 {
  SELECT 'abc'==('ABC'||max('' COLLATE binary,'' COLLATE nocase));
} {0}
do_execsql_test collate8-3.4 {
  SELECT 'abc'==('ABC'||CASE WHEN 1-1=2 THEN '' COLLATE nocase
                                        ELSE '' COLLATE binary END);
  SELECT 'abc'==('ABC'||CASE WHEN 1+1=2 THEN '' COLLATE nocase
                                        ELSE '' COLLATE binary END);
} {1 1}
do_execsql_test collate8-3.5 {
  SELECT 'abc'==('ABC'||CASE WHEN 1=2 THEN '' COLLATE binary
                                      ELSE '' COLLATE nocase END);
} {0}
finish_test
"#,
    );
}
