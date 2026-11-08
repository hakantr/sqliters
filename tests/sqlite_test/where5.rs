#[test]
fn test_where5() {
    sqliters_tcltest::run_test_script(
        "where5",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_test where5-1.0 {
  execsql {
    CREATE TABLE t1(x TEXT);
    CREATE TABLE t2(x INTEGER);
    CREATE TABLE t3(x INTEGER PRIMARY KEY);
    INSERT INTO t1 VALUES(-1);
    INSERT INTO t1 VALUES(0);
    INSERT INTO t1 VALUES(1);
    INSERT INTO t2 SELECT * FROM t1;
    INSERT INTO t3 SELECT * FROM t2;
  }
  execsql {
    SELECT * FROM t1 WHERE x<0
  }
} {-1}
do_test where5-1.1 {
  execsql {
    SELECT * FROM t1 WHERE x<=0
  }
} {-1 0}
do_test where5-1.2 {
  execsql {
    SELECT * FROM t1 WHERE x=0
  }
} {0}
do_test where5-1.3 {
  execsql {
    SELECT * FROM t1 WHERE x>=0
  }
} {0 1}
do_test where5-1.4 {
  execsql {
    SELECT * FROM t1 WHERE x>0
  }
} {1}
do_test where5-1.5 {
  execsql {
    SELECT * FROM t1 WHERE x<>0
  }
} {-1 1}
do_test where5-1.6 {
  execsql {
    SELECT * FROM t1 WHERE x<NULL
  }
} {}
do_test where5-1.7 {
  execsql {
    SELECT * FROM t1 WHERE x<=NULL
  }
} {}
do_test where5-1.8 {
  execsql {
    SELECT * FROM t1 WHERE x=NULL
  }
} {}
do_test where5-1.9 {
  execsql {
    SELECT * FROM t1 WHERE x>=NULL
  }
} {}
do_test where5-1.10 {
  execsql {
    SELECT * FROM t1 WHERE x>NULL
  }
} {}
do_test where5-1.11 {
  execsql {
    SELECT * FROM t1 WHERE x!=NULL
  }
} {}
do_test where5-1.12 {
  execsql {
    SELECT * FROM t1 WHERE x IS NULL
  }
} {}
do_test where5-1.13 {
  execsql {
    SELECT * FROM t1 WHERE x IS NOT NULL
  }
} {-1 0 1}
do_test where5-2.0 {
  execsql {
    SELECT * FROM t2 WHERE x<0
  }
} {-1}
do_test where5-2.1 {
  execsql {
    SELECT * FROM t2 WHERE x<=0
  }
} {-1 0}
do_test where5-2.2 {
  execsql {
    SELECT * FROM t2 WHERE x=0
  }
} {0}
do_test where5-2.3 {
  execsql {
    SELECT * FROM t2 WHERE x>=0
  }
} {0 1}
do_test where5-2.4 {
  execsql {
    SELECT * FROM t2 WHERE x>0
  }
} {1}
do_test where5-2.5 {
  execsql {
    SELECT * FROM t2 WHERE x<>0
  }
} {-1 1}
do_test where5-2.6 {
  execsql {
    SELECT * FROM t2 WHERE x<NULL
  }
} {}
do_test where5-2.7 {
  execsql {
    SELECT * FROM t2 WHERE x<=NULL
  }
} {}
do_test where5-2.8 {
  execsql {
    SELECT * FROM t2 WHERE x=NULL
  }
} {}
do_test where5-2.9 {
  execsql {
    SELECT * FROM t2 WHERE x>=NULL
  }
} {}
do_test where5-2.10 {
  execsql {
    SELECT * FROM t2 WHERE x>NULL
  }
} {}
do_test where5-2.11 {
  execsql {
    SELECT * FROM t2 WHERE x!=NULL
  }
} {}
do_test where5-2.12 {
  execsql {
    SELECT * FROM t2 WHERE x IS NULL
  }
} {}
do_test where5-2.13 {
  execsql {
    SELECT * FROM t2 WHERE x IS NOT NULL
  }
} {-1 0 1}
do_test where5-3.0 {
  execsql {
    SELECT * FROM t3 WHERE x<0
  }
} {-1}
do_test where5-3.1 {
  execsql {
    SELECT * FROM t3 WHERE x<=0
  }
} {-1 0}
do_test where5-3.2 {
  execsql {
    SELECT * FROM t3 WHERE x=0
  }
} {0}
do_test where5-3.3 {
  execsql {
    SELECT * FROM t3 WHERE x>=0
  }
} {0 1}
do_test where5-3.4 {
  execsql {
    SELECT * FROM t3 WHERE x>0
  }
} {1}
do_test where5-3.5 {
  execsql {
    SELECT * FROM t3 WHERE x<>0
  }
} {-1 1}
do_test where5-3.6 {
  execsql {
    SELECT * FROM t3 WHERE x<NULL
  }
} {}
do_test where5-3.7 {
  execsql {
    SELECT * FROM t3 WHERE x<=NULL
  }
} {}
do_test where5-3.8 {
  execsql {
    SELECT * FROM t3 WHERE x=NULL
  }
} {}
do_test where5-3.9 {
  execsql {
    SELECT * FROM t3 WHERE x>=NULL
  }
} {}
do_test where5-3.10 {
  execsql {
    SELECT * FROM t3 WHERE x>NULL
  }
} {}
do_test where5-3.11 {
  execsql {
    SELECT * FROM t3 WHERE x!=NULL
  }
} {}
do_test where5-3.12 {
  execsql {
    SELECT * FROM t3 WHERE x IS NULL
  }
} {}
do_test where5-3.13 {
  execsql {
    SELECT * FROM t3 WHERE x IS NOT NULL
  }
} {-1 0 1}
do_test where5-4.0 {
  execsql {
    SELECT x<NULL FROM t3
  }
} {{} {} {}}
do_test where5-4.1 {
  execsql {
    SELECT x<=NULL FROM t3
  }
} {{} {} {}}
do_test where5-4.2 {
  execsql {
    SELECT x==NULL FROM t3
  }
} {{} {} {}}
do_test where5-4.3 {
  execsql {
    SELECT x>NULL FROM t3
  }
} {{} {} {}}
do_test where5-4.4 {
  execsql {
    SELECT x>=NULL FROM t3
  }
} {{} {} {}}
do_test where5-4.5 {
  execsql {
    SELECT x!=NULL FROM t3
  }
} {{} {} {}}
do_test where5-4.6 {
  execsql {
    SELECT x IS NULL FROM t3
  }
} {0 0 0}
do_test where5-4.7 {
  execsql {
    SELECT x IS NOT NULL FROM t3
  }
} {1 1 1}
finish_test
"#,
    );
}
