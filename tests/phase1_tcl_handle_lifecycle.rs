//! Phase 1.1-E `sqliters-tcltest` Real Handle Lifecycle, Streaming `db_eval`,
//! Command Identity, `testfixture` Open Profile, Explicit Capabilities & Zero-Subtest Guard
//! (`implementation_plan.md` lines 195, 241-349, 891-905).
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters_core::Connection;
use sqliters_tcltest::harness::bootstrap_tester;
use sqliters_tcltest::interp::{TclControl, TclInterp};
use sqliters_tcltest::{
    SqliteTestHost, TESTFIXTURE_DEFAULT_PAGE_SIZE, evaluate_test_script, phase1_capability,
    run_test_script,
};
use sqliters_types::{LockGeometry, Value};

static NEXT_DIR_ID: AtomicU64 = AtomicU64::new(1);

struct TempTestDir {
    path: PathBuf,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sqliters_p1e_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn assert_tcl_script_passes(name: &str, script: &str, expected_subtests: usize) {
    let report = evaluate_test_script(name, script);
    assert!(
        report.is_ok(),
        "{name} failed: passed={}, failed={:?}, script_error={:?}, unknown_caps={:?}",
        report.passed,
        report.failed,
        report.script_error,
        report.unknown_capabilities,
    );
    assert_eq!(
        report.passed, expected_subtests,
        "{name}: expected {expected_subtests} subtests, got {}",
        report.passed
    );
    assert!(report.failed.is_empty());
    assert!(report.script_error.is_none());
    assert!(!report.skipped_zero_subtests);
}

#[test]
fn prepare_v1_v2_utf8_tail_and_unstepped_handle_semantics() {
    let script = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix p1e_prep

do_execsql_test 1.0 {
  CREATE TABLE t1(id INTEGER PRIMARY KEY, label TEXT, score REAL);
} {}

# Unstepped sqlite3_prepare_v2 has zero DML side effects and holds NO reader lock,
# yet is registered as an open handle on db (blocking `db close` with SQLITE_BUSY).
do_test 1.1 {
  set ::st_ins [sqlite3_prepare_v2 db "INSERT INTO t1 VALUES(42, 'unstepped', 3.5)" -1]
  list \
    [db eval {SELECT count(*) FROM t1}] \
    [db eval {PRAGMA lock_status}] \
    [catch {db close} msg] \
    $msg \
    [sqlite3_errcode db]
} {0 {main unlocked temp closed} 1 {unable to close due to unfinalized statements or unfinished backups} SQLITE_BUSY}

do_test 1.2 {
  sqlite3 db2 test.db
  set r1 [catchsql {INSERT INTO t1 VALUES(1, 'from_db2', 1.25)} db2]
  set r2 [sqlite3_step $::st_ins]
  set r3 [sqlite3_finalize $::st_ins]
  db2 close
  list $r1 $r2 $r3 [db eval {SELECT id, label FROM t1 ORDER BY id}]
} {{0 {}} SQLITE_DONE SQLITE_OK {1 from_db2 42 unstepped}}

# Both 3-arg (omitted tailvar) and 4-arg (with tailvar + UTF-8 multi-byte characters + nByte slicing).
do_test 1.3 {
  set sql "SELECT 'şğüöçı' AS tr, 7 AS num; SELECT 99;"
  set s3 [sqlite3_prepare_v2 db $sql -1]
  set c_before [sqlite3_column_count $s3]
  set d_before [sqlite3_data_count $s3]
  set names [list [sqlite3_column_name $s3 0] [sqlite3_column_name $s3 1]]
  set rc1 [sqlite3_step $s3]
  set d_row [sqlite3_data_count $s3]
  set vals [list [sqlite3_column_text $s3 0] [sqlite3_column_int $s3 1] [sqlite3_column_type $s3 0] [sqlite3_column_type $s3 1]]
  set rc2 [sqlite3_step $s3]
  set d_done [sqlite3_data_count $s3]
  set fin [sqlite3_finalize $s3]
  list $c_before $d_before $names $rc1 $d_row $vals $rc2 $d_done $fin
} {2 0 {tr num} SQLITE_ROW 2 {şğüöçı 7 TEXT INTEGER} SQLITE_DONE 0 SQLITE_OK}

do_test 1.4 {
  set sql "SELECT 'şğüöçı'; SELECT 99;"
  set s4 [sqlite3_prepare_v2 db $sql -1 tail_full]
  sqlite3_finalize $s4
  # First statement "SELECT 'şğüöçı';" is 8 + 12 + 2 = 22 UTF-8 bytes; slice first 27 bytes (" SELE" partial tail = 5 bytes).
  set s_limited [sqlite3_prepare_v2 db $sql 27 tail_limited]
  # Also slice at byte 28 inside a multi-byte tail (" 'ş'"): "SELECT 1; 'ş'" where byte limit lands mid-codepoint cleanly.
  set sql_mid "SELECT 1; 'ş'"
  set s_mid [sqlite3_prepare_v2 db $sql_mid 12 tail_mid]
  sqlite3_finalize $s_limited
  sqlite3_finalize $s_mid
  list $tail_full $tail_limited [string length $tail_mid]
} {{ SELECT 99;} { SELE} 3}

# Empty / comment-only SQL returns "" and does NOT leave an open statement handle blocking `db close`.
do_test 1.5 {
  sqlite3 db_empty test.db
  set s_comment [sqlite3_prepare_v2 db_empty "  -- just a comment\n  /* block */ " -1 rem_tail]
  set close_rc [catch {db_empty close} close_msg]
  list $s_comment $rem_tail $close_rc $close_msg
} {{} {} 0 {}}

# Parameter binding & clear_bindings against real Statement.
do_test 1.6 {
  set sb [sqlite3_prepare_v2 db "SELECT ?1, :named, ?3" -1]
  set pcnt [sqlite3_bind_parameter_count $sb]
  set pname2 [sqlite3_bind_parameter_name $sb 2]
  sqlite3_bind_int64 $sb 1 9007199254740993
  sqlite3_bind_text $sb 2 "merhaba" -1
  sqlite3_bind_double $sb 3 2.5
  set r1 [sqlite3_step $sb]
  set row1 [list [sqlite3_column_int64 $sb 0] [sqlite3_column_text $sb 1] [sqlite3_column_double $sb 2]]
  sqlite3_reset $sb
  sqlite3_clear_bindings $sb
  set r2 [sqlite3_step $sb]
  set row2 [list [sqlite3_column_type $sb 0] [sqlite3_column_type $sb 1] [sqlite3_column_type $sb 2]]
  sqlite3_finalize $sb
  list $pcnt $pname2 $r1 $row1 $r2 $row2
} {3 :named SQLITE_ROW {9007199254740993 merhaba 2.5} SQLITE_ROW {NULL NULL NULL}}

# Prepare error behavior: V2 preserves prior tailvar on error (test1.c:5216), V1 updates tailvar (test1.c:5139).
do_test 1.7 {
  set keep_tail "UNCHANGED"
  set rc_v2 [catch {sqlite3_prepare_v2 db "SELECT * FROM no_such_tbl; SELECT 7;" -1 keep_tail} err_v2]
  set code_v2 [sqlite3_errcode db]
  set v1_tail "INIT"
  set rc_v1 [catch {sqlite3_prepare db "SELECT * FROM no_such_tbl; SELECT 7;" -1 v1_tail} err_v1]
  list $rc_v2 $err_v2 $code_v2 $keep_tail $rc_v1 $err_v1 $v1_tail
} {1 {(1) no such table: no_such_tbl} SQLITE_ERROR UNCHANGED 1 {(1) no such table: no_such_tbl} { SELECT 7;}}

finish_test
"#;
    assert_tcl_script_passes("p1e_prep", script, 8);
}

#[test]
fn prepare_v1_vs_v2_schema_change_and_autoreset_lifecycle() {
    let script = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix p1e_v1v2

do_execsql_test 1.0 {
  PRAGMA foreign_keys = ON;
  CREATE TABLE t_schema(a INTEGER PRIMARY KEY, b TEXT);
  INSERT INTO t_schema VALUES(1, 'orig');
} {}

# Legacy V1 vs V2 across ALTER TABLE ADD COLUMN:
# - V1 step returns SQLITE_ERROR and latches SQLITE_SCHEMA for reset/finalize.
# - V2 transparently reprepares and refreshes column count & names.
do_test 1.1 {
  set sv1_a [sqlite3_prepare db "SELECT * FROM t_schema" -1]
  set sv1_b [sqlite3_prepare db "SELECT * FROM t_schema" -1]
  set sv2 [sqlite3_prepare_v2 db "SELECT * FROM t_schema" -1]

  db eval {ALTER TABLE t_schema ADD COLUMN c TEXT DEFAULT 'newcol'}

  set v1_step [sqlite3_step $sv1_a]
  set v1_step_err [sqlite3_errcode db]
  set v1_reset [sqlite3_reset $sv1_a]
  set v1_reset_err [sqlite3_errcode db]
  sqlite3_finalize $sv1_a

  set v1_b_step [sqlite3_step $sv1_b]
  set v1_fin [sqlite3_finalize $sv1_b]

  set v2_step [sqlite3_step $sv2]
  set v2_cols [list [sqlite3_column_count $sv2] [sqlite3_column_name $sv2 0] [sqlite3_column_name $sv2 1] [sqlite3_column_name $sv2 2]]
  set v2_vals [list [sqlite3_column_int $sv2 0] [sqlite3_column_text $sv2 1] [sqlite3_column_text $sv2 2]]
  sqlite3_finalize $sv2

  list $v1_step $v1_step_err $v1_reset $v1_reset_err $v1_b_step $v1_fin $v2_step $v2_cols $v2_vals
} {SQLITE_ERROR SQLITE_ERROR SQLITE_SCHEMA SQLITE_SCHEMA SQLITE_ERROR SQLITE_SCHEMA SQLITE_ROW {3 a b c} {1 orig newcol}}

# Core autoreset on SELECT after SQLITE_DONE (without calling sqlite3_reset).
do_test 1.2 {
  set s_sel [sqlite3_prepare_v2 db "SELECT a, b FROM t_schema" -1]
  set r1 [sqlite3_step $s_sel]
  set v1 [sqlite3_column_text $s_sel 1]
  set r2 [sqlite3_step $s_sel]
  # Immediate step after SQLITE_DONE autoresets and yields first row again:
  set r3 [sqlite3_step $s_sel]
  set v3 [sqlite3_column_text $s_sel 1]
  sqlite3_finalize $s_sel
  list $r1 $v1 $r2 $r3 $v3
} {SQLITE_ROW orig SQLITE_DONE SQLITE_ROW orig}

# fkey2-17.1.2 / 17.1.3 autoreset after SQLITE_CONSTRAINT + primary vs extended error codes.
do_test 1.3 {
  db eval {
    CREATE TABLE parent_fk(a, b, c, UNIQUE(b, c));
    CREATE TABLE child_fk(d, e, f, FOREIGN KEY(e, f) REFERENCES parent_fk(b, c));
    INSERT INTO parent_fk VALUES(1, 2, 3);
  }
  set fk_stmt [sqlite3_prepare_v2 db "INSERT INTO child_fk VALUES(4, 5, 6)" -1]
  set s1 [sqlite3_step $fk_stmt]
  set p1 [sqlite3_errcode db]
  set x1 [sqlite3_extended_errcode db]
  # Step again without manual reset (autoreset=1):
  set s2 [sqlite3_step $fk_stmt]
  set p2 [sqlite3_errcode db]
  set x2 [sqlite3_extended_errcode db]
  # Enable extended result codes on db; step now surfaces SQLITE_CONSTRAINT_FOREIGNKEY directly:
  sqlite3_extended_result_codes db 1
  set s3 [sqlite3_step $fk_stmt]
  set p3 [sqlite3_errcode db]
  sqlite3_extended_result_codes db 0
  # Direct finalize after failed step surfaces latched SQLITE_CONSTRAINT (`vdbeapi.c`),
  # whereas reset consumes SQLITE_CONSTRAINT and subsequent finalize returns SQLITE_OK:
  set fk_stmt2 [sqlite3_prepare_v2 db "INSERT INTO child_fk VALUES(4, 5, 6)" -1]
  sqlite3_step $fk_stmt2
  set rst2 [sqlite3_reset $fk_stmt2]
  set fin2 [sqlite3_finalize $fk_stmt2]
  set fin1 [sqlite3_finalize $fk_stmt]
  list $s1 $p1 $x1 $s2 $p2 $x2 $s3 $p3 $rst2 $fin2 $fin1
} {SQLITE_CONSTRAINT SQLITE_CONSTRAINT SQLITE_CONSTRAINT_FOREIGNKEY SQLITE_CONSTRAINT SQLITE_CONSTRAINT SQLITE_CONSTRAINT_FOREIGNKEY SQLITE_CONSTRAINT_FOREIGNKEY SQLITE_CONSTRAINT_FOREIGNKEY SQLITE_CONSTRAINT SQLITE_OK SQLITE_CONSTRAINT}

finish_test
"#;
    assert_tcl_script_passes("p1e_v1v2", script, 4);
}

#[test]
fn lock6_and_lock7_real_statement_shared_lock_and_write_to_read_downgrade() {
    let script = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix p1e_lock67

do_execsql_test 1.0 {
  CREATE TABLE t4(a PRIMARY KEY, b);
  INSERT INTO t4 VALUES(1, 'one');
  INSERT INTO t4 VALUES(2, 'two');
  INSERT INTO t4 VALUES(3, 'three');
} {}

# Stepping a table-scan SELECT once to SQLITE_ROW holds SHARED on db and blocks db2 writes;
# sqlite3_reset drops SHARED to unlocked so db2 can write.
do_test 1.1 {
  sqlite3 db2 test.db
  set st_scan [sqlite3_prepare_v2 db "SELECT a, b FROM t4 ORDER BY a" -1]
  set st_rc [sqlite3_step $st_scan]
  set ls_during [db eval {PRAGMA lock_status}]
  set db2_read [db2 eval {SELECT count(*) FROM t4}]
  set db2_wr_blocked [catchsql {INSERT INTO t4 VALUES(4, 'four')} db2]
  set rst_rc [sqlite3_reset $st_scan]
  set ls_after_reset [db eval {PRAGMA lock_status}]
  set db2_wr_ok [catchsql {INSERT INTO t4 VALUES(4, 'four')} db2]
  db eval {DELETE FROM t4 WHERE a = 4}
  sqlite3_finalize $st_scan
  list $st_rc $ls_during $db2_read $db2_wr_blocked $rst_rc $ls_after_reset $db2_wr_ok
} {SQLITE_ROW {main shared temp closed} 3 {1 {database is locked}} SQLITE_OK {main unlocked temp closed} {0 {}}}

# Also verify GROUP BY / aggregate query stepped to SQLITE_ROW holds SHARED until reset/finalize.
do_test 1.2 {
  set st_grp [sqlite3_prepare_v2 db "SELECT b, count(*) FROM t4 GROUP BY b" -1]
  set rc1 [sqlite3_step $st_grp]
  set ls1 [db eval {PRAGMA lock_status}]
  set blocked [catchsql {INSERT INTO t4 VALUES(9, 'nine')} db2]
  set fin [sqlite3_finalize $st_grp]
  set ls2 [db eval {PRAGMA lock_status}]
  list $rc1 $ls1 $blocked $fin $ls2
} {SQLITE_ROW {main shared temp closed} {1 {database is locked}} SQLITE_OK {main unlocked temp closed}}

# Independent lock-6.1..6.5: paused sqlite_master SELECT on db + autocommit DELETE + explicit tx INSERT
# downgrades Write -> Read (SHARED retained, RESERVED/EXCLUSIVE released so db2 reads committed state).
do_test 6.1 {
  set ::STMT [sqlite3_prepare $::DB "SELECT * FROM sqlite_master" -1 TAIL]
  set step1 [sqlite3_step $::STMT]
  execsql { DELETE FROM t4 }
  list \
    $step1 \
    [execsql { PRAGMA lock_status }] \
    [execsql { SELECT count(*) FROM sqlite_master } db2] \
    [execsql { SELECT * FROM t4 } db2] \
    [catchsql { INSERT INTO t4 VALUES(99, 'blocked') } db2]
} {SQLITE_ROW {main shared temp closed} 2 {} {1 {database is locked}}}

do_test 6.2 {
  execsql {
    BEGIN;
    INSERT INTO t4 VALUES(1, 'one');
    INSERT INTO t4 VALUES(2, 'two');
    INSERT INTO t4 VALUES(3, 'three');
    COMMIT;
  }
  execsql { SELECT * FROM t4 } db2
} {1 one 2 two 3 three}

do_test 6.3 {
  execsql { SELECT a FROM t4 ORDER BY a } db2
} {1 2 3}

do_test 6.4 {
  execsql { PRAGMA integrity_check } db2
} {ok}

do_test 6.5 {
  list [sqlite3_finalize $::STMT] [execsql { PRAGMA lock_status }]
} {SQLITE_OK {main unlocked temp closed}}

# Independent lock-7.1..7.4: journal_mode=truncate + active read statement + zero-page-write commit
# downgrades to `main shared` instead of retaining RESERVED/EXCLUSIVE.
do_test 7.1 {
  set ::STMT [sqlite3_prepare $::DB "SELECT * FROM sqlite_master" -1 TAIL]
  sqlite3_step $::STMT
} {SQLITE_ROW}

do_test 7.2 {
  execsql { PRAGMA lock_status }
} {main shared temp closed}

do_test 7.3 {
  execsql {
    PRAGMA journal_mode = truncate;
    BEGIN;
    UPDATE t4 SET a = 10 WHERE 0;
    COMMIT;
  }
  list [execsql { PRAGMA lock_status }] [execsql { SELECT a FROM t4 ORDER BY a } db2]
} {{main shared temp closed} {1 2 3}}

do_test 7.4 {
  list [sqlite3_finalize $::STMT] [execsql { PRAGMA lock_status }]
} {SQLITE_OK {main unlocked temp closed}}

db2 close
finish_test
"#;
    assert_tcl_script_passes("p1e_lock67", script, 12);
}

#[test]
fn streaming_db_eval_reentrancy_locks_and_control_flow_cleanup() {
    let script = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix p1e_dbeval

do_execsql_test 1.0 {
  CREATE TABLE t1(a INTEGER PRIMARY KEY, b TEXT);
  INSERT INTO t1 VALUES(1, 'alpha'), (2, 'beta'), (3, 'gamma');
  CREATE TABLE t2(x INT, y INT);
  INSERT INTO t2 VALUES(8, 9);
  CREATE TABLE audit_log(note TEXT);
} {}

# lock-1.16..1.22 streaming bodied db_eval:
# Outer SELECT holds SHARED per row; nested same-db UPDATE commits via Write -> Read keeping SHARED;
# db2 SELECT succeeds while db2 UPDATE fails with SQLITE_BUSY ("database is locked").
do_test 1.1 {
  sqlite3 db2 test.db
  set seen {}
  db eval {SELECT a, b FROM t1 ORDER BY a} qv {
    set ls_before [db eval {PRAGMA lock_status}]
    set upd_same [catch {db eval {UPDATE t2 SET x = y, y = x}} upd_msg]
    set ls_after [db eval {PRAGMA lock_status}]
    set db2_sel [catch {db2 eval {SELECT a FROM t1 ORDER BY a}} sel_msg]
    set db2_upd [catch {db2 eval {UPDATE t2 SET x = 100}} busy_msg]
    lappend seen [list $qv(*) $qv(a) $qv(b) $ls_before $upd_same $ls_after $db2_sel $sel_msg $db2_upd $busy_msg]
  }
  db2 close
  list [llength $seen] [lindex $seen 0] [db eval {SELECT x, y FROM t2}] [db eval {PRAGMA lock_status}]
} {3 {{a b} 1 alpha {main shared temp closed} 0 {main shared temp closed} 0 {1 2 3} 1 {database is locked}} {9 8} {main unlocked temp closed}}

# `continue` skips remainder of current iteration and advances cleanly.
do_test 1.2 {
  set kept {}
  db eval {SELECT a, b FROM t1 ORDER BY a} {
    if {$a == 2} { continue }
    lappend kept "$a:$b"
  }
  list $kept [db eval {PRAGMA lock_status}]
} {{1:alpha 3:gamma} {main unlocked temp closed}}

# `break` finalizes current statement and stops multi-statement tail immediately!
do_test 1.3 {
  set hits 0
  set res [db eval {
    SELECT a FROM t1 ORDER BY a;
    INSERT INTO audit_log VALUES('tail_must_not_run');
  } {
    incr hits
    if {$a == 2} { break }
  }]
  list $res $hits [db eval {SELECT count(*) FROM audit_log}] [db eval {PRAGMA lock_status}]
} {{} 2 0 {main unlocked temp closed}}

# `return` inside a proc wrapping `db eval` finalizes statement cleanly without leaking locks.
do_test 1.4 {
  proc find_second {} {
    db eval {SELECT a, b FROM t1 ORDER BY a} {
      if {$a == 2} {
        return "found-$b"
      }
    }
    return "missing"
  }
  list [find_second] [db eval {PRAGMA lock_status}]
} {found-beta {main unlocked temp closed}}

# Tcl `error` inside `db eval` body finalizes statement cleanly and preserves Tcl error message.
do_test 1.5 {
  set rc [catch {
    db eval {SELECT a FROM t1 ORDER BY a} {
      if {$a == 2} { error "body-exploded" }
    }
  } msg]
  list $rc $msg [db eval {PRAGMA lock_status}]
} {1 body-exploded {main unlocked temp closed}}

# SQL error in multi-statement `db eval` tail finalizes cleanly and leaves handle closable.
do_test 1.6 {
  set rc [catch {
    db eval {
      SELECT a FROM t1;
      SELECT * FROM nonexistent_after_first;
    } {
      # consume first statement rows
    }
  } msg]
  set close_rc [catch {db close} close_msg]
  sqlite3 db test.db
  list $rc $msg $close_rc $close_msg
} {1 {no such table: nonexistent_after_first} 0 {}}

# Early-exit finalize error/state handling:
# - `return` early exit leaves `sqlite3_errcode db` at `SQLITE_OK`.
# - `error` early exit after a failed inner SQL statement preserves `db`'s SQL error code.
do_test 1.7 {
  proc ret_early {} {
    db eval {SELECT a FROM t1 ORDER BY a} {
      return "early-$a"
    }
  }
  set r_val [ret_early]
  set r_code [sqlite3_errcode db]

  set e_rc [catch {
    db eval {SELECT a FROM t1 ORDER BY a} {
      catch {db eval {INSERT INTO t1 VALUES(1, 'dup')}}
      error "abort-body"
    }
  } e_msg]
  set e_code [sqlite3_errcode db]
  set e_xcode [sqlite3_extended_errcode db]
  list $r_val $r_code $e_rc $e_msg $e_code $e_xcode
} {early-1 SQLITE_OK 1 abort-body SQLITE_CONSTRAINT SQLITE_CONSTRAINT_PRIMARYKEY}

finish_test
"#;
    assert_tcl_script_passes("p1e_dbeval", script, 8);
}

#[test]
fn db_close_busy_preserves_intact_handle_and_rename_delete_hook_semantics() {
    let script = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix p1e_identity

do_execsql_test 1.0 {
  CREATE TABLE items(k INTEGER PRIMARY KEY, v TEXT);
  INSERT INTO items VALUES(1, 'one'), (2, 'two');
} {}

# `db close` fails with BUSY for both unstepped and Row-paused statements AND inside streaming `db eval`,
# preserving the exact same DbId and full usability of `db`.
do_test 1.1 {
  set ptr_before [sqlite3_connection_pointer db]

  # (a) Unstepped statement
  set st [sqlite3_prepare_v2 db "SELECT k, v FROM items ORDER BY k" -1]
  set c1 [catch {db close} m1]
  set ptr_a [sqlite3_connection_pointer db]

  # (b) Stepped to SQLITE_ROW
  set step_rc [sqlite3_step $st]
  set c2 [catch {db close} m2]
  set ptr_b [sqlite3_connection_pointer db]
  set usable [db eval {SELECT count(*) FROM items}]
  sqlite3_finalize $st

  # (c) Inside streaming `db eval` body
  set inner_close {}
  db eval {SELECT k FROM items ORDER BY k} {
    if {$k == 1} {
      set inner_close [list [catch {db close} imsg] $imsg]
    }
  }
  set ptr_c [sqlite3_connection_pointer db]
  set post_eval [db eval {SELECT v FROM items WHERE k = 2}]

  list \
    [expr {$ptr_before eq $ptr_a && $ptr_a eq $ptr_b && $ptr_b eq $ptr_c}] \
    $c1 $m1 $step_rc $c2 $m2 $usable $inner_close $post_eval
} {1 1 {unable to close due to unfinalized statements or unfinished backups} SQLITE_ROW 1 {unable to close due to unfinalized statements or unfinished backups} 2 {1 {unable to close due to unfinalized statements or unfinished backups}} two}

# `rename db2 db2_moved` preserves DbId and invalidates old command name;
# `rename db2_moved {}` detaches command once while external Statement remains steppable until finalized!
do_test 1.2 {
  sqlite3 db2 test.db
  set p1 [sqlite3_connection_pointer db2]
  rename db2 db2_moved
  set old_rc [catch {db2 eval {SELECT 1}} old_msg]
  set p2 [sqlite3_connection_pointer db2_moved]
  set ext_st [sqlite3_prepare_v2 db2_moved "SELECT v FROM items ORDER BY k" -1]
  rename db2_moved {}
  set gone_rc [catch {db2_moved eval {SELECT 1}} gone_msg]
  set s1 [sqlite3_step $ext_st]
  set v1 [sqlite3_column_text $ext_st 0]
  set fin [sqlite3_finalize $ext_st]
  list [expr {$p1 eq $p2}] $old_rc $gone_rc $s1 $v1 $fin
} {1 1 1 SQLITE_ROW one SQLITE_OK}

# Deleting a DB command from inside its own multi-statement `db eval` body unwinds safely.
do_test 1.3 {
  sqlite3 db_self test.db
  set visited {}
  db_self eval {SELECT k FROM items ORDER BY k} {
    lappend visited $k
    if {$k == 1} {
      rename db_self {}
    }
  }
  set after_rc [catch {db_self eval {SELECT 1}}]
  list $visited $after_rc
} {{1 2} 1}

# Renaming host builtin (`sqlite3`) and core interpreter builtin (`set`) updates dispatch cleanly.
do_test 1.4 {
  rename sqlite3 sq_open_custom
  set r_old_sq [catch {sqlite3 db_tmp test.db}]
  sq_open_custom db_tmp test.db
  set val [db_tmp eval {SELECT count(*) FROM items}]
  db_tmp close
  rename sq_open_custom sqlite3

  rename set custom_set
  custom_set set_check_rc ""
  if {[catch {set x 10}]} {
    custom_set set_check_rc "old_set_disabled"
  }
  rename custom_set set

  list $r_old_sq $val $set_check_rc
} {1 2 old_set_disabled}

# `reset_db` cleanly finalizes host-owned statements on `db`, closes `db`, deletes `test.db`, and reopens `db`.
do_test 1.5 {
  set dangling [sqlite3_prepare_v2 db "SELECT * FROM items" -1]
  sqlite3_step $dangling
  reset_db
  list \
    [sqlite3_step $dangling] \
    [db eval {SELECT count(*) FROM sqlite_master}] \
    [db eval {PRAGMA page_size}]
} {SQLITE_MISUSE 0 1024}

# Orphaned statement from `rename db {}` is also finalized BEFORE `reset_db` unlinks `test.db`.
do_test 1.6 {
  db eval {
    CREATE TABLE orphan_tbl(id INTEGER PRIMARY KEY, name TEXT);
    INSERT INTO orphan_tbl VALUES(1, 'live');
  }
  set orphaned [sqlite3_prepare_v2 db "SELECT id, name FROM orphan_tbl" -1]
  set pre_step [sqlite3_step $orphaned]
  rename db {}
  reset_db
  list \
    $pre_step \
    [sqlite3_step $orphaned] \
    [db eval {SELECT count(*) FROM sqlite_master}] \
    [db eval {PRAGMA lock_status}]
} {SQLITE_ROW SQLITE_MISUSE 0 {main unlocked temp closed}}

finish_test
"#;
    assert_tcl_script_passes("p1e_identity", script, 7);
}

#[test]
fn db_transaction_tcl10_nested_savepoints_control_flow_and_busy_commit_rollback() {
    let script = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix p1e_tx

do_test 10.1 {
  db transaction {}
} {}

do_test 10.2 {
  db transaction deferred {}
} {}

do_test 10.3 {
  db transaction immediate {}
} {}

do_test 10.4 {
  db transaction exclusive {}
} {}

do_test 10.5 {
  set rc1 [catch {db transaction xyzzy {}} msg1]
  set rc2 [catch {db transaction} msg2]
  list $rc1 $msg1 $rc2 $msg2
} {1 {bad transaction type "xyzzy": must be deferred, exclusive, or immediate} 1 {wrong # args: should be "db transaction [TYPE] SCRIPT"}}

do_test 10.6 {
  set rc [catch {db transaction {error test-error}} msg]
  list $rc $msg
} {1 test-error}

do_test 10.7 {
  db transaction {
    db eval {CREATE TABLE t4(x)}
    db transaction {
      db eval {INSERT INTO t4 VALUES(1)}
    }
  }
  db eval {SELECT * FROM t4}
} {1}

do_test 10.8 {
  catch {
    db transaction {
      db eval {INSERT INTO t4 VALUES(2)}
      db eval {INSERT INTO t4 VALUES(3)}
      db eval {INSERT INTO t4 VALUES(4)}
      error test-error
    }
  }
  db eval {SELECT * FROM t4}
} {1}

# Nested `db transaction` uses `SAVEPOINT _tcl_transaction` and rolls back ONLY the inner block!
do_test 10.9 {
  db transaction {
    db eval {INSERT INTO t4 VALUES(2)}
    catch {
      db transaction {
        db eval {INSERT INTO t4 VALUES(3)}
        db eval {INSERT INTO t4 VALUES(4)}
        error test-error
      }
    }
  }
  db eval {SELECT * FROM t4}
} {1 2}

# `continue` inside `db transaction` commits and propagates `TCL_CONTINUE` to enclosing `for` loop.
do_test 10.10 {
  for {set i 0} {$i < 1} {incr i} {
    db transaction {
      db eval {INSERT INTO t4 VALUES(5)}
      continue
    }
    error "This line should not be run"
  }
  db eval {SELECT * FROM t4}
} {1 2 5}

# `break` inside `db transaction` commits and breaks enclosing `for` loop on first iteration!
do_test 10.11 {
  for {set i 0} {$i < 10} {incr i} {
    db transaction {
      db eval {INSERT INTO t4 VALUES(6)}
      break
    }
  }
  db eval {SELECT * FROM t4}
} {1 2 5 6}

# `return` inside `db transaction` commits and propagates `TCL_RETURN` (`2` in `catch`, or proc return value).
do_test 10.12 {
  set rc [catch {
    for {set i 0} {$i < 10} {incr i} {
      db transaction {
        db eval {INSERT INTO t4 VALUES(7)}
        return
      }
    }
  }]
  proc tx_ret_proc {} {
    db transaction {
      db eval {INSERT INTO t4 VALUES(8)}
      return "committed-8"
    }
    return "unreachable"
  }
  list $rc [tx_ret_proc] [db eval {SELECT * FROM t4}]
} {2 committed-8 {1 2 5 6 7 8}}

# 3-level nested `db transaction` partial and full rollback (`tcl-10.14..10.17`).
do_test 10.14 {
  db transaction {
    db eval {
      DELETE FROM t4;
      INSERT INTO t4 VALUES('one');
    }
    catch {
      db transaction {
        db eval {INSERT INTO t4 VALUES('two')}
        db transaction {
          db eval {INSERT INTO t4 VALUES('three')}
          error "throw an error!"
        }
      }
    }
  }
  list [db eval {SELECT * FROM t4}] [db eval {BEGIN ; COMMIT}]
} {one {}}

do_test 10.16 {
  db transaction {
    db eval {INSERT INTO t4 VALUES('two')}
    db transaction {
      db eval {INSERT INTO t4 VALUES('three')}
      db transaction {
        db eval {INSERT INTO t4 VALUES('four')}
      }
    }
  }
  catch {
    db transaction {
      db eval {INSERT INTO t4 VALUES('A')}
      db transaction {
        db eval {INSERT INTO t4 VALUES('B')}
        db transaction {
          db eval {INSERT INTO t4 VALUES('C')}
          error "throw an error!"
        }
      }
    }
  }
  list [db eval {SELECT * FROM t4}] [db eval {BEGIN ; COMMIT}]
} {{one two three four} {}}

# `COMMIT` failing with `SQLITE_BUSY` ("database is locked") against `db2` SHARED lock
# executes `ROLLBACK` so no transaction remains stuck open on `db` (`tcl-10.18..10.19`)!
do_test 10.18 {
  sqlite3 db2 test.db
  db2 eval {
    BEGIN;
    SELECT * FROM sqlite_master;
  }
  set rc [catch {
    db transaction {
      db eval {INSERT INTO t4 VALUES('five')}
    }
  } msg]
  set err_code [sqlite3_errcode db]
  set post_tx [db eval {BEGIN ; COMMIT}]
  list $rc $msg $err_code $post_tx [db2 eval {SELECT * FROM t4}]
} {1 {database is locked} SQLITE_BUSY {} {one two three four}}

# `BEGIN EXCLUSIVE` on `db2` blocks `db transaction` body and unwinds implicit savepoint tx (`tcl-10.20..10.21`).
do_test 10.20 {
  db2 eval {
    COMMIT;
    BEGIN EXCLUSIVE;
  }
  set rc [catch {
    db transaction {
      db eval {INSERT INTO t4 VALUES('five')}
    }
  } msg]
  db2 close
  set post_tx [db eval {BEGIN ; COMMIT}]
  list $rc $msg $post_tx [db eval {SELECT * FROM t4}]
} {1 {database is locked} {} {one two three four}}

# Command-deletion safety (`rename db_tx {}`) and `db close` BUSY preservation inside `db transaction`.
do_test 10.22 {
  sqlite3 db_tx test.db
  db_tx transaction {
    db_tx eval {INSERT INTO t4 VALUES('from_deleted_cmd')}
    rename db_tx {}
  }
  set st_hold [sqlite3_prepare_v2 db "SELECT * FROM t4" -1]
  db transaction {
    set close_rc [catch {db close} close_msg]
    db transaction {
      db eval {INSERT INTO t4 VALUES('after_busy_close')}
    }
  }
  sqlite3_finalize $st_hold
  list $close_rc [db eval {SELECT * FROM t4}]
} {1 {one two three four from_deleted_cmd after_busy_close}}

finish_test
"#;
    assert_tcl_script_passes("p1e_tx", script, 17);
}

#[test]
fn testfixture_1024_page_size_vs_production_4096_and_pending_byte_controls() {
    let tmp = TempTestDir::new("pagesize_pbyte");

    // 1. Production `Connection::open` defaults to 4096-byte pages + 1 GiB pending byte.
    let prod_path = tmp.path().join("prod.db");
    {
        let prod_conn = Connection::open(&prod_path).expect("open production db");
        let ps = prod_conn
            .query_row("PRAGMA page_size;", &[])
            .expect("query page_size")
            .expect("row");
        assert_eq!(ps[0], Value::Integer(4096));
        prod_conn
            .execute_batch("CREATE TABLE prod_t(x INT); INSERT INTO prod_t VALUES(123);")
            .expect("populate prod.db");
        prod_conn.try_close().expect("close prod.db");
    }
    assert_eq!(
        fs::metadata(&prod_path).expect("prod.db metadata").len(),
        8192,
        "2-page production database must be 2 * 4096 = 8192 bytes"
    );

    // 2. `sqliters-tcltest` host defaults to 1024-byte pages + 64 KiB pending byte (`65536`),
    //    preserves 4096 when opening pre-existing `prod.db`, and enforces pending-byte invariants!
    let sqlite_test_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("sqlite/test");
    let host = SqliteTestHost::new_shared("p1e_profile", sqlite_test_dir);
    assert_eq!(
        host.borrow().default_page_size(),
        TESTFIXTURE_DEFAULT_PAGE_SIZE
    );
    assert_eq!(host.borrow().lock_geometry(), LockGeometry::test_64k());

    let mut interp = TclInterp::new(tmp.path().to_path_buf(), Some(host.clone()));
    bootstrap_tester(&mut interp, &host, tmp.path()).expect("bootstrap tester");

    let prod_path_tcl = prod_path.to_string_lossy().into_owned();
    interp
        .set_var("PROD_DB_PATH", prod_path_tcl)
        .expect("set PROD_DB_PATH");

    let script = r#"
set testprefix p1e_profile

# Verify lock4-1.1 invariant: two 2-page databases created in harness are {2048 2048} bytes (1024 page size).
do_test 1.1 {
  db eval {CREATE TABLE t1(x); INSERT INTO t1 VALUES(1);}
  sqlite3 db2 test2.db
  db2 eval {CREATE TABLE t2(x); INSERT INTO t2 VALUES(2);}
  db2 close
  list [db eval {PRAGMA page_size}] [file size test.db] [file size test2.db]
} {1024 2048 2048}

# Opening pre-existing 4096-byte production DB in harness preserves its 4096-byte page size.
do_test 1.2 {
  sqlite3 db_prod $::PROD_DB_PATH
  set info [list [db_prod eval {PRAGMA page_size}] [db_prod eval {SELECT x FROM prod_t}]]
  db_prod close
  set info
} {4096 123}

# Pending-byte defaults to 65536, allows idempotent write while db is open,
# and rejects changing to a different offset while ANY db or statement handle is open.
do_test 1.3 {
  set def1 $::sqlite_pending_byte
  set def2 [sqlite3_test_control_pending_byte]
  set idem1 [set ::sqlite_pending_byte 65536]
  set idem2 [sqlite3_test_control_pending_byte 65536]
  set chg_err1 [catch {set ::sqlite_pending_byte 1048576} msg1]
  set chg_err2 [catch {sqlite3_test_control_pending_byte 1048576} msg2]
  list $def1 $def2 $idem1 $idem2 $chg_err1 $chg_err2 [expr {$::sqlite_pending_byte == 65536}]
} {65536 65536 65536 65536 1 1 1}

# Once all DB handles are closed, changing pending byte succeeds and applies to subsequent opens.
do_test 1.4 {
  db close
  set prev [sqlite3_test_control_pending_byte 1048576]
  set cur $::sqlite_pending_byte
  set ::sqlite_pending_byte 65536
  set restored [sqlite3_test_control_pending_byte]
  sqlite3 db test.db
  list $prev $cur $restored [db eval {SELECT x FROM t1}]
} {65536 1048576 65536 1}

finish_test
"#;

    match interp.eval(script) {
        Ok(_) | Err(TclControl::FinishTest) => {}
        Err(other) => panic!("unexpected script failure: {:?}", other),
    }
    let report = host.borrow().report.clone();
    assert!(report.is_ok(), "failed={:?}", report.failed);
    assert_eq!(report.passed, 4);
}

#[test]
fn explicit_capability_table_and_zero_subtest_guard() {
    // 1. Direct Rust API audit of Phase 1.1 + Phase 1.2 capabilities.
    assert_eq!(phase1_capability("autoreset"), Some(true));
    assert_eq!(phase1_capability("foreignkey"), Some(true));
    assert_eq!(phase1_capability("savepoint"), Some(true));
    assert_eq!(phase1_capability("returning"), Some(true));
    assert_eq!(phase1_capability("cte"), Some(true));
    assert_eq!(phase1_capability("generated"), Some(true));
    assert_eq!(phase1_capability("wal"), Some(true));
    assert_eq!(phase1_capability("autovacuum"), Some(true));
    assert_eq!(phase1_capability("incr_vacuum"), Some(true));
    assert_eq!(phase1_capability("tempdb"), Some(true));
    assert_eq!(phase1_capability("attach"), Some(true));
    assert_eq!(phase1_capability("view"), Some(true));
    assert_eq!(phase1_capability("upsert"), Some(true));
    assert_eq!(phase1_capability("trigger"), Some(true));
    assert_eq!(phase1_capability("windowfunc"), Some(true));
    assert_eq!(phase1_capability("analyze"), Some(true));
    assert_eq!(phase1_capability("default_autovacuum"), Some(false));
    for disabled in [
        "stat4",
        "progress",
        "trace",
        "auth",
        "incrblob",
        "vtab",
        "rtree",
        "fts5",
        "json",
        "fts3",
        "fts4",
        "load_ext",
        "shared_cache",
        "utf16",
        "lock_proxy_pragmas",
        "prefer_proxy_locking",
        "memorymanage",
    ] {
        assert_eq!(
            phase1_capability(disabled),
            Some(false),
            "capability `{disabled}` must be explicitly 0 in Phase 5"
        );
    }
    assert_eq!(phase1_capability("nonexistent_xyz"), None);

    // 2. Tcl `capable` / `ifcapable` evaluation & unknown-capability diagnostic recording.
    let cap_script = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix p1e_caps

do_test 1.1 {
  list \
    [capable autoreset] \
    [capable foreignkey] \
    [capable wal] \
    [capable tempdb] \
    [capable attach] \
    [capable autovacuum] \
    [capable incr_vacuum] \
    [capable default_autovacuum] \
    [capable view] \
    [capable upsert] \
    [capable trigger] \
    [capable windowfunc] \
    [capable analyze] \
    [capable progress] \
    [capable trace] \
    [capable auth] \
    [capable incrblob] \
    [capable vtab] \
    [capable rtree] \
    [capable fts5] \
    [capable json] \
    [capable stat4] \
    [capable fts3] \
    [capable load_ext] \
    [capable shared_cache] \
    [capable utf16] \
    [capable lock_proxy_pragmas] \
    [capable prefer_proxy_locking] \
    [capable nonexistent_xyz] \
    [capable !nonexistent_xyz]
} {1 1 1 1 1 1 1 0 1 1 1 1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1}

finish_test
"#;
    let report = evaluate_test_script("p1e_caps", cap_script);
    assert!(report.is_ok());
    assert_eq!(report.passed, 1);
    assert_eq!(
        report.unknown_capabilities,
        vec!["nonexistent_xyz".to_string()]
    );

    // 3. Zero-subtest guard on `finish_test; return`.
    let zero_script = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable !shared_cache {
  finish_test
  return
}
do_test unreachable-1.1 { expr {1 + 1} } {2}
finish_test
"#;
    let zero_report = evaluate_test_script("p1e_zero", zero_script);
    assert_eq!(zero_report.passed, 0);
    assert!(zero_report.skipped_zero_subtests);
    assert!(!zero_report.is_ok());
}

#[test]
#[should_panic(expected = "Zero subtests executed")]
fn run_test_script_panics_when_zero_subtests_execute() {
    run_test_script(
        "p1e_zero_panic",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
ifcapable !shared_cache {
  finish_test
  return
}
"#,
    );
}

#[cfg(unix)]
mod unix_c_interop {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    const C_TCL_LOCK_HELPER_SRC: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include "sqlite3.h"

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <db_path> <pending_byte>\n", argv[0]);
        return 2;
    }
    const char *db_path = argv[1];
    unsigned int pbyte = (unsigned int)strtoul(argv[2], NULL, 0);

    sqlite3_test_control(SQLITE_TESTCTRL_PENDING_BYTE, pbyte);
    sqlite3 *db = NULL;
    int rc = sqlite3_open_v2(
        db_path,
        &db,
        SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
        NULL
    );
    if (rc != SQLITE_OK) {
        fprintf(stderr, "open failed: %d\n", rc);
        if (db) sqlite3_close(db);
        return 2;
    }
    sqlite3_busy_timeout(db, 0);
    char *errmsg = NULL;
    rc = sqlite3_exec(db, "BEGIN EXCLUSIVE;", NULL, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "BEGIN EXCLUSIVE failed (%d): %s\n", rc, errmsg ? errmsg : "");
        sqlite3_free(errmsg);
        sqlite3_close(db);
        return 2;
    }
    printf("EXCLUSIVE_READY\n");
    fflush(stdout);
    while (fgetc(stdin) != EOF) {}

    sqlite3_exec(db, "ROLLBACK;", NULL, NULL, NULL);
    sqlite3_close(db);
    return 0;
}
"#;

