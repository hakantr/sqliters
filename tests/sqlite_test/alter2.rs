#![forbid(unsafe_code)]

#[test]
#[ignore = "31/36 subtests passed; pending: Tcl UDF"]
fn test_alter2() {
    sqliters_tcltest::run_test_script(
        "alter2",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable {!pragma} return
do_not_use_codec
proc set_file_format {newval} {
  hexio_write test.db 44 [hexio_render_int32 $newval]
  set schemacookie [hexio_get_int [hexio_read test.db 40 4]]
  incr schemacookie
  hexio_write test.db 40 [hexio_render_int32 $schemacookie]
  return {}
}
proc get_file_format {{fname test.db}} {
  return [hexio_get_int [hexio_read $fname 44 4]]
}
proc alter_table {tbl sql {file_format 2}} {
  sqlite3 dbat test.db
  set s [string map {' ''} $sql]
  set t [string map {' ''} $tbl]
  sqlite3_db_config dbat DEFENSIVE 0
  dbat eval [subst {
    PRAGMA writable_schema = 1;
    UPDATE sqlite_master SET sql = '$s' WHERE name = '$t' AND type = 'table';
    PRAGMA writable_schema = 0;
  }]
  dbat close
  set_file_format 2
}
proc failing_app_func {args} {error "bad function"}
do_test alter2-1.0 {
  db func substr failing_app_func
  db func like failing_app_func
  db func sqlite_rename_table failing_app_func
  db func sqlite_rename_trigger failing_app_func
  db func sqlite_rename_parent failing_app_func
  catchsql {SELECT substr('abcdefg',1,3)}
} {1 {bad function}}
sqlite3_db_config db DEFENSIVE 0
do_test alter2-1.1 {
  execsql {
    CREATE TABLE abc(a, b);
    INSERT INTO abc VALUES(1, 2);
    INSERT INTO abc VALUES(3, 4);
    INSERT INTO abc VALUES(5, 6);
  }
} {}
do_test alter2-1.2 {
  alter_table abc {CREATE TABLE abc(a, b, c);}
} {}
do_test alter2-1.3 {
  execsql {
    SELECT * FROM abc;
  }
} {1 2 {} 3 4 {} 5 6 {}}
do_test alter2-1.4 {
  execsql {
    UPDATE abc SET c = 10 WHERE a = 1;
    SELECT * FROM abc;
  }
} {1 2 10 3 4 {} 5 6 {}}
do_test alter2-1.5 {
  execsql {
    CREATE INDEX abc_i ON abc(c);
  }
} {}
do_test alter2-1.6 {
  execsql {
    SELECT c FROM abc ORDER BY c;
  }
} {{} {} 10}
do_test alter2-1.7 {
  execsql {
    SELECT * FROM abc WHERE c = 10;
  }
} {1 2 10}
do_test alter2-1.8 {
  execsql {
    SELECT sum(a), c FROM abc GROUP BY c;
  }
} {8 {} 1 10}
do_test alter2-1.9 {
  alter_table abc {CREATE TABLE abc(a, b, c, d);}
  if {[permutation] == "prepare"} { db cache flush }
  execsql { SELECT * FROM abc; }
  execsql {
    UPDATE abc SET d = 11 WHERE c IS NULL AND a<4;
    SELECT * FROM abc;
  }
} {1 2 10 {} 3 4 {} 11 5 6 {} {}}
do_test alter2-1.10 {
  execsql {
    SELECT typeof(d) FROM abc;
  }
} {null integer null}
do_test alter2-1.99 {
  execsql {
    DROP TABLE abc;
  }
} {}
ifcapable view {
  do_test alter2-2.1 {
    execsql {
      CREATE TABLE abc2(a, b, c);
      INSERT INTO abc2 VALUES(1, 2, 10);
      INSERT INTO abc2 VALUES(3, 4, NULL);
      INSERT INTO abc2 VALUES(5, 6, NULL);
      CREATE VIEW abc2_v AS SELECT * FROM abc2;
      SELECT * FROM abc2_v;
    }
  } {1 2 10 3 4 {} 5 6 {}}
  do_test alter2-2.2 {
    alter_table abc2 {CREATE TABLE abc2(a, b, c, d);}
    execsql {
      SELECT * FROM abc2_v;
    }
  } {1 2 10 {} 3 4 {} {} 5 6 {} {}}
  do_test alter2-2.3 {
    execsql {
      DROP TABLE abc2;
      DROP VIEW abc2_v;
    }
  } {}
}
ifcapable trigger {
  do_test alter2-3.1 {
    execsql {
      CREATE TABLE abc3(a, b);
      CREATE TABLE blog(o, n);
      CREATE TRIGGER abc3_t AFTER UPDATE OF b ON abc3 BEGIN
        INSERT INTO blog VALUES(old.b, new.b);
      END;
    }
  } {}
  do_test alter2-3.2 {
    execsql {
      INSERT INTO abc3 VALUES(1, 4);
      UPDATE abc3 SET b = 2 WHERE b = 4;
      SELECT * FROM blog;
    }
  } {4 2}
  do_test alter2-3.3 {
    execsql {
      INSERT INTO abc3 VALUES(3, 4);
      INSERT INTO abc3 VALUES(5, 6);
    }
    alter_table abc3 {CREATE TABLE abc3(a, b, c);}
    execsql {
      SELECT * FROM abc3;
    }
  } {1 2 {} 3 4 {} 5 6 {}}
  do_test alter2-3.4 {
    execsql {
      UPDATE abc3 SET b = b*2 WHERE a<4;
      SELECT * FROM abc3;
    }
  } {1 4 {} 3 8 {} 5 6 {}}
  do_test alter2-3.5 {
    execsql {
      SELECT * FROM blog;
    }
  } {4 2 2 4 4 8}

  do_test alter2-3.6 {
    execsql {
      CREATE TABLE clog(o, n);
      CREATE TRIGGER abc3_t2 AFTER UPDATE OF c ON abc3 BEGIN
        INSERT INTO clog VALUES(old.c, new.c);
      END;
      UPDATE abc3 SET c = a*2;
      SELECT * FROM clog;
    }
  } {{} 2 {} 6 {} 10}
} else {
  execsql { CREATE TABLE abc3(a, b); }
}
do_test alter2-4.1 {
  db close
  set_file_format 5
  catch { sqlite3 db test.db }
  set {} {}
} {}
do_test alter2-4.2 {
  catchsql { SELECT * FROM sqlite_master; }
  catchsql { SELECT * FROM sqlite_master; }
} {1 {unsupported file format}}
do_test alter2-4.3 {
  sqlite3_errcode db
} {SQLITE_ERROR}
do_test alter2-4.4 {
  set ::DB [sqlite3_connection_pointer db]
  catchsql {
    SELECT * FROM sqlite_master;
  }
} {1 {unsupported file format}}
do_test alter2-4.5 {
  sqlite3_errcode db
} {SQLITE_ERROR}
set default_file_format [expr $SQLITE_DEFAULT_FILE_FORMAT==4 ? 4 : 1]
ifcapable vacuum {
  do_test alter2-5.1 {
    set_file_format 2
    db close
    sqlite3 db test.db
    execsql {SELECT 1 FROM sqlite_master LIMIT 1;}
    get_file_format
  } {2}
  do_test alter2-5.2 {
    execsql { VACUUM }
  } {}
  do_test alter2-5.3 {
    get_file_format
  } $default_file_format
}
do_test alter2-6.1 {
  db close
  set_file_format 2
  sqlite3 db test.db
  get_file_format
} {2}
ifcapable attach {
  do_test alter2-6.2 {
    forcedelete test2.db-journal
    forcedelete test2.db
    execsql {
      ATTACH 'test2.db' AS aux;
      CREATE TABLE aux.t1(a, b);
    }
    get_file_format test2.db
  } $default_file_format
}
do_test alter2-6.3 {
  execsql {
    CREATE TABLE t1(a, b);
  }
  get_file_format 
} {2}
do_test alter2-7.1 {
  execsql {
    DROP TABLE t1;
    CREATE TABLE t1(a);
    INSERT INTO t1 VALUES(1);
    INSERT INTO t1 VALUES(2);
    INSERT INTO t1 VALUES(3);
    INSERT INTO t1 VALUES(4);
    SELECT * FROM t1;
  }
} {1 2 3 4}
do_test alter2-7.2 {
  set sql {CREATE TABLE t1(a, b DEFAULT '123', c INTEGER DEFAULT '123')}
  alter_table t1 $sql 3
  execsql {
    SELECT * FROM t1 LIMIT 1;
  }
} {1 123 123}
do_test alter2-7.3 {
  execsql {
    SELECT a, typeof(a), b, typeof(b), c, typeof(c) FROM t1 LIMIT 1;
  }
} {1 integer 123 text 123 integer}
do_test alter2-7.4 {
  execsql {
    SELECT a, typeof(a), b, typeof(b), c, typeof(c) FROM t1 LIMIT 1;
  }
} {1 integer 123 text 123 integer}
do_test alter2-7.5 {
  set sql {CREATE TABLE t1(a, b DEFAULT -123.0, c VARCHAR(10) default 5)}
  alter_table t1 $sql 3
  execsql {
    SELECT a, typeof(a), b, typeof(b), c, typeof(c) FROM t1 LIMIT 1;
  }
} {1 integer -123.0 real 5 text}
ifcapable trigger {
db function set_val {set ::val}
  do_test alter2-8.1 {
    execsql {
      CREATE TRIGGER trig1 BEFORE UPDATE ON t1 BEGIN
      SELECT set_val(
          old.b||' '||typeof(old.b)||' '||old.c||' '||typeof(old.c)||' '||
          new.b||' '||typeof(new.b)||' '||new.c||' '||typeof(new.c) 
      );
      END;
    }
    list
  } {}
}
do_test alter2-8.2 {
  execsql {
    UPDATE t1 SET c = 10 WHERE a = 1;
    SELECT a, typeof(a), b, typeof(b), c, typeof(c) FROM t1 LIMIT 1;
  }
} {1 integer -123.0 real 10 text}
ifcapable trigger {
  do_test alter2-8.3 {
    set ::val
  } {-123.0 real 5 text -123.0 real 10 text}
}
ifcapable trigger {
  do_test alter2-9.1 {
    execsql {
      CREATE TRIGGER trig2 BEFORE DELETE ON t1 BEGIN
      SELECT set_val(
          old.b||' '||typeof(old.b)||' '||old.c||' '||typeof(old.c)
      );
      END;
    }
    list
  } {}
  do_test alter2-9.2 {
    execsql {
      DELETE FROM t1 WHERE a = 2;
    }
    set ::val
  } {-123.0 real 5 text}
}
ifcapable bloblit {
  do_test alter2-10.1 {
    execsql {
      CREATE TABLE t2(a);
      INSERT INTO t2 VALUES('a');
      INSERT INTO t2 VALUES('b');
      INSERT INTO t2 VALUES('c');
      INSERT INTO t2 VALUES('d');
    }
    alter_table t2 {CREATE TABLE t2(a, b DEFAULT X'ABCD', c DEFAULT NULL);} 3
    catchsql {
      SELECT * FROM sqlite_master;
    }
    execsql {
      SELECT quote(a), quote(b), quote(c) FROM t2 LIMIT 1;
    }
  } {'a' X'ABCD' NULL}
  do_test alter2-10.2 {
    execsql {
      CREATE INDEX i1 ON t2(b);
      SELECT a FROM t2 WHERE b = X'ABCD';
    }
  } {a b c d}
  do_test alter2-10.3 {
    execsql {
      DELETE FROM t2 WHERE a = 'c';
      SELECT a FROM t2 WHERE b = X'ABCD';
    }
  } {a b d}
  do_test alter2-10.4 {
    execsql {
      SELECT count(b) FROM t2 WHERE b = X'ABCD';
    }
  } {3}
}
finish_test
"#,
    );
}
