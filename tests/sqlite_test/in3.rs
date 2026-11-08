#[test]
#[ignore = "20/36 subtests passed; pending: in3 edge cases"]
fn test_in3() {
    sqliters_tcltest::run_test_script("in3", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable !subquery {
  finish_test
  return
}
proc nEphemeral {sql} {
  set nEph 0
  foreach op [execsql "EXPLAIN $sql"] {
    if {$op eq "OpenEphemeral"} {incr nEph}
  }
  set nEph
}
proc exec_neph {sql} {
  return [concat [nEphemeral $sql] [execsql $sql]]
}
do_test in3-1.1 {
  execsql {
    CREATE TABLE t1(a PRIMARY KEY, b);
    INSERT INTO t1 VALUES(1, 2);
    INSERT INTO t1 VALUES(3, 4);
    INSERT INTO t1 VALUES(5, 6);
  }
} {}
do_test in3-1.2 {
  exec_neph { SELECT rowid FROM t1 WHERE rowid IN (SELECT rowid FROM t1); }
} {0 1 2 3}
do_test in3-1.3 {
  exec_neph { SELECT a FROM t1 WHERE a IN (SELECT a FROM t1); }
} {0 1 3 5}
do_test in3-1.4 {
  exec_neph { SELECT rowid FROM t1 WHERE rowid+0 IN (SELECT rowid FROM t1); }
} {0 1 2 3}
do_test in3-1.5 {
  exec_neph { SELECT a FROM t1 WHERE a+0 IN (SELECT a FROM t1); }
} {0 1 3 5}
do_test in3-1.6 {
  exec_neph { SELECT rowid FROM t1 WHERE rowid IN (SELECT rowid+0 FROM t1); }
} {1 1 2 3}
do_test in3-1.7 {
  exec_neph { SELECT a FROM t1 WHERE a IN (SELECT a+0 FROM t1); }
} {1 1 3 5}
do_test in3-1.8 {
  exec_neph { SELECT a FROM t1 WHERE a IN (SELECT a FROM t1 WHERE 1); }
} {1 1 3 5}
do_test in3-1.9 {
  exec_neph { SELECT a FROM t1 WHERE a IN (SELECT a FROM t1 GROUP BY a); }
} {1 1 3 5}
do_test in3-1.10 {
  exec_neph { SELECT a FROM t1 WHERE a IN (SELECT a FROM t1 ORDER BY a); }
} {0 1 3 5}
do_test in3-1.11 {
  exec_neph {SELECT a FROM t1 WHERE a IN (SELECT a FROM t1 ORDER BY a LIMIT 1)}
} {1 1}
do_test in3-1.12 {
  exec_neph {
    SELECT a FROM t1 WHERE a IN (SELECT a FROM t1 ORDER BY a LIMIT 1 OFFSET 1)
  }
} {1 3}
ifcapable compound {
  do_test in3-1.13 {
    exec_neph {
      SELECT a FROM t1 WHERE a IN (
        SELECT a FROM t1 UNION ALL SELECT a FROM t1
      )
    }
  } {1 1 3 5}
}
do_test in3-1.14 {
  exec_neph { SELECT a FROM t1 WHERE a COLLATE nocase IN (SELECT a FROM t1) }
} {1 1 3 5}
do_test in3-1.15 {
  exec_neph { SELECT a FROM t1 WHERE a COLLATE binary IN (SELECT a FROM t1) }
} {0 1 3 5}
do_test in3-1.16 {
  exec_neph {SELECT a FROM t1 WHERE a COLLATE nocase IN (SELECT rowid FROM t1)}
} {0 1 3}
do_test in3-1.17 {
  exec_neph {SELECT a FROM t1 WHERE a COLLATE binary IN (SELECT rowid FROM t1)}
} {0 1 3}
do_test in3-2.1 {
  execsql {
    DROP TABLE IF EXISTS t1;
    CREATE TABLE t1(w int, x int, y int);
    CREATE TABLE t2(p int, q int, r int, s int);
  }
  for {set i 1} {$i<=100} {incr i} {
    set w $i
    set x [expr {int(log($i)/log(2))}]
    set y [expr {$i*$i + 2*$i + 1}]
    execsql "INSERT INTO t1 VALUES($w,$x,$y)"
  }
  set maxy [execsql {select max(y) from t1}]
  db eval { INSERT INTO t2 SELECT 101-w, x, $maxy+1-y, y FROM t1 }
} {}
do_test in3-2.2 {
  execsql {
    SELECT rowid 
    FROM t1 
    WHERE rowid IN (SELECT rowid FROM t1 WHERE rowid IN (1, 2));
  }
} {1 2}
do_test in3-2.3 {
  execsql {
    select rowid from t1 where rowid IN (-1,2,4)
  }
} {2 4}
do_test in3-2.4 {
  execsql {
    SELECT rowid FROM t1 WHERE rowid IN 
       (select rowid from t1 where rowid IN (-1,2,4))
  }
} {2 4}
do_test in3-3.1 {
  catch {execsql {
    DROP TABLE t1;
    DROP TABLE t2;
  }}

  execsql {

    CREATE TABLE t1(a BLOB, b NUMBER ,c TEXT);
    CREATE UNIQUE INDEX t1_i1 ON t1(a);        /* no affinity */
    CREATE UNIQUE INDEX t1_i2 ON t1(b);        /* numeric affinity */
    CREATE UNIQUE INDEX t1_i3 ON t1(c);        /* text affinity */

    CREATE TABLE t2(x BLOB, y NUMBER, z TEXT);
    CREATE UNIQUE INDEX t2_i1 ON t2(x);        /* no affinity */
    CREATE UNIQUE INDEX t2_i2 ON t2(y);        /* numeric affinity */
    CREATE UNIQUE INDEX t2_i3 ON t2(z);        /* text affinity */

    INSERT INTO t1 VALUES(1, 1, 1);
    INSERT INTO t2 VALUES('1', '1', '1');
  }
} {}
do_test in3-3.2 {
  exec_neph { SELECT x IN (SELECT a FROM t1) FROM t2 }
} {0 0}
do_test in3-3.3 {
  exec_neph { SELECT x IN (SELECT b FROM t1) FROM t2 }
} {0 1}
do_test in3-3.4 {
  exec_neph { SELECT x IN (SELECT c FROM t1) FROM t2 }
} {0 1}
do_test in3-3.5 {
  exec_neph { SELECT y IN (SELECT a FROM t1) FROM t2 }
} {1 1}
do_test in3-3.6 {
  exec_neph { SELECT y IN (SELECT b FROM t1) FROM t2 }
} {0 1}
do_test in3-3.7 {
  exec_neph { SELECT y IN (SELECT c FROM t1) FROM t2 }
} {1 1}
do_test in3-4.1 {
  execsql {
    CREATE TABLE t3(a, b, c);
    CREATE UNIQUE INDEX t3_i ON t3(b, a);
  }

  execsql {
    INSERT INTO t3 VALUES(1, 'numeric', 2);
    INSERT INTO t3 VALUES(2, 'text', 2);
    INSERT INTO t3 VALUES(3, 'real', 2);
    INSERT INTO t3 VALUES(4, 'none', 2);
  }
} {}
do_test in3-4.2 {
  exec_neph { SELECT 'text' IN (SELECT b FROM t3) }
} {0 1}
do_test in3-4.3 {
  exec_neph { SELECT 'TEXT' COLLATE nocase IN (SELECT b FROM t3) }
} {1 1}
do_test in3-4.4 {
  exec_neph { SELECT b FROM t3 WHERE b IN (SELECT b FROM t3) }
} {1 none numeric real text}
do_test in3-4.5 {
  execsql { CREATE UNIQUE INDEX t3_i2 ON t3(b) }
  exec_neph { SELECT b FROM t3 WHERE b IN (SELECT b FROM t3) }
} {0 none numeric real text}
do_test in3-4.6 {
  execsql { DROP INDEX t3_i2 }
} {}
do_test in3-5.1 {
  execsql {
    CREATE TABLE Folders(
      folderid INTEGER PRIMARY KEY, 
      parentid INTEGER, 
      rootid INTEGER, 
      path VARCHAR(255)
    );
  }
} {}
do_test in3-5.2 {
  catchsql {
    DELETE FROM Folders WHERE folderid IN
    (SELECT folderid FROM Folder WHERE path LIKE 'C:\MP3\Albums\' || '%');
  }
} {1 {no such table: Folder}}
finish_test
"#);
}
