#[test]
#[ignore = "22/54 subtests passed; pending: TRIGGER, ATTACH, VIRTUAL TABLE"]
fn test_alter4() {
    sqliters_tcltest::run_test_script("alter4", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable !altertable {
  finish_test
  return
}
do_test alter4-1.1 {
  execsql {
    CREATE TEMP TABLE abc(a, b, c);
    SELECT sql FROM sqlite_temp_master;
  }
} {{CREATE TABLE abc(a, b, c)}}
do_test alter4-1.1b {
  execsql {
    SELECT sql FROM temp.sqlite_master;
  }
} {{CREATE TABLE abc(a, b, c)}}
do_test alter4-1.2 {
  execsql {ALTER TABLE abc ADD d INTEGER;}
  execsql {
    SELECT sql FROM sqlite_temp_master;
  }
} {{CREATE TABLE abc(a, b, c, d INTEGER)}}
do_test alter4-1.2b {
  execsql {
    SELECT sql FROM temp.sqlite_master;
  }
} {{CREATE TABLE abc(a, b, c, d INTEGER)}}
do_test alter4-1.3 {
  execsql {ALTER TABLE abc ADD e}
  execsql {
    SELECT sql FROM sqlite_temp_master;
  }
} {{CREATE TABLE abc(a, b, c, d INTEGER, e)}}
do_test alter4-1.3b {
  execsql {
    SELECT sql FROM temp.sqlite_master;
  }
} {{CREATE TABLE abc(a, b, c, d INTEGER, e)}}
do_test alter4-1.4 {
  execsql {
    CREATE TABLE temp.t1(a, b);
    ALTER TABLE t1 ADD c;
    SELECT sql FROM sqlite_temp_master WHERE tbl_name = 't1';
  }
} {{CREATE TABLE t1(a, b, c)}}
do_test alter4-1.4b {
  execsql {
    SELECT sql FROM temp.sqlite_master WHERE tbl_name = 't1';
  }
} {{CREATE TABLE t1(a, b, c)}}
do_test alter4-1.5 {
  execsql {
    ALTER TABLE t1 ADD d CHECK (a>d);
    SELECT sql FROM sqlite_temp_master WHERE tbl_name = 't1';
  }
} {{CREATE TABLE t1(a, b, c, d CHECK (a>d))}}
ifcapable foreignkey {
  do_test alter4-1.6 {
    execsql {
      CREATE TEMP TABLE t2(a, b, UNIQUE(a, b));
      ALTER TABLE t2 ADD c REFERENCES t1(c)  ;
      SELECT sql FROM sqlite_temp_master
       WHERE tbl_name = 't2' AND type = 'table';
    }
  } {{CREATE TABLE t2(a, b, c REFERENCES t1(c), UNIQUE(a, b))}}
}
do_test alter4-1.7 {
  execsql {
    CREATE TEMPORARY TABLE t3(a, b, UNIQUE(a, b));
    ALTER TABLE t3 ADD COLUMN c VARCHAR(10, 20);
    SELECT sql FROM sqlite_temp_master
     WHERE tbl_name = 't3' AND type = 'table';
  }
} {{CREATE TABLE t3(a, b, c VARCHAR(10, 20), UNIQUE(a, b))}}
do_test alter4-1.99 {
  catchsql {
    DROP TABLE t2; 
  }
  execsql {
    DROP TABLE abc; 
    DROP TABLE t1; 
    DROP TABLE t3; 
  }
} {}
do_test alter4-2.1 {
  execsql {
    CREATE TABLE temp.t1(a, b);
    INSERT INTO t1 VALUES(1,2);
  }
  catchsql {
    ALTER TABLE t1 ADD c PRIMARY KEY;
  }
} {1 {Cannot add a PRIMARY KEY column}}
do_test alter4-2.2 {
  catchsql {
    ALTER TABLE t1 ADD c UNIQUE
  }
} {1 {Cannot add a UNIQUE column}}
do_test alter4-2.3 {
  catchsql {
    ALTER TABLE t1 ADD b VARCHAR(10)
  }
} {1 {duplicate column name: b}}
do_test alter4-2.3 {
  catchsql {
    ALTER TABLE t1 ADD c NOT NULL;
  }
} {1 {Cannot add a NOT NULL column with default value NULL}}
do_test alter4-2.4 {
  catchsql {
    ALTER TABLE t1 ADD c NOT NULL DEFAULT 10;
  }
} {0 {}}
ifcapable view {
  do_test alter4-2.5 {
    execsql {
      CREATE TEMPORARY VIEW v1 AS SELECT * FROM t1;
    }
    catchsql {
      alter table v1 add column d;
    }
  } {1 {Cannot add a column to a view}}
}
do_test alter4-2.6 {
  catchsql {
    alter table t1 add column d DEFAULT CURRENT_TIME;
  }
} {1 {Cannot add a column with non-constant default}}
do_test alter4-2.7 {
  catchsql {
    alter table t1 add column d default (-5+1);
  }
} {1 {Cannot add a column with non-constant default}}
do_test alter4-2.99 {
  execsql {
    DROP TABLE t1;
  }
} {}
do_test alter4-3.1 {
  execsql {
    CREATE TEMP TABLE t1(a, b);
    INSERT INTO t1 VALUES(1, 100);
    INSERT INTO t1 VALUES(2, 300);
    SELECT * FROM t1;
  }
} {1 100 2 300}
do_test alter4-3.1 {
  execsql {
    PRAGMA schema_version = 10;
  }
} {}
do_test alter4-3.2 {
  execsql {
    ALTER TABLE t1 ADD c;
    SELECT * FROM t1;
  }
} {1 100 {} 2 300 {}}
ifcapable schema_version {
  do_test alter4-3.4 {
    execsql {
      PRAGMA schema_version;
    }
  } {10}
}
do_test alter4-4.1 {
  db close
  forcedelete test.db
  set ::DB [sqlite3 db test.db]
  execsql {
    CREATE TEMP TABLE t1(a, b);
    INSERT INTO t1 VALUES(1, 100);
    INSERT INTO t1 VALUES(2, 300);
    SELECT * FROM t1;
  }
} {1 100 2 300}
do_test alter4-4.1 {
  execsql {
    PRAGMA schema_version = 20;
  }
} {}
do_test alter4-4.2 {
  execsql {
    ALTER TABLE t1 ADD c DEFAULT 'hello world';
    SELECT * FROM t1;
  }
} {1 100 {hello world} 2 300 {hello world}}
ifcapable schema_version {
  do_test alter4-4.4 {
    execsql {
      PRAGMA schema_version;
    }
  } {20}
}
do_test alter4-4.99 {
  execsql {
    DROP TABLE t1;
  }
} {}
ifcapable attach {
  do_test alter4-5.1 {
    forcedelete test2.db
    forcedelete test2.db-journal
    execsql {
      CREATE TEMP TABLE t1(a, b);
      INSERT INTO t1 VALUES(1, 'one');
      INSERT INTO t1 VALUES(2, 'two');
      ATTACH 'test2.db' AS aux;
      CREATE TABLE aux.t1 AS SELECT * FROM t1;
      PRAGMA aux.schema_version = 30;
      SELECT sql FROM aux.sqlite_master;
    } 
  } {{CREATE TABLE t1(a,b)}}
  do_test alter4-5.2 {
    execsql {
      ALTER TABLE aux.t1 ADD COLUMN c VARCHAR(128);
      SELECT sql FROM aux.sqlite_master;
    }
  } {{CREATE TABLE t1(a,b, c VARCHAR(128))}}
  do_test alter4-5.3 {
    execsql {
      SELECT * FROM aux.t1;
    }
  } {1 one {} 2 two {}}
  ifcapable schema_version {
    do_test alter4-5.4 {
      execsql {
        PRAGMA aux.schema_version;
      }
    } {31}
  }
  do_test alter4-5.6 {
    execsql {
      ALTER TABLE aux.t1 ADD COLUMN d DEFAULT 1000;
      SELECT sql FROM aux.sqlite_master;
    }
  } {{CREATE TABLE t1(a,b, c VARCHAR(128), d DEFAULT 1000)}}
  do_test alter4-5.7 {
    execsql {
      SELECT * FROM aux.t1;
    }
  } {1 one {} 1000 2 two {} 1000}
  ifcapable schema_version {
    do_test alter4-5.8 {
      execsql {
        PRAGMA aux.schema_version;
      }
    } {32}
  }
  do_test alter4-5.9 {
    execsql {
      SELECT * FROM t1;
    }
  } {1 one 2 two}
  do_test alter4-5.99 {
    execsql {
      DROP TABLE aux.t1;
      DROP TABLE t1;
    }
  } {}
}
ifcapable trigger&&tempdb {
  do_test alter4-6.1 {
    execsql {
      CREATE TEMP TABLE t1(a, b);
      CREATE TEMP TABLE log(trig, a, b);

      CREATE TRIGGER t1_a AFTER INSERT ON t1 BEGIN
        INSERT INTO log VALUES('a', new.a, new.b);
      END;
      CREATE TEMP TRIGGER t1_b AFTER INSERT ON t1 BEGIN
        INSERT INTO log VALUES('b', new.a, new.b);
      END;
  
      INSERT INTO t1 VALUES(1, 2);
      SELECT * FROM log ORDER BY trig, a, b;
    }
  } {a 1 2 b 1 2}
  do_test alter4-6.2 {
    execsql {
      ALTER TABLE t1 ADD COLUMN c DEFAULT 'c';
      INSERT INTO t1(a, b) VALUES(3, 4);
      SELECT * FROM log ORDER BY trig, a, b;
    }
  } {a 1 2 a 3 4 b 1 2 b 3 4}
}
do_test alter4-8.1 {
  execsql {
    CREATE TEMP TABLE t4(c1);
  }
} {}
set ::sql ""
do_test alter4-8.2 {
  set cols c1
  for {set i 2} {$i < 100} {incr i} {
    execsql "
      ALTER TABLE t4 ADD c$i
    "
    lappend cols c$i
  }
  set ::sql "CREATE TABLE t4([join $cols {, }])"
  list 
} {}
do_test alter4-8.2 {
  execsql {
    SELECT sql FROM sqlite_temp_master WHERE name = 't4';
  }
} [list $::sql]
do_execsql_test alter4-9.1 {
  CREATE TABLE t5(
    a INTEGER DEFAULT -9223372036854775808,
    b INTEGER DEFAULT (-(-9223372036854775808))
  );
  INSERT INTO t5 DEFAULT VALUES;
}
do_execsql_test alter4-9.2 { SELECT typeof(a), a, typeof(b), b FROM t5; } {
  integer -9223372036854775808
  real     9.22337203685478e+18
}
do_execsql_test alter4-9.3 { 
  ALTER TABLE t5 ADD COLUMN c INTEGER DEFAULT (-(-9223372036854775808));
  SELECT typeof(c), c FROM t5;
} {real 9.22337203685478e+18}
do_test alter4-10.1 {
  db close
  sqlite3 db :memory:
  sqlite3_db_config db LEGACY_FILE_FORMAT 1
  db eval {
    CREATE TABLE t1(a,b,c);
    CREATE INDEX t1a ON t1(a DESC);
    INSERT INTO t1 VALUES(1,2,3);
    INSERT INTO t1 VALUES(2,3,4);
    ALTER TABLE t1 ADD COLUMN d;
    PRAGMA integrity_check;
  }
} {ok}
reset_db
do_execsql_test alter4-11.0 {
  CREATE TABLE t1(c INTEGER PRIMARY KEY, d);
  INSERT INTO t1(c,d) VALUES(1,2);
  PRAGMA foreign_keys = on;
  ALTER TABLE t1 ADD COLUMN e;
}
do_execsql_test alter4-11.1 {
  ALTER TABLE t1 ADD COLUMN f REFERENCES t1;
}
do_catchsql_test alter4-11.2 {
  ALTER TABLE t1 ADD COLUMN g REFERENCES t1 DEFAULT 4;
} {1 {Cannot add a REFERENCES column with non-NULL default value}}
do_catchsql_test alter4-11.3 {
  ALTER TABLE t2 ADD COLUMN g;
} {1 {no such table: t2}}
ifcapable fts5 {
  do_execsql_test alter4-11.4 {
    CREATE VIRTUAL TABLE fff USING fts5(f);
  }
  do_catchsql_test alter4-11.2 {
    ALTER TABLE fff ADD COLUMN g;
  } {1 {virtual tables may not be altered}}
}
finish_test
"#);
}
