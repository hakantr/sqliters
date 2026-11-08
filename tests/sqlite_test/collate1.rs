#[test]
#[ignore = "20/58 subtests passed; pending: VIEW"]
fn test_collate1() {
    sqliters_tcltest::run_test_script("collate1", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix collate1
db collate HEX hex_collate
proc hex_collate {lhs rhs} {
  set lhs_ishex [regexp {^(0x|)[1234567890abcdefABCDEF]+$} $lhs]
  set rhs_ishex [regexp {^(0x|)[1234567890abcdefABCDEF]+$} $rhs]
  if {$lhs_ishex && $rhs_ishex} { 
    set lhsx [scan $lhs %x]
    set rhsx [scan $rhs %x]
    if {$lhs < $rhs} {return -1}
    if {$lhs == $rhs} {return 0}
    if {$lhs > $rhs} {return 1}
  }
  if {$lhs_ishex} {
    return -1;
  }
  if {$rhs_ishex} {
    return 1;
  }
  return [string compare $lhs $rhs]
}
db function hex {format 0x%X}
db collate numeric numeric_collate
proc numeric_collate {lhs rhs} {
  if {$lhs == $rhs} {return 0} 
  return [expr ($lhs>$rhs)?1:-1]
}
do_test collate1-1.0 {
  execsql {
    CREATE TABLE collate1t1(c1, c2);
    INSERT INTO collate1t1 VALUES(45, hex(45));
    INSERT INTO collate1t1 VALUES(NULL, NULL);
    INSERT INTO collate1t1 VALUES(281, hex(281));
  }
} {}
do_test collate1-1.1 {
  execsql {
    SELECT c2 FROM collate1t1 ORDER BY 1;
  }
} {{} 0x119 0x2D}
do_test collate1-1.2 {
  execsql {
    SELECT c2 FROM collate1t1 ORDER BY 1 COLLATE hex;
  }
} {{} 0x2D 0x119}
do_test collate1-1.3 {
  execsql {
    SELECT c2 FROM collate1t1 ORDER BY 1 COLLATE hex DESC;
  }
} {0x119 0x2D {}}
do_test collate1-1.4 {
  execsql {
   SELECT c2 FROM collate1t1 ORDER BY 1 COLLATE hex ASC;
  }
} {{} 0x2D 0x119}
do_test collate1-1.5 {
  execsql {
    SELECT c2 COLLATE hex FROM collate1t1 ORDER BY 1
  }
} {{} 0x2D 0x119}
do_test collate1-1.6 {
  execsql {
    SELECT c2 COLLATE hex FROM collate1t1 ORDER BY 1 ASC
  }
} {{} 0x2D 0x119}
do_test collate1-1.7 {
  execsql {
    SELECT c2 COLLATE hex FROM collate1t1 ORDER BY 1 DESC
  }
} {0x119 0x2D {}}
do_test collate1-1.99 {
  execsql {
    DROP TABLE collate1t1;
  }
} {}
do_test collate1-2.0 {
  execsql {
    CREATE TABLE collate1t1(c1, c2);
    INSERT INTO collate1t1 VALUES('5', '0x11');
    INSERT INTO collate1t1 VALUES('5', '0xA');
    INSERT INTO collate1t1 VALUES(NULL, NULL);
    INSERT INTO collate1t1 VALUES('7', '0xA');
    INSERT INTO collate1t1 VALUES('11', '0x11');
    INSERT INTO collate1t1 VALUES('11', '0x101');
  }
} {}
do_test collate1-2.2 {
  execsql {
    SELECT c1, c2 FROM collate1t1 ORDER BY 1 COLLATE numeric, 2 COLLATE hex;
  }
} {{} {} 5 0xA 5 0x11 7 0xA 11 0x11 11 0x101}
do_test collate1-2.3 {
  execsql {
    SELECT c1, c2 FROM collate1t1 ORDER BY 1 COLLATE binary, 2 COLLATE hex;
  }
} {{} {} 11 0x11 11 0x101 5 0xA 5 0x11 7 0xA}
do_test collate1-2.4 {
  execsql {
    SELECT c1, c2 FROM collate1t1 ORDER BY 1 COLLATE binary DESC, 2 COLLATE hex;
  }
} {7 0xA 5 0xA 5 0x11 11 0x11 11 0x101 {} {}}
do_test collate1-2.5 {
  execsql {
    SELECT c1, c2 FROM collate1t1 
        ORDER BY 1 COLLATE binary DESC, 2 COLLATE hex DESC;
  }
} {7 0xA 5 0x11 5 0xA 11 0x101 11 0x11 {} {}}
do_test collate1-2.6 {
  execsql {
    SELECT c1, c2 FROM collate1t1 
        ORDER BY 1 COLLATE binary ASC, 2 COLLATE hex ASC;
  }
} {{} {} 11 0x11 11 0x101 5 0xA 5 0x11 7 0xA}
do_test collate1-2.12.1 {
  execsql {
    SELECT c1 COLLATE numeric, c2 FROM collate1t1 
     ORDER BY 1, 2 COLLATE hex;
  }
} {{} {} 5 0xA 5 0x11 7 0xA 11 0x11 11 0x101}
do_test collate1-2.12.2 {
  execsql {
    SELECT c1 COLLATE hex, c2 FROM collate1t1 
     ORDER BY 1 COLLATE numeric, 2 COLLATE hex;
  }
} {{} {} 5 0xA 5 0x11 7 0xA 11 0x11 11 0x101}
do_test collate1-2.12.3 {
  execsql {
    SELECT c1, c2 COLLATE hex FROM collate1t1 
     ORDER BY 1 COLLATE numeric, 2;
  }
} {{} {} 5 0xA 5 0x11 7 0xA 11 0x11 11 0x101}
do_test collate1-2.12.4 {
  execsql {
    SELECT c1 COLLATE numeric, c2 COLLATE hex
      FROM collate1t1 
     ORDER BY 1, 2;
  }
} {{} {} 5 0xA 5 0x11 7 0xA 11 0x11 11 0x101}
do_test collate1-2.13 {
  execsql {
    SELECT c1 COLLATE binary, c2 COLLATE hex
      FROM collate1t1
     ORDER BY 1, 2;
  }
} {{} {} 11 0x11 11 0x101 5 0xA 5 0x11 7 0xA}
do_test collate1-2.14 {
  execsql {
    SELECT c1, c2
      FROM collate1t1 ORDER BY 1 COLLATE binary DESC, 2 COLLATE hex;
  }
} {7 0xA 5 0xA 5 0x11 11 0x11 11 0x101 {} {}}
do_test collate1-2.15 {
  execsql {
    SELECT c1 COLLATE binary, c2 COLLATE hex
      FROM collate1t1 
     ORDER BY 1 DESC, 2 DESC;
  }
} {7 0xA 5 0x11 5 0xA 11 0x101 11 0x11 {} {}}
do_test collate1-2.16 {
  execsql {
    SELECT c1 COLLATE hex, c2 COLLATE binary
      FROM collate1t1 
     ORDER BY 1 COLLATE binary ASC, 2 COLLATE hex ASC;
  }
} {{} {} 11 0x11 11 0x101 5 0xA 5 0x11 7 0xA}
do_test collate1-2.99 {
  execsql {
    DROP TABLE collate1t1;
  }
} {}
do_test collate1-3.0 {
  execsql {
    CREATE TABLE collate1t1(a COLLATE hex, b);
    INSERT INTO collate1t1 VALUES( '0x5', 5 );
    INSERT INTO collate1t1 VALUES( '1', 1 );
    INSERT INTO collate1t1 VALUES( '0x45', 69 );
    INSERT INTO collate1t1 VALUES( NULL, NULL );
    SELECT * FROM collate1t1 ORDER BY a;
  }
} {{} {} 1 1 0x5 5 0x45 69}
do_test collate1-3.1 {
  execsql {
    SELECT * FROM collate1t1 ORDER BY 1;
  }
} {{} {} 1 1 0x5 5 0x45 69}
do_test collate1-3.2 {
  execsql {
    SELECT * FROM collate1t1 ORDER BY collate1t1.a;
  }
} {{} {} 1 1 0x5 5 0x45 69}
do_test collate1-3.3 {
  execsql {
    SELECT * FROM collate1t1 ORDER BY main.collate1t1.a;
  }
} {{} {} 1 1 0x5 5 0x45 69}
do_test collate1-3.4 {
  execsql {
    SELECT a as c1, b as c2 FROM collate1t1 ORDER BY c1;
  }
} {{} {} 1 1 0x5 5 0x45 69}
do_test collate1-3.5 {
  execsql {
    SELECT a as c1, b as c2 FROM collate1t1 ORDER BY c1 COLLATE binary;
  }
} {{} {} 0x45 69 0x5 5 1 1}
do_test collate1-3.5.1 {
  execsql {
    SELECT a COLLATE binary as c1, b as c2
      FROM collate1t1 ORDER BY c1;
  }
} {{} {} 0x45 69 0x5 5 1 1}
do_test collate1-3.6 {
  execsql {
    DROP TABLE collate1t1;
  }
} {}
do_test collate1-4.0 {
  execsql {
    CREATE TABLE collate1t1(c1 numeric, c2 text);
    INSERT INTO collate1t1 VALUES(1, 1);
    INSERT INTO collate1t1 VALUES(12, 12);
    INSERT INTO collate1t1 VALUES(NULL, NULL);
    INSERT INTO collate1t1 VALUES(101, 101);
  }
} {}
do_test collate1-4.1 {
  execsql {
    SELECT c1 FROM collate1t1 ORDER BY 1;
  }
} {{} 1 12 101}
do_test collate1-4.2 {
  execsql {
    SELECT c2 FROM collate1t1 ORDER BY 1;
  }
} {{} 1 101 12}
do_test collate1-4.3 {
  execsql {
    SELECT c2+0 FROM collate1t1 ORDER BY 1;
  }
} {{} 1 12 101}
do_test collate1-4.4 {
  execsql {
    SELECT c1||'' FROM collate1t1 ORDER BY 1;
  }
} {{} 1 101 12}
do_test collate1-4.4.1 {
  execsql {
    SELECT (c1||'') COLLATE numeric FROM collate1t1 ORDER BY 1;
  }
} {{} 1 12 101}
do_test collate1-4.5 {
  execsql {
    DROP TABLE collate1t1;
  }
} {}
do_test collate1-5.1 {
  execsql {
    CREATE TABLE c5(
      id INTEGER PRIMARY KEY,
      a TEXT COLLATE binary COLLATE nocase COLLATE rtrim,
      b TEXT COLLATE nocase COLLATE binary,
      c TEXT COLLATE rtrim COLLATE binary COLLATE rtrim COLLATE nocase
    );
    INSERT INTO c5 VALUES(1, 'abc','abc','abc');
    INSERT INTO c5 VALUES(2, 'abc   ','ABC','ABC');
    SELECT id FROM c5 WHERE a='abc' ORDER BY id;
  }
} {1 2}
do_test collate1-5.2 {
  execsql {
    SELECT id FROM c5 WHERE b='abc' ORDER BY id;
  }
} {1}
do_test collate1-5.3 {
  execsql {
    SELECT id FROM c5 WHERE c='abc' ORDER BY id;
  }
} {1 2}
sqlite3_db_config db SQLITE_DBCONFIG_DQS_DML 1
do_execsql_test 6.1 {
  SELECT """""""";
} {\"\"\"}
do_catchsql_test 6.2 {
  CREATE TABLE x1(a);
  SELECT a FROM x1 ORDER BY a COLLATE """""""";
} {1 {no such collation sequence: """}}
do_catchsql_test 6.3 {
  SELECT a FROM x1 ORDER BY 1 COLLATE """""""";
} {1 {no such collation sequence: """}}
do_catchsql_test 6.4 {
  SELECT 0 UNION SELECT 0 ORDER BY 1 COLLATE """""""";
} {1 {no such collation sequence: """}}
db collate {"""} [list string compare -nocase]
do_execsql_test 6.5 {
  PRAGMA foreign_keys = ON;
  CREATE TABLE p1(a PRIMARY KEY COLLATE '"""');
  CREATE TABLE c1(x, y REFERENCES p1);
} {}
do_execsql_test 6.6 { 
  INSERT INTO p1 VALUES('abc'); 
  INSERT INTO c1 VALUES(1, 'ABC'); 
}
ifcapable foreignkey {
  do_catchsql_test 6.7 { 
    DELETE FROM p1 WHERE rowid = 1 
  } {1 {FOREIGN KEY constraint failed}}
}
do_execsql_test 6.8 { 
  INSERT INTO p1 VALUES('abb');
  INSERT INTO p1 VALUES('wxz');
  INSERT INTO p1 VALUES('wxy');

  INSERT INTO c1 VALUES(2, 'abb');
  INSERT INTO c1 VALUES(3, 'wxz');
  INSERT INTO c1 VALUES(4, 'WXY');
  SELECT x, y FROM c1 ORDER BY y COLLATE """""""";
} {2 abb 1 ABC 4 WXY 3 wxz}
do_execsql_test 7.0 {
   SELECT 'abc' UNION ALL SELECT 'DEF'
    ORDER BY 1 COLLATE nocase COLLATE nocase COLLATE nocase COLLATE nocase;
} {abc DEF}
do_execsql_test 7.1 {
   SELECT 'abc' UNION ALL SELECT 'DEF'
    ORDER BY 1 COLLATE nocase COLLATE nocase COLLATE nocase COLLATE binary;
} {DEF abc}
do_execsql_test 7.2 {
   SELECT 'abc' UNION ALL SELECT 'DEF'
    ORDER BY 1 COLLATE binary COLLATE binary COLLATE binary COLLATE nocase;
} {abc DEF}
do_execsql_test 8.0 {
  SELECT ' ' > char(20) COLLATE rtrim;
} 0
do_execsql_test 8.1 {
  SELECT '' < char(20) COLLATE rtrim;
} 1
do_execsql_test 8.2 {
  DROP TABLE IF EXISTS t0;
  CREATE TABLE t0(c0 COLLATE RTRIM, c1 BLOB UNIQUE,
                  PRIMARY KEY (c0, c1)) WITHOUT ROWID;
  INSERT INTO t0 VALUES (123, 3), (' ', 1), ('	', 2), ('', 4);
  SELECT * FROM t0 WHERE c1 = 1;
} {{ } 1}
do_execsql_test 9.0 {
  CREATE TABLE t1(a, b);
  CREATE TABLE t2(c, d);
}
do_faultsim_test 9.1 -faults oom* -body {
  execsql {
    SELECT * FROM (
        SELECT b COLLATE nocase IN (SELECT c FROM t2) FROM t1
    );
  }
} -test {
  faultsim_test_result {0 {}}
}
reset_db
do_catchsql_test 10.0 {
  CREATE TABLE t1(a INTEGER PRIMARY KEY,b);
  INSERT INTO t1 VALUES(0,NULL);
  CREATE TABLE t2(x UNIQUE);
  CREATE VIEW v1a(z,y) AS SELECT x COLLATE x FROM t2;
  SELECT a,b,z,y,'' FROM t1 JOIN v1a ON b IS NOT FALSE;
} {1 {no such collation sequence: x}}
finish_test
"#);
}
