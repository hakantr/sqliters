#[test]
fn test_null() {
    sqliters_tcltest::run_test_script(
        "null",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test null-1.0 {
  execsql {
    begin;
    create table t1(a,b,c);
    insert into t1 values(1,0,0);
    insert into t1 values(2,0,1);
    insert into t1 values(3,1,0);
    insert into t1 values(4,1,1);
    insert into t1 values(5,null,0);
    insert into t1 values(6,null,1);
    insert into t1 values(7,null,null);
    commit;
    select * from t1;
  }
} {1 0 0 2 0 1 3 1 0 4 1 1 5 {} 0 6 {} 1 7 {} {}}
do_test null-1.1 {
  execsql {
    select ifnull(a+b,99) from t1;
  }
} {1 2 4 5 99 99 99}
do_test null-1.2 {
  execsql {
    select ifnull(b*c,99) from t1;
  }
} {0 0 0 1 99 99 99}
do_test null-2.1 {
  execsql {
    select ifnull(case when b<>0 then 1 else 0 end, 99) from t1;
  }
} {0 0 1 1 0 0 0}
do_test null-2.2 {
  execsql {
    select ifnull(case when not b<>0 then 1 else 0 end, 99) from t1;
  }
} {1 1 0 0 0 0 0}
do_test null-2.3 {
  execsql {
    select ifnull(case when b<>0 and c<>0 then 1 else 0 end, 99) from t1;
  }
} {0 0 0 1 0 0 0}
do_test null-2.4 {
  execsql {
    select ifnull(case when not (b<>0 and c<>0) then 1 else 0 end, 99) from t1;
  }
} {1 1 1 0 1 0 0}
do_test null-2.5 {
  execsql {
    select ifnull(case when b<>0 or c<>0 then 1 else 0 end, 99) from t1;
  }
} {0 1 1 1 0 1 0}
do_test null-2.6 {
  execsql {
    select ifnull(case when not (b<>0 or c<>0) then 1 else 0 end, 99) from t1;
  }
} {1 0 0 0 0 0 0}
do_test null-2.7 {
  execsql {
    select ifnull(case b when c then 1 else 0 end, 99) from t1;
  }
} {1 0 0 1 0 0 0}
do_test null-2.8 {
  execsql {
    select ifnull(case c when b then 1 else 0 end, 99) from t1;
  }
} {1 0 0 1 0 0 0}
do_test null-3.1 {
  execsql {
    select count(*), count(b), count(c), sum(b), sum(c), 
           avg(b), avg(c), min(b), max(b) from t1;
  }
} {7 4 6 2 3 0.5 0.5 0 1}
do_test null-3.2 {
  execsql {
    SELECT sum(b), total(b) FROM t1 WHERE b<0
  }
} {{} 0.0}
do_test null-4.1 {
  execsql {
    select a from t1 where b<10
  }
} {1 2 3 4}
do_test null-4.2 {
  execsql {
    select a from t1 where not b>10
  }
} {1 2 3 4}
do_test null-4.3 {
  execsql {
    select a from t1 where b<10 or c=1;
  }
} {1 2 3 4 6}
do_test null-4.4 {
  execsql {
    select a from t1 where b<10 and c=1;
  }
} {2 4}
do_test null-4.5 {
  execsql {
    select a from t1 where not (b<10 and c=1);
  }
} {1 3 5}
do_test null-5.1 {
  execsql {
    select distinct b from t1 order by b;
  }
} {{} 0 1}
ifcapable compound {
  do_test null-6.1 {
    execsql {
      select b from t1 union select c from t1 order by b;
    }
  } {{} 0 1}
  do_test null-6.2 {
    execsql {
      select b from t1 union select c from t1 order by 1;
    }
  } {{} 0 1}
  do_test null-6.3 {
    execsql {
      select b from t1 union select c from t1 order by t1.b;
    }
  } {{} 0 1}
  do_test null-6.4 {
    execsql {
      select b from t1 union select c from t1 order by main.t1.b;
    }
  } {{} 0 1}
  do_test null-6.5 {
    catchsql {
      select b from t1 union select c from t1 order by t1.a;
    }
  } {1 {1st ORDER BY term does not match any column in the result set}}
  do_test null-6.6 {
    catchsql {
      select b from t1 union select c from t1 order by main.t1.a;
    }
  } {1 {1st ORDER BY term does not match any column in the result set}}
} ;# ifcapable compound
ifcapable conflict {
do_test null-7.1 {
    execsql {
      create table t2(a, b unique on conflict ignore);
      insert into t2 values(1,1);
      insert into t2 values(2,null);
      insert into t2 values(3,null);
      insert into t2 values(4,1);
      select a from t2;
    }
  } {1 2 3}
  do_test null-7.2 {
    execsql {
      create table t3(a, b, c, unique(b,c) on conflict ignore);
      insert into t3 values(1,1,1);
      insert into t3 values(2,null,1);
      insert into t3 values(3,null,1);
      insert into t3 values(4,1,1);
      select a from t3;
    }
  } {1 2 3}
}
do_test null-8.1 {
  execsql {
    CREATE TABLE t4(x,y);
    INSERT INTO t4 VALUES(1,11);
    INSERT INTO t4 VALUES(2,NULL);
    SELECT x FROM t4 WHERE y=NULL;
  }
} {}
ifcapable subquery {
  do_test null-8.2 {
    execsql {
      SELECT x FROM t4 WHERE y IN (33,NULL);
    }
  } {}
}
do_test null-8.3 {
  execsql {
    SELECT x FROM t4 WHERE y<33 ORDER BY x;
  }
} {1}
do_test null-8.4 {
  execsql {
    SELECT x FROM t4 WHERE y>6 ORDER BY x;
  }
} {1}
do_test null-8.5 {
  execsql {
    SELECT x FROM t4 WHERE y!=33 ORDER BY x;
  }
} {1}
do_test null-8.11 {
  execsql {
    CREATE INDEX t4i1 ON t4(y);
    SELECT x FROM t4 WHERE y=NULL;
  }
} {}
ifcapable subquery {
  do_test null-8.12 {
    execsql {
      SELECT x FROM t4 WHERE y IN (33,NULL);
    }
  } {}
}
do_test null-8.13 {
  execsql {
    SELECT x FROM t4 WHERE y<33 ORDER BY x;
  }
} {1}
do_test null-8.14 {
  execsql {
    SELECT x FROM t4 WHERE y>6 ORDER BY x;
  }
} {1}
do_test null-8.15 {
  execsql {
    SELECT x FROM t4 WHERE y!=33 ORDER BY x;
  }
} {1}
do_execsql_test null-9.1 {
  CREATE TABLE t5(a, b, c);
  CREATE UNIQUE INDEX t5ab ON t5(a, b);

  INSERT INTO t5 VALUES(1, NULL, 'one');
  INSERT INTO t5 VALUES(1, NULL, 'i');
  INSERT INTO t5 VALUES(NULL, 'x', 'two');
  INSERT INTO t5 VALUES(NULL, 'x', 'ii');
}
do_execsql_test null-9.2 {
  SELECT * FROM t5 WHERE a = 1 AND b IS NULL;
} {1 {} one 1 {} i}
do_execsql_test null-9.3 {
  SELECT * FROM t5 WHERE a IS NULL AND b = 'x';
} {{} x two {} x ii}
reset_db
do_execsql_test null-10.1 {
  CREATE TABLE t0(c0 PRIMARY KEY DESC);
  INSERT INTO t0(c0) VALUES (0);
  SELECT * FROM t0 WHERE t0.c0 > NULL;
} {}
finish_test
"#,
    );
}
