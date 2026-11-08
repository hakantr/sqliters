#[test]
fn test_savepoint6() {
    sqliters_tcltest::run_test_script("savepoint6", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc sql {zSql} {
  if {0 && $::debug_op} {
    puts stderr "$zSql ;"
    flush stderr
  }
  uplevel db eval [list $zSql]
}
set DATABASE_SCHEMA {
    PRAGMA auto_vacuum = incremental;
    CREATE TABLE t1(x, y);
    CREATE UNIQUE INDEX i1 ON t1(x);
    CREATE INDEX i2 ON t1(y);
}
if {0==[info exists ::G(savepoint6_iterations)]} {
  set ::G(savepoint6_iterations) 1000
}
set lSavepoint [list]
array set aEntry [list]
proc x_to_y {x} {
  set nChar [expr int(rand()*250) + 250]
  set str " $nChar [string repeat $x. $nChar]"
  string range $str 1 $nChar
}
proc savepoint {zName} {
  if {$::debug_op} { puts stderr "savepoint $zName" ; flush stderr }
  catch { sql "SAVEPOINT $zName" }
  lappend ::lSavepoint [list $zName [array get ::aEntry]]
}
proc rollback {zName} {
  if {$::debug_op} { puts stderr "rollback $zName" ; flush stderr }
  catch { sql "ROLLBACK TO $zName" }
  for {set i [expr {[llength $::lSavepoint]-1}]} {$i>=0} {incr i -1} {
    set zSavepoint [lindex $::lSavepoint $i 0]
    if {$zSavepoint eq $zName} {
      unset -nocomplain ::aEntry
      array set ::aEntry [lindex $::lSavepoint $i 1]


      if {$i+1 < [llength $::lSavepoint]} {
        set ::lSavepoint [lreplace $::lSavepoint [expr $i+1] end]
      }
      break
    }
  }
}
proc release {zName} {
  if {$::debug_op} { puts stderr "release $zName" ; flush stderr }
  catch { sql "RELEASE $zName" }
  for {set i [expr {[llength $::lSavepoint]-1}]} {$i>=0} {incr i -1} {
    set zSavepoint [lindex $::lSavepoint $i 0]
    if {$zSavepoint eq $zName} {
      set ::lSavepoint [lreplace $::lSavepoint $i end]
      break
    }
  }

  if {[llength $::lSavepoint] == 0} {
  }
}
proc insert_rows {lX} {
  if {$::debug_op} { puts stderr "insert_rows $lX" ; flush stderr }
  foreach x $lX {
    set y [x_to_y $x]

    sql "INSERT OR REPLACE INTO t1 VALUES($x, '$y')"

    set ::aEntry($x) $y
  }
}
proc delete_rows {lX} {
  if {$::debug_op} { puts stderr "delete_rows $lX" ; flush stderr }
  foreach x $lX {
    sql "DELETE FROM t1 WHERE x = $x"

    unset -nocomplain ::aEntry($x)
  }
}
proc checkdb {} {
  set nEntry [db one {SELECT count(*) FROM t1}]
  set nEntry2 [array size ::aEntry]
  if {$nEntry != $nEntry2} {
    error "$nEntry entries in database, $nEntry2 entries in array"
  }
  db eval {SELECT x, y FROM t1} {
    if {![info exists ::aEntry($x)]} {
      error "Entry $x exists in database, but not in array"
    }
    if {$::aEntry($x) ne $y} {
      error "Entry $x is set to {$y} in database, {$::aEntry($x)} in array"
    }
  }

  db eval { PRAGMA integrity_check }
}
proc random_integers {nRes nRange} {
  set ret [list]
  for {set i 0} {$i<$nRes} {incr i} {
    lappend ret [expr int(rand()*$nRange)]
  }
  return $ret
} 
set ::debug_op 0
proc debug_ops {} {
  set ::debug_op 1
}
proc database_op {} {
  set i [expr int(rand()*2)] 
  if {$i==0} {
    insert_rows [random_integers 100 1000]
  }
  if {$i==1} {
    delete_rows [random_integers 100 1000]
    set i [expr int(rand()*3)] 
    if {$i==0} {
      sql {PRAGMA incremental_vacuum}
    }
  }
}
proc savepoint_op {} {
  set names {one two three four five}
  set cmds  {savepoint savepoint savepoint savepoint release rollback}

  set C [lindex $cmds [expr int(rand()*6)]]
  set N [lindex $names [expr int(rand()*5)]]

  $C $N
  return ok
}
expr srand(0)
do_test savepoint6-1.1 {
  sql $DATABASE_SCHEMA
} {}
do_test savepoint6-1.2 {
  insert_rows {
    497 166 230 355 779 588 394 317 290 475 362 193 805 851 564 
    763 44 930 389 819 765 760 966 280 538 414 500 18 25 287 320 
    30 382 751 87 283 981 429 630 974 421 270 810 405 
  }

  savepoint one
  insert_rows 858
  delete_rows 930
  savepoint two
    execsql {PRAGMA incremental_vacuum}
    savepoint three
      insert_rows 144
     rollback three
    rollback two
  release one

  execsql {SELECT count(*) FROM t1}
} {44}
foreach zSetup [list {
  set testname normal
  sqlite3 db test.db
} {
  if {[wal_is_wal_mode]} continue
  set testname tempdb
  sqlite3 db ""
} {
  if {[permutation] eq "journaltest"} {
    continue
  }
  set testname nosync
  sqlite3 db test.db
  sql { PRAGMA synchronous = off }
} {
  set testname smallcache
  sqlite3 db test.db
  sql { PRAGMA cache_size = 10 }
}] {

  unset -nocomplain ::lSavepoint
  unset -nocomplain ::aEntry

  catch { db close }
  forcedelete test.db test.db-wal test.db-journal
  eval $zSetup
  sql $DATABASE_SCHEMA

  wal_set_journal_mode

  do_test savepoint6-$testname.setup {
    savepoint one
    insert_rows [random_integers 100 1000]
    release one
    checkdb
  } {ok}
  
  for {set i 0} {$i < $::G(savepoint6_iterations)} {incr i} {
    do_test savepoint6-$testname.$i.1 {
      savepoint_op
      checkdb
    } {ok}
  
    do_test savepoint6-$testname.$i.2 {
      database_op
      database_op
      checkdb
    } {ok}
  }

  wal_check_journal_mode savepoint6-$testname.walok
}
unset -nocomplain ::lSavepoint
unset -nocomplain ::aEntry
finish_test
"#);
}
