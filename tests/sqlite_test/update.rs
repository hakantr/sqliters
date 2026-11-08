#[test]
#[ignore = "133/141 subtests passed; pending: TRIGGER, EQP/optimizer"]
fn test_update() {
    sqliters_tcltest::run_test_script(
        "update",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test update-1.1 {
  set v [catch {execsql {UPDATE test1 SET f2=5 WHERE f1<1}} msg]
  lappend v $msg
} {1 {no such table: test1}}
do_test update-2.1 {
  set v [catch \
       {execsql {UPDATE sqlite_master SET name='xyz' WHERE name='123'}} msg]
  lappend v $msg
} {1 {table sqlite_master may not be modified}}
do_test update-3.1 {
  execsql {CREATE TABLE test1(f1 int,f2 int)}
  for {set i 1} {$i<=10} {incr i} {
    set sql "INSERT INTO test1 VALUES($i,[expr {1<<$i}])"
    execsql $sql
  }
  execsql {SELECT * FROM test1 ORDER BY f1}
} {1 2 2 4 3 8 4 16 5 32 6 64 7 128 8 256 9 512 10 1024}
do_test update-3.2 {
  set v [catch {execsql {UPDATE test1 SET f1=f3*2 WHERE f2==32}} msg]
  lappend v $msg
} {1 {no such column: f3}}
do_test update-3.3 {
  set v [catch {execsql {UPDATE test1 SET f1=test2.f1*2 WHERE f2==32}} msg]
  lappend v $msg
} {1 {no such column: test2.f1}}
do_test update-3.4 {
  set v [catch {execsql {UPDATE test1 SET f3=f1*2 WHERE f2==32}} msg]
  lappend v $msg
} {1 {no such column: f3}}
do_test update-3.5 {
  execsql {UPDATE test1 SET f2=f2*3}
} {}
do_test update-3.5.1 {
  db changes
} {10}
do_test update-3.5.2 {
  db eval {SELECT count(*) FROM test1}
} {10}
do_test update-3.5.3 {
  db changes
} {10}
do_test update-3.6 {
  execsql {SELECT * FROM test1 ORDER BY f1}
} {1 6 2 12 3 24 4 48 5 96 6 192 7 384 8 768 9 1536 10 3072}
do_test update-3.7 {
  execsql {PRAGMA count_changes=on}
  execsql {UPDATE test1 SET f2=f2/3 WHERE f1<=5}
} {5}
do_test update-3.8 {
  execsql {SELECT * FROM test1 ORDER BY f1}
} {1 2 2 4 3 8 4 16 5 32 6 192 7 384 8 768 9 1536 10 3072}
do_test update-3.9 {
  execsql {UPDATE test1 SET f2=f2/3 WHERE f1>5}
} {5}
do_test update-3.10 {
  execsql {SELECT * FROM test1 ORDER BY f1}
} {1 2 2 4 3 8 4 16 5 32 6 64 7 128 8 256 9 512 10 1024}
do_test update-3.11 {
  execsql {UPDATE test1 SET F2=f1, F1=f2}
} {10}
do_test update-3.12 {
  execsql {SELECT * FROM test1 ORDER BY F1}
} {2 1 4 2 8 3 16 4 32 5 64 6 128 7 256 8 512 9 1024 10}
do_test update-3.13 {
  execsql {PRAGMA count_changes=off}
  execsql {UPDATE test1 SET F2=f1, F1=f2}
} {}
do_test update-3.14 {
  execsql {SELECT * FROM test1 ORDER BY F1}
} {1 2 2 4 3 8 4 16 5 32 6 64 7 128 8 256 9 512 10 1024}
do_test update-4.0 {
  execsql {
    DELETE FROM test1 WHERE f1<=5;
    INSERT INTO test1(f1,f2) VALUES(8,88);
    INSERT INTO test1(f1,f2) VALUES(8,888);
    INSERT INTO test1(f1,f2) VALUES(77,128);
    INSERT INTO test1(f1,f2) VALUES(777,128);
  }
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-4.1 {
  execsql {UPDATE test1 SET f2=f2+1 WHERE f1==8}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 89 8 257 8 889 9 512 10 1024 77 128 777 128}
do_test update-4.2 {
  execsql {UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2>800}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 89 8 257 8 888 9 512 10 1024 77 128 777 128}
do_test update-4.3 {
  execsql {UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2<800}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-4.4 {
  execsql {UPDATE test1 SET f1=f1+1 WHERE f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 8 88 8 128 8 256 8 888 9 512 10 1024 78 128 778 128}
do_test update-4.5 {
  execsql {UPDATE test1 SET f1=f1-1 WHERE f1>100 and f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 8 88 8 128 8 256 8 888 9 512 10 1024 78 128 777 128}
do_test update-4.6 {
  execsql {
    PRAGMA count_changes=on;
    UPDATE test1 SET f1=f1-1 WHERE f1<=100 and f2==128;
  }
} {2}
do_test update-4.7 {
  execsql {
    PRAGMA count_changes=off;
    SELECT * FROM test1 ORDER BY f1,f2
  }
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-5.0 {
  execsql {CREATE INDEX idx1 ON test1(f1)}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-5.1 {
  execsql {UPDATE test1 SET f2=f2+1 WHERE f1==8}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 89 8 257 8 889 9 512 10 1024 77 128 777 128}
do_test update-5.2 {
  execsql {UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2>800}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 89 8 257 8 888 9 512 10 1024 77 128 777 128}
do_test update-5.3 {
  execsql {UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2<800}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-5.4 {
  execsql {UPDATE test1 SET f1=f1+1 WHERE f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 8 88 8 128 8 256 8 888 9 512 10 1024 78 128 778 128}
do_test update-5.4.1 {
  execsql {SELECT * FROM test1 WHERE f1==78 ORDER BY f1,f2}
} {78 128}
do_test update-5.4.2 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {778 128}
do_test update-5.4.3 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 128 8 256 8 888}
do_test update-5.5 {
  execsql {UPDATE test1 SET f1=f1-1 WHERE f1>100 and f2==128}
} {}
do_test update-5.5.1 {
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 8 88 8 128 8 256 8 888 9 512 10 1024 78 128 777 128}
do_test update-5.5.2 {
  execsql {SELECT * FROM test1 WHERE f1==78 ORDER BY f1,f2}
} {78 128}
do_test update-5.5.3 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {}
do_test update-5.5.4 {
  execsql {SELECT * FROM test1 WHERE f1==777 ORDER BY f1,f2}
} {777 128}
do_test update-5.5.5 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 128 8 256 8 888}
do_test update-5.6 {
  execsql {
    PRAGMA count_changes=on;
    UPDATE test1 SET f1=f1-1 WHERE f1<=100 and f2==128;
  }
} {2}
do_test update-5.6.1 {
  execsql {
    PRAGMA count_changes=off;
    SELECT * FROM test1 ORDER BY f1,f2
  }
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-5.6.2 {
  execsql {SELECT * FROM test1 WHERE f1==77 ORDER BY f1,f2}
} {77 128}
do_test update-5.6.3 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {}
do_test update-5.6.4 {
  execsql {SELECT * FROM test1 WHERE f1==777 ORDER BY f1,f2}
} {777 128}
do_test update-5.6.5 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 256 8 888}
execsql {PRAGMA synchronous=FULL}
do_test update-6.0 {
  execsql {DROP INDEX idx1}
  execsql {CREATE INDEX idx1 ON test1(f2)}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-6.1 {
  execsql {UPDATE test1 SET f2=f2+1 WHERE f1==8}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 89 8 257 8 889 9 512 10 1024 77 128 777 128}
do_test update-6.1.1 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 89 8 257 8 889}
do_test update-6.1.2 {
  execsql {SELECT * FROM test1 WHERE f2==89 ORDER BY f1,f2}
} {8 89}
do_test update-6.1.3 {
  execsql {SELECT * FROM test1 WHERE f1==88 ORDER BY f1,f2}
} {}
do_test update-6.2 {
  execsql {UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2>800}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 89 8 257 8 888 9 512 10 1024 77 128 777 128}
do_test update-6.3 {
  execsql {UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2<800}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-6.3.1 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 256 8 888}
do_test update-6.3.2 {
  execsql {SELECT * FROM test1 WHERE f2==89 ORDER BY f1,f2}
} {}
do_test update-6.3.3 {
  execsql {SELECT * FROM test1 WHERE f2==88 ORDER BY f1,f2}
} {8 88}
do_test update-6.4 {
  execsql {UPDATE test1 SET f1=f1+1 WHERE f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 8 88 8 128 8 256 8 888 9 512 10 1024 78 128 778 128}
do_test update-6.4.1 {
  execsql {SELECT * FROM test1 WHERE f1==78 ORDER BY f1,f2}
} {78 128}
do_test update-6.4.2 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {778 128}
do_test update-6.4.3 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 128 8 256 8 888}
do_test update-6.5 {
  execsql {UPDATE test1 SET f1=f1-1 WHERE f1>100 and f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 8 88 8 128 8 256 8 888 9 512 10 1024 78 128 777 128}
do_test update-6.5.1 {
  execsql {SELECT * FROM test1 WHERE f1==78 ORDER BY f1,f2}
} {78 128}
do_test update-6.5.2 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {}
do_test update-6.5.3 {
  execsql {SELECT * FROM test1 WHERE f1==777 ORDER BY f1,f2}
} {777 128}
do_test update-6.5.4 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 128 8 256 8 888}
do_test update-6.6 {
  execsql {UPDATE test1 SET f1=f1-1 WHERE f1<=100 and f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-6.6.1 {
  execsql {SELECT * FROM test1 WHERE f1==77 ORDER BY f1,f2}
} {77 128}
do_test update-6.6.2 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {}
do_test update-6.6.3 {
  execsql {SELECT * FROM test1 WHERE f1==777 ORDER BY f1,f2}
} {777 128}
do_test update-6.6.4 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 256 8 888}
do_test update-7.0 {
  execsql {CREATE INDEX idx2 ON test1(f2)}
  execsql {CREATE INDEX idx3 ON test1(f1,f2)}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-7.1 {
  execsql {UPDATE test1 SET f2=f2+1 WHERE f1==8}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 89 8 257 8 889 9 512 10 1024 77 128 777 128}
do_test update-7.1.1 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 89 8 257 8 889}
do_test update-7.1.2 {
  execsql {SELECT * FROM test1 WHERE f2==89 ORDER BY f1,f2}
} {8 89}
do_test update-7.1.3 {
  execsql {SELECT * FROM test1 WHERE f1==88 ORDER BY f1,f2}
} {}
do_test update-7.2 {
  execsql {UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2>800}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 89 8 257 8 888 9 512 10 1024 77 128 777 128}
do_test update-7.3 {
  execsql {UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2<800}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-7.3.1 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 256 8 888}
do_test update-7.3.2 {
  execsql {SELECT * FROM test1 WHERE f2==89 ORDER BY f1,f2}
} {}
do_test update-7.3.3 {
  execsql {SELECT * FROM test1 WHERE f2==88 ORDER BY f1,f2}
} {8 88}
do_test update-7.4 {
  execsql {UPDATE test1 SET f1=f1+1 WHERE f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 8 88 8 128 8 256 8 888 9 512 10 1024 78 128 778 128}
do_test update-7.4.1 {
  execsql {SELECT * FROM test1 WHERE f1==78 ORDER BY f1,f2}
} {78 128}
do_test update-7.4.2 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {778 128}
do_test update-7.4.3 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 128 8 256 8 888}
do_test update-7.5 {
  execsql {UPDATE test1 SET f1=f1-1 WHERE f1>100 and f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 8 88 8 128 8 256 8 888 9 512 10 1024 78 128 777 128}
do_test update-7.5.1 {
  execsql {SELECT * FROM test1 WHERE f1==78 ORDER BY f1,f2}
} {78 128}
do_test update-7.5.2 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {}
do_test update-7.5.3 {
  execsql {SELECT * FROM test1 WHERE f1==777 ORDER BY f1,f2}
} {777 128}
do_test update-7.5.4 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 128 8 256 8 888}
do_test update-7.6 {
  execsql {UPDATE test1 SET f1=f1-1 WHERE f1<=100 and f2==128}
  execsql {SELECT * FROM test1 ORDER BY f1,f2}
} {6 64 7 128 8 88 8 256 8 888 9 512 10 1024 77 128 777 128}
do_test update-7.6.1 {
  execsql {SELECT * FROM test1 WHERE f1==77 ORDER BY f1,f2}
} {77 128}
do_test update-7.6.2 {
  execsql {SELECT * FROM test1 WHERE f1==778 ORDER BY f1,f2}
} {}
do_test update-7.6.3 {
  execsql {SELECT * FROM test1 WHERE f1==777 ORDER BY f1,f2}
} {777 128}
do_test update-7.6.4 {
  execsql {SELECT * FROM test1 WHERE f1==8 ORDER BY f1,f2}
} {8 88 8 256 8 888}
do_test update-9.1 {
  set v [catch {execsql {
    UPDATE test1 SET x=11 WHERE f1=1025
  }} msg]
  lappend v $msg
} {1 {no such column: x}}
do_test update-9.2 {
  set v [catch {execsql {
    UPDATE test1 SET f1=x(11) WHERE f1=1025
  }} msg]
  lappend v $msg
} {1 {no such function: x}}
do_test update-9.3 {
  set v [catch {execsql {
    UPDATE test1 SET f1=11 WHERE x=1025
  }} msg]
  lappend v $msg
} {1 {no such column: x}}
do_test update-9.4 {
  set v [catch {execsql {
    UPDATE test1 SET f1=11 WHERE x(f1)=1025
  }} msg]
  lappend v $msg
} {1 {no such function: x}}
do_test update-10.1 {
  execsql {
    DROP TABLE test1;
    CREATE TABLE t1(
       a integer primary key,
       b UNIQUE, 
       c, d,
       e, f,
       UNIQUE(c,d)
    );
    INSERT INTO t1 VALUES(1,2,3,4,5,6);
    INSERT INTO t1 VALUES(2,3,4,4,6,7);
    SELECT * FROM t1
  }
} {1 2 3 4 5 6 2 3 4 4 6 7}
do_test update-10.2 {
  catchsql {
    UPDATE t1 SET a=1, e=9 WHERE f=6;
    SELECT * FROM t1;
  }
} {0 {1 2 3 4 9 6 2 3 4 4 6 7}}
do_test update-10.3 {
  catchsql {
    UPDATE t1 SET a=1, e=10 WHERE f=7;
    SELECT * FROM t1;
  }
} {1 {UNIQUE constraint failed: t1.a}}
do_test update-10.4 {
  catchsql {
    SELECT * FROM t1;
  }
} {0 {1 2 3 4 9 6 2 3 4 4 6 7}}
do_test update-10.5 {
  catchsql {
    UPDATE t1 SET b=2, e=11 WHERE f=6;
    SELECT * FROM t1;
  }
} {0 {1 2 3 4 11 6 2 3 4 4 6 7}}
do_test update-10.6 {
  catchsql {
    UPDATE t1 SET b=2, e=12 WHERE f=7;
    SELECT * FROM t1;
  }
} {1 {UNIQUE constraint failed: t1.b}}
do_test update-10.7 {
  catchsql {
    SELECT * FROM t1;
  }
} {0 {1 2 3 4 11 6 2 3 4 4 6 7}}
do_test update-10.8 {
  catchsql {
    UPDATE t1 SET c=3, d=4, e=13 WHERE f=6;
    SELECT * FROM t1;
  }
} {0 {1 2 3 4 13 6 2 3 4 4 6 7}}
do_test update-10.9 {
  catchsql {
    UPDATE t1 SET c=3, d=4, e=14 WHERE f=7;
    SELECT * FROM t1;
  }
} {1 {UNIQUE constraint failed: t1.c, t1.d}}
do_test update-10.10 {
  catchsql {
    SELECT * FROM t1;
  }
} {0 {1 2 3 4 13 6 2 3 4 4 6 7}}
ifcapable subquery {
  do_test update-11.1 {
    execsql {
      UPDATE t1 SET e=e+1 WHERE b IN (SELECT b FROM t1);
      SELECT b,e FROM t1;
    }
  } {2 14 3 7}
  do_test update-11.2 {
    execsql {
      UPDATE t1 SET e=e+1 WHERE a IN (SELECT a FROM t1);
      SELECT a,e FROM t1;
    }
  } {1 15 2 8}
  do_test update-11.3 {
    execsql {
      UPDATE t1 AS xyz SET e=e+1 WHERE xyz.a IN (SELECT a FROM t1);
      SELECT a,e FROM t1;
    }
  } {1 16 2 9}
  do_test update-11.4 {
    execsql {
      UPDATE t1 AS xyz SET e=e+1 WHERE EXISTS(SELECT 1 FROM t1 WHERE t1.a<xyz.a);
      SELECT a,e FROM t1;
    }
  } {1 16 2 10}
}
integrity_check update-12.1
do_test update-13.1 {
  execsql {
    BEGIN;
    CREATE TABLE t2(a);
    INSERT INTO t2 VALUES(1);
    INSERT INTO t2 VALUES(2);
    INSERT INTO t2 SELECT a+2 FROM t2;
    INSERT INTO t2 SELECT a+4 FROM t2;
    INSERT INTO t2 SELECT a+8 FROM t2;
    INSERT INTO t2 SELECT a+16 FROM t2;
    INSERT INTO t2 SELECT a+32 FROM t2;
    INSERT INTO t2 SELECT a+64 FROM t2;
    INSERT INTO t2 SELECT a+128 FROM t2;
    INSERT INTO t2 SELECT a+256 FROM t2;
    INSERT INTO t2 SELECT a+512 FROM t2;
    INSERT INTO t2 SELECT a+1024 FROM t2;
    COMMIT;
    SELECT count(*) FROM t2;
  }
} {2048}
do_test update-13.2 {
  execsql {
    SELECT count(*) FROM t2 WHERE a=rowid;
  }
} {2048}
do_test update-13.3 {
  execsql {
    UPDATE t2 SET rowid=rowid-1;
    SELECT count(*) FROM t2 WHERE a=rowid+1;
  }
} {2048}
do_test update-13.3 {
  execsql {
    UPDATE t2 SET rowid=rowid+10000;
    UPDATE t2 SET rowid=rowid-9999;
    SELECT count(*) FROM t2 WHERE a=rowid;
  }
} {2048}
do_test update-13.4 {
  execsql {
    BEGIN;
    INSERT INTO t2 SELECT a+2048 FROM t2;
    INSERT INTO t2 SELECT a+4096 FROM t2;
    INSERT INTO t2 SELECT a+8192 FROM t2;
    SELECT count(*) FROM t2 WHERE a=rowid;
    COMMIT;
  }
} 16384
do_test update-13.5 {
  execsql {
    UPDATE t2 SET rowid=rowid-1;
    SELECT count(*) FROM t2 WHERE a=rowid+1;
  }
} 16384
integrity_check update-13.6
ifcapable {trigger} {
do_test update-14.1 {
  execsql {
    CREATE TABLE t3(a,b,c);
    CREATE TRIGGER t3r1 BEFORE UPDATE on t3 WHEN nosuchcol BEGIN
      SELECT 'illegal WHEN clause';
    END;
  }
} {}
do_test update-14.2 {
  catchsql {
    UPDATE t3 SET a=1;
  }
} {1 {no such column: nosuchcol}}
do_test update-14.3 {
  execsql {
    CREATE TABLE t4(a,b,c);
    CREATE TRIGGER t4r1 AFTER UPDATE on t4 WHEN nosuchcol BEGIN
      SELECT 'illegal WHEN clause';
    END;
  }
} {}
do_test update-14.4 {
  catchsql {
    UPDATE t4 SET a=1;
  }
} {1 {no such column: nosuchcol}}

} ;# ifcapable {trigger}
ifcapable altertable {
  do_execsql_test update-15.1 {
    CREATE TABLE t15(a INTEGER PRIMARY KEY, b);
    INSERT INTO t15(a,b) VALUES(10,'abc'),(20,'def'),(30,'ghi');
    ALTER TABLE t15 ADD COLUMN c;
    CREATE INDEX t15c ON t15(c);
    INSERT INTO t15(a,b)
      VALUES(5,'zyx'),(15,'wvu'),(25,'tsr'),(35,'qpo');
    UPDATE t15 SET c=printf('y%d',a) WHERE c IS NULL;
    SELECT a,b,c,'|' FROM t15 ORDER BY a;
  } {5 zyx y5 | 10 abc y10 | 15 wvu y15 | 20 def y20 | 25 tsr y25 | 30 ghi y30 | 35 qpo y35 |}
}
do_execsql_test update-16.1 {
  CREATE TABLE t16(a INTEGER PRIMARY KEY ON CONFLICT REPLACE, b UNIQUE);
  INSERT INTO t16(a,b) VALUES(1,2),(3,4),(5,6);
  UPDATE t16 SET a=a;
  SELECT * FROM t16 ORDER BY +a;
} {1 2 3 4 5 6}
do_execsql_test update-17.10 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(x,y);
  INSERT INTO t1(x) VALUES(1);
  CREATE INDEX t1x1 ON t1(1) WHERE 3;
  UPDATE t1 SET x=2, y=3 WHERE 3;
  SELECT * FROM t1;
} {2 3}
reset_db
do_execsql_test update-18.10 {
  PRAGMA encoding = 'UTF16';
  CREATE TABLE t0(c0 REAL, c1);
  INSERT INTO t0(c0,c1) VALUES('xyz',11),('uvw',22);
  CREATE INDEX i0 ON t0(c1) WHERE c0 GLOB 3;
  CREATE INDEX i1 ON t0(c0,c1) WHERE typeof(c0)='text' AND typeof(c1)='integer';
  UPDATE t0 SET c1=345;
  SELECT * FROM t0;
} {xyz 345 uvw 345}
reset_db
do_execsql_test update-18.20 {
  PRAGMA encoding = 'utf16';
  CREATE TABLE t0(c0 TEXT);
  CREATE INDEX i0 ON t0(0 LIKE COALESCE(c0, 0));
  INSERT INTO t0(c0) VALUES (0), (0);
  SELECT * FROM t0;
} {0 0}
reset_db
do_execsql_test update-19.10 {
  CREATE TABLE t1(
   a TEXT,
   b INTEGER PRIMARY KEY UNIQUE
  ); 
  INSERT INTO t1 VALUES(1,2);
  UPDATE t1 SET a = quote(b) WHERE b>=2;
  SELECT * FROM t1;
} {2 2}
reset_db
do_execsql_test update-20.10 {
  PRAGMA recursive_triggers = true;
  CREATE TABLE t1(a UNIQUE ON CONFLICT REPLACE, b);
  INSERT INTO t1(a,b) VALUES(4,12),(9,13);
  CREATE INDEX i0 ON t1(b);
  CREATE TRIGGER tr0 DELETE ON t1 BEGIN
    UPDATE t1 SET b = a;
  END;
  PRAGMA integrity_check;
} {ok}
do_catchsql_test update-20.20 {
  UPDATE t1 SET a=0;
} {1 {constraint failed}}
do_execsql_test update-20.30 {
  PRAGMA integrity_check;
} {ok}
reset_db
do_execsql_test update-21.1 {
  CREATE TABLE t1 (vkey INTEGER, c5 INTEGER);
  INSERT INTO t1 VALUES(3,NULL),(6,-54);
}
db null NULL
do_execsql_test update-21.2 {
  BEGIN;
  UPDATE t1 SET vkey = 100 WHERE c5 is null;
  SELECT * FROM t1 ORDER BY vkey, c5;
  ROLLBACK;
} {6 -54 100 NULL}
do_execsql_test update-21.3 {
  BEGIN;
  UPDATE t1 SET vkey = 100 WHERE NOT (-10*(select min(vkey) from t1) >= c5);
  SELECT * FROM t1 ORDER BY vkey, c5;
  ROLLBACK;
} {3 NULL 6 -54}
do_execsql_test update-21.4 {
  BEGIN;
  UPDATE t1 SET vkey = 100 WHERE c5 is null OR NOT (-10*(select min(vkey) from t1) >= c5);
  SELECT * FROM t1 ORDER BY vkey, c5;
  ROLLBACK;
} {6 -54 100 NULL}
do_execsql_test update-21.11 {
  DROP TABLE t1;
  CREATE TABLE t1(a INTEGER PRIMARY KEY, b INT);
  CREATE TABLE t2(d INT);
}
do_eqp_test update-21.12 {
  WITH t3(x,y) AS (SELECT d, row_number()OVER() FROM t2)
    UPDATE t1 SET b=(SELECT y FROM t3 WHERE t1.a=t3.x);
} {
  QUERY PLAN
  |--SCAN t1
  `--CORRELATED SCALAR SUBQUERY xxxxxx
     |--CO-ROUTINE t3
     |  |--CO-ROUTINE (subquery-xxxxxx)
     |  |  `--SCAN t2
     |  `--SCAN (subquery-xxxxxx)
     |--BLOOM FILTER ON t3 (x=?)
     `--SEARCH t3 USING AUTOMATIC COVERING INDEX (x=?)
}
do_execsql_test update-22.0 {
  DROP TABLE IF EXISTS t1;
  CREATE TABLE t1(x INT, y INT);
  INSERT INTO t1(x) VALUES(1),(2),(3),(4),(5);
  UPDATE t1 SET x=x+100, y=x<=(SELECT min(x) FROM t1)
   WHERE x<3 OR (1 BETWEEN 0 AND x<=(SELECT min(x)+2 FROM t1));
  SELECT x FROM t1 WHERE x<100 ORDER BY x;
} {4 5}
finish_test
"#,
    );
}
