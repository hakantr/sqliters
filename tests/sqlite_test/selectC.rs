#![forbid(unsafe_code)]

#[test]
#[ignore = "24/28 subtests passed; pending: Tcl UDF"]
fn test_selectC() {
    sqliters_tcltest::run_test_script(
        "selectC",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix selectC
do_test selectC-1.1 {
  execsql {
    CREATE TABLE t1(a, b, c);
    INSERT INTO t1 VALUES(1,'aaa','bbb');
    INSERT INTO t1 SELECT * FROM t1;
    INSERT INTO t1 VALUES(2,'ccc','ddd');

    SELECT DISTINCT a AS x, b||c AS y
      FROM t1
     WHERE y IN ('aaabbb','xxx');
  }
} {1 aaabbb}
do_test selectC-1.2 {
  execsql {
    SELECT DISTINCT a AS x, b||c AS y
      FROM t1
     WHERE b||c IN ('aaabbb','xxx');
  }
} {1 aaabbb}
do_test selectC-1.3 {
  execsql {
    SELECT DISTINCT a AS x, b||c AS y
      FROM t1
     WHERE y='aaabbb'
  }
} {1 aaabbb}
do_test selectC-1.4 {
  execsql {
    SELECT DISTINCT a AS x, b||c AS y
      FROM t1
     WHERE b||c='aaabbb'
  }
} {1 aaabbb}
do_test selectC-1.5 {
  execsql {
    SELECT DISTINCT a AS x, b||c AS y
      FROM t1
     WHERE x=2
  }
} {2 cccddd}
do_test selectC-1.6 {
  execsql {
    SELECT DISTINCT a AS x, b||c AS y
      FROM t1
     WHERE a=2
  }
} {2 cccddd}
do_test selectC-1.7 {
  execsql {
    SELECT DISTINCT a AS x, b||c AS y
      FROM t1
     WHERE +y='aaabbb'
  }
} {1 aaabbb}
do_test selectC-1.8 {
  execsql {
    SELECT a AS x, b||c AS y
      FROM t1
     GROUP BY x, y
    HAVING y='aaabbb'
  }
} {1 aaabbb}
do_test selectC-1.9 {
  execsql {
    SELECT a AS x, b||c AS y
      FROM t1
     GROUP BY x, y
    HAVING b||c='aaabbb'
  }
} {1 aaabbb}
do_test selectC-1.10 {
  execsql {
    SELECT a AS x, b||c AS y
      FROM t1
     WHERE y='aaabbb'
     GROUP BY x, y
  }
} {1 aaabbb}
do_test selectC-1.11 {
  execsql {
    SELECT a AS x, b||c AS y
      FROM t1
     WHERE b||c='aaabbb'
     GROUP BY x, y
  }
} {1 aaabbb}
proc longname_toupper x {return [string toupper $x]}
db function uppercaseconversionfunctionwithaverylongname longname_toupper
do_test selectC-1.12.1 {
  execsql {
    SELECT DISTINCT upper(b) AS x
      FROM t1
     ORDER BY x
  }
} {AAA CCC}
do_test selectC-1.12.2 {
  execsql {
    SELECT DISTINCT uppercaseconversionfunctionwithaverylongname(b) AS x
      FROM t1
     ORDER BY x
  }
} {AAA CCC}
do_test selectC-1.13.1 {
  execsql {
    SELECT upper(b) AS x
      FROM t1
     GROUP BY x
     ORDER BY x
  }
} {AAA CCC}
do_test selectC-1.13.2 {
  execsql {
    SELECT uppercaseconversionfunctionwithaverylongname(b) AS x
      FROM t1
     GROUP BY x
     ORDER BY x
  }
} {AAA CCC}
do_test selectC-1.14.1 {
  execsql {
    SELECT upper(b) AS x
      FROM t1
     ORDER BY x DESC
  }
} {CCC AAA AAA}
do_test selectC-1.14.2 {
  execsql {
    SELECT uppercaseconversionfunctionwithaverylongname(b) AS x
      FROM t1
     ORDER BY x DESC
  }
} {CCC AAA AAA}
ifcapable trigger&&compound {
  do_test selectC-2.1 {
    catchsql {
      CREATE TABLE t21a(a,b);
      INSERT INTO t21a VALUES(1,2);
      CREATE TABLE t21b(n);
      CREATE TRIGGER r21 AFTER INSERT ON t21b BEGIN
        SELECT a FROM t21a WHERE a>new.x UNION ALL
        SELECT b FROM t21a WHERE b>new.x ORDER BY 1 LIMIT 2;
      END;
      INSERT INTO t21b VALUES(6);
    }
  } {1 {no such column: new.x}}
}
do_test selectC-3.1 {
  execsql {
    CREATE TABLE person (
        org_id          TEXT NOT NULL,
        nickname        TEXT NOT NULL,
        license         TEXT,
        CONSTRAINT person_pk PRIMARY KEY (org_id, nickname),
        CONSTRAINT person_license_uk UNIQUE (license)
    );
    INSERT INTO person VALUES('meyers', 'jack', '2GAT123');
    INSERT INTO person VALUES('meyers', 'hill', 'V345FMP');
    INSERT INTO person VALUES('meyers', 'jim', '2GAT138');
    INSERT INTO person VALUES('smith', 'maggy', '');
    INSERT INTO person VALUES('smith', 'jose', 'JJZ109');
    INSERT INTO person VALUES('smith', 'jack', 'THX138');
    INSERT INTO person VALUES('lakeside', 'dave', '953OKG');
    INSERT INTO person VALUES('lakeside', 'amy', NULL);
    INSERT INTO person VALUES('lake-apts', 'tom', NULL);
    INSERT INTO person VALUES('acorn', 'hideo', 'CQB421');
    
    SELECT 
      org_id, 
      count((NOT (org_id IS NULL)) AND (NOT (nickname IS NULL)))
    FROM person 
    WHERE (CASE WHEN license != '' THEN 1 ELSE 0 END)
    GROUP BY 1;
  }
} {acorn 1 lakeside 1 meyers 3 smith 2}
do_test selectC-3.2 {
  execsql {
    CREATE TABLE t2(a PRIMARY KEY, b);
    INSERT INTO t2 VALUES('abc', 'xxx');
    INSERT INTO t2 VALUES('def', 'yyy');
    SELECT a, max(b || a) FROM t2 WHERE (b||b||b)!='value' GROUP BY a;
  }
} {abc xxxabc def yyydef}
do_test selectC-3.3 {
  execsql {
    SELECT b, max(a || b) FROM t2 WHERE (b||b||b)!='value' GROUP BY a;
  }
} {xxx abcxxx yyy defyyy}
proc udf {} { incr ::udf }
set ::udf 0
db function udf udf
do_execsql_test selectC-4.1 {
  create table t_distinct_bug (a, b, c);
  insert into t_distinct_bug values ('1', '1', 'a');
  insert into t_distinct_bug values ('1', '2', 'b');
  insert into t_distinct_bug values ('1', '3', 'c');
  insert into t_distinct_bug values ('1', '1', 'd');
  insert into t_distinct_bug values ('1', '2', 'e');
  insert into t_distinct_bug values ('1', '3', 'f');
} {}
do_execsql_test selectC-4.2 {
  select a from (select distinct a, b from t_distinct_bug)
} {1 1 1}
do_execsql_test selectC-4.2b {
  CREATE VIEW v42b AS SELECT DISTINCT a, b FROM t_distinct_bug;
  SELECT a FROM v42b;
} {1 1 1}
do_execsql_test selectC-4.3 {
  select a, udf() from (select distinct a, b from t_distinct_bug)
} {1 1 1 2 1 3}
do_execsql_test 5.0 {
  CREATE TABLE x1(a);
  CREATE TABLE x2(b);
  CREATE TABLE x3(c);
  CREATE VIEW vvv AS SELECT b FROM x2 ORDER BY 1;

  INSERT INTO x1 VALUES('a'), ('b');
  INSERT INTO x2 VALUES(22), (23), (25), (24), (21);
  INSERT INTO x3 VALUES(302), (303), (301);
}
do_execsql_test 5.1 {
  CREATE TABLE x4 AS SELECT b FROM vvv UNION ALL SELECT c from x3;
  SELECT * FROM x4;
} {21 22 23 24 25 302 303 301}
do_execsql_test 5.2 {
  SELECT * FROM x1, x4
} {
  a 21 a 22 a 23 a 24 a 25 a 302 a 303 a 301
  b 21 b 22 b 23 b 24 b 25 b 302 b 303 b 301
}
do_execsql_test 5.3 {
  SELECT * FROM x1, (SELECT b FROM vvv UNION ALL SELECT c from x3) ORDER BY 1,2;
} {
  a 21 a 22 a 23 a 24 a 25 a 301 a 302 a 303
  b 21 b 22 b 23 b 24 b 25 b 301 b 302 b 303
}
finish_test
"#,
    );
}