    fn c_exclusive_lock_helper() -> &'static Path {
        static BIN: OnceLock<PathBuf> = OnceLock::new();
        BIN.get_or_init(|| {
            let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let sqlite_src_dir = workspace_root.join("sqlite");
            let ref_root = workspace_root.join("target").join("sqlite-ref");
            let amalg_dir = ref_root.join("amalg");
            fs::create_dir_all(&amalg_dir).expect("create target/sqlite-ref/amalg");

            let sqlite3_c = amalg_dir.join("sqlite3.c");
            let sqlite3_h = amalg_dir.join("sqlite3.h");
            if !sqlite3_c.exists() || !sqlite3_h.exists() {
                let make_out = Command::new("make")
                    .arg("-C")
                    .arg(&amalg_dir)
                    .arg("-f")
                    .arg(sqlite_src_dir.join("Makefile.linux-generic"))
                    .arg(format!("TOP={}", sqlite_src_dir.display()))
                    .arg("sqlite3.c")
                    .output()
                    .expect("build out-of-tree sqlite3.c");
                assert!(
                    make_out.status.success(),
                    "make sqlite3.c failed: {}",
                    String::from_utf8_lossy(&make_out.stderr)
                );
            }

            let sqlite3_o = ref_root.join("sqlite3.o");
            if !sqlite3_o.exists() {
                let cc_o = Command::new("cc")
                    .args(["-O2", "-pthread", "-c"])
                    .arg("-I")
                    .arg(&amalg_dir)
                    .arg(&sqlite3_c)
                    .arg("-o")
                    .arg(&sqlite3_o)
                    .output()
                    .expect("compile sqlite3.o");
                assert!(
                    cc_o.status.success(),
                    "compile sqlite3.o failed: {}",
                    String::from_utf8_lossy(&cc_o.stderr)
                );
            }

            let helper_c = ref_root.join("c_tcl_lock_helper.c");
            let helper_bin = ref_root.join("c_tcl_lock_helper");
            let need_build = match fs::read_to_string(&helper_c) {
                Ok(existing) => existing != C_TCL_LOCK_HELPER_SRC || !helper_bin.exists(),
                Err(_) => true,
            };
            if need_build {
                fs::write(&helper_c, C_TCL_LOCK_HELPER_SRC).expect("write c_tcl_lock_helper.c");
                let cc_bin = Command::new("cc")
                    .args(["-O2", "-pthread"])
                    .arg("-I")
                    .arg(&amalg_dir)
                    .arg(&helper_c)
                    .arg(&sqlite3_o)
                    .args(["-ldl", "-lm", "-o"])
                    .arg(&helper_bin)
                    .output()
                    .expect("compile c_tcl_lock_helper");
                assert!(
                    cc_bin.status.success(),
                    "compile c_tcl_lock_helper failed: {}",
                    String::from_utf8_lossy(&cc_bin.stderr)
                );
            }

            let status = Command::new("git")
                .arg("-C")
                .arg(workspace_root)
                .args(["status", "--porcelain", "--", "sqlite"])
                .output()
                .expect("git status sqlite");
            assert!(
                status.status.success() && status.stdout.is_empty(),
                "./sqlite/ must remain 100% clean"
            );

            helper_bin
        })
    }

    #[test]
    fn schema_free_prepare_and_open_succeed_under_c_sqlite_exclusive_lock_at_64k() {
        let tmp = TempTestDir::new("c_excl");
        let db_path = tmp.path().join("test.db");

        let sqlite_test_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("sqlite/test");
        let host = SqliteTestHost::new_shared("p1e_c_excl", sqlite_test_dir);
        let mut interp = TclInterp::new(tmp.path().to_path_buf(), Some(host.clone()));
        bootstrap_tester(&mut interp, &host, tmp.path()).expect("bootstrap tester");

        // Populate `test.db` via harness (1024 page size, 64 KiB pending byte) and close `db`.
        interp
            .eval(
                r#"
                db eval {
                  CREATE TABLE t1(x INT);
                  INSERT INTO t1 VALUES(777);
                }
                db close
                "#,
            )
            .expect("init test.db");

        // Acquire a real OS `BEGIN EXCLUSIVE` lock from out-of-tree C SQLite 3.54.1 at `0x10000` (`65536`).
        let helper = c_exclusive_lock_helper();
        let mut child = Command::new(helper)
            .arg(&db_path)
            .arg("65536")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn c_tcl_lock_helper");

        let mut line = String::new();
        {
            let stdout = child.stdout.as_mut().expect("child stdout");
            let mut reader = BufReader::new(stdout);
            reader
                .read_line(&mut line)
                .expect("read EXCLUSIVE_READY from C helper");
        }
        assert_eq!(line.trim(), "EXCLUSIVE_READY");

        // While C holds EXCLUSIVE on `test.db`:
        // - `sqlite3 db_ext test.db` succeeds (lazy schema load, zero page 1 read on open),
        // - schema-free `PRAGMA lock_status`, `BEGIN DEFERRED`, `SAVEPOINT`, `RELEASE`, `COMMIT` succeed,
        // - schema-requiring `SELECT * FROM t1` fails at `prepare` with `(5) database is locked` (`SQLITE_BUSY`).
        let while_locked_res = interp.eval(
            r#"
            set testprefix p1e_c_excl
            do_test 1.1 {
              sqlite3 db_ext test.db
              set st_ls [sqlite3_prepare_v2 db_ext "PRAGMA lock_status" -1]
              set rc_ls [sqlite3_step $st_ls]
              set ls_pair [list [sqlite3_column_text $st_ls 0] [sqlite3_column_text $st_ls 1]]
              sqlite3_finalize $st_ls

              db_ext eval {BEGIN DEFERRED}
              db_ext eval {SAVEPOINT sp_only_mem}
              db_ext eval {RELEASE sp_only_mem}
              db_ext eval {COMMIT}

              set sel_rc [catch {sqlite3_prepare_v2 db_ext "SELECT * FROM t1" -1} sel_err]
              set err_code [sqlite3_errcode db_ext]
              list $rc_ls $ls_pair $sel_rc $sel_err $err_code
            } {SQLITE_ROW {main unlocked} 1 {(5) database is locked} SQLITE_BUSY}
            "#,
        );
        assert!(
            while_locked_res.is_ok(),
            "while_locked script error: {:?}",
            while_locked_res
        );

        // Release C EXCLUSIVE lock cleanly.
        drop(child.stdin.take());
        let exit = child.wait().expect("wait C helper");
        assert!(exit.success());

        // Now `db_ext` reads `t1` cleanly!
        let after_unlock_res = interp.eval(
            r#"
            do_test 1.2 {
              set v [db_ext eval {SELECT x FROM t1}]
              db_ext close
              set v
            } {777}
            finish_test
            "#,
        );
        match after_unlock_res {
            Ok(_) | Err(TclControl::FinishTest) => {}
            Err(other) => panic!("after_unlock script error: {:?}", other),
        }

        let report = host.borrow().report.clone();
        assert!(report.is_ok(), "failed={:?}", report.failed);
        assert_eq!(report.passed, 2);
    }
}
