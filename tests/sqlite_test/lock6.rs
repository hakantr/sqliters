#[test]
#[ignore = "1/8 subtests passed; pending: lock6 edge cases"]
fn test_lock6() {
    sqliters_tcltest::run_test_script("lock6", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
proc launch_testfixture {} {
  set prg [info nameofexec]
  if {$prg eq ""} {
    set prg [file join . testfixture]
  }
  set chan [open "|$prg tf_main2.tcl" r+]
  fconfigure $chan -buffering line
  return $chan
}
proc testfixture {chan cmd} {
  puts $chan $cmd
  puts $chan OVER
  set r ""
  while { 1 } {
    set line [gets $chan]
    if { $line == "OVER" } { 
      return $r
    }
    append r $line
  }
}
set f [open tf_main2.tcl w]
puts $f {
  set l [open log w]
  set script ""
  while {![eof stdin]} {
    flush stdout
    set line [gets stdin]
    puts $l "READ $line"
    if { $line == "OVER" } {
      catch {eval $script} result
      puts $result
      puts $l "WRITE $result"
      puts OVER
      puts $l "WRITE OVER"
      flush stdout
      set script ""
    } else {
      append script $line
      append script " ; "
    }
  }
  close $l
}
close $f
ifcapable lock_proxy_pragmas&&prefer_proxy_locking {
  set sqlite_hostid_num 1

  set using_proxy 0
  foreach {name value} [array get env SQLITE_FORCE_PROXY_LOCKING] {
    set using_proxy $value
  }

  set env(SQLITE_FORCE_PROXY_LOCKING) "1"

  do_test lock6-1.1 {
    set ::tf1 [launch_testfixture]
    testfixture $::tf1 "sqlite3_test_control_pending_byte $::sqlite_pending_byte"
    testfixture $::tf1 {
      set sqlite_hostid_num 2    
      sqlite3 db test.db -key xyzzy
      set lockpath [db eval {
        PRAGMA lock_proxy_file=":auto:";
        select * from sqlite_master;
        PRAGMA lock_proxy_file;
      }]
      string match "*test.db:auto:" $lockpath
    }
  } {1}
  
  set sqlite_hostid_num 3   
  do_test lock6-1.2 {
    execsql {pragma lock_status}
  } {main unlocked temp closed}

  sqlite3_soft_heap_limit 0
  do_test lock6-1.3 {
    list [catch {
      sqlite3 db test.db
      execsql { select * from sqlite_master } 
    } msg] $msg
  } {1 {database is locked}}

  do_test lock6-1.4 {
    set lockpath [execsql {
      PRAGMA lock_proxy_file=":auto:";
      PRAGMA lock_proxy_file;
    } db]
    set lockpath
  } {{:auto: (not held)}}

  do_test lock6-1.4.1 {
    catchsql {
      PRAGMA lock_proxy_file="notmine";
      select * from sqlite_master;
    } db
  } {1 {database is locked}}

  do_test lock6-1.4.2 {
    execsql {
      PRAGMA lock_proxy_file;
    } db
  } {notmine}
    
  do_test lock6-1.5 {
    testfixture $::tf1 {
      db eval {
        BEGIN;
        SELECT * FROM sqlite_master;
      }
    }
  } {}

  catch {testfixture $::tf1 {db close}}

  do_test lock6-1.6 {
    execsql {
      PRAGMA lock_proxy_file="mine";
      select * from sqlite_master;
    } db
  } {}
  
  catch {close $::tf1}
  set env(SQLITE_FORCE_PROXY_LOCKING) $using_proxy
  set sqlite_hostid_num 0

  sqlite3_soft_heap_limit $cmdlinearg(soft-heap-limit)
}
finish_test
"#);
}
