//! Phase 1.1 Foreign Key Regressions preserved from `sqlite/test/fkey3.test`
//! (`implementation_plan.md` lines 135-137, 301).
//!
//! Upstream `sqlite/test/fkey3.test` starts with `ifcapable {!foreignkey||!trigger}` even though
//! none of its 31 subtests create or invoke SQL triggers. With the Phase 1.1 capability table
//! setting `trigger = 0` (Phase 4) and enforcing the zero-subtest guard, `tests/sqlite_test/fkey3.rs`
//! is explicitly ignored until Phase 4 while all 31 FK subtests are actively verified here with
//! their exact source IDs, SQL statements, and expected results/errors.
#![forbid(unsafe_code)]

const FKEY3_31_SCENARIOS_TCL: &str = r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl

ifcapable {!foreignkey} {
  finish_test
  return
}

set testprefix fkey3

# Create a table and some data to work with.
#
do_test fkey3-1.1 {
  execsql {
    PRAGMA foreign_keys=ON;
    CREATE TABLE t1(x INTEGER PRIMARY KEY);
    INSERT INTO t1 VALUES(100);
    INSERT INTO t1 VALUES(101);
    CREATE TABLE t2(y INTEGER REFERENCES t1 (x));
    INSERT INTO t2 VALUES(100);
    INSERT INTO t2 VALUES(101);
    SELECT 1, x FROM t1;
    SELECT 2, y FROM t2;
  }
} {1 100 1 101 2 100 2 101}

do_test fkey3-1.2 {
  catchsql {
    DELETE FROM t1 WHERE x=100;
  }
} {1 {FOREIGN KEY constraint failed}}

do_test fkey3-1.3 {
  catchsql {
    DROP TABLE t1;
  }
} {1 {FOREIGN KEY constraint failed}}

do_test fkey3-1.4 {
  execsql {
    DROP TABLE t2;
  }
} {}

do_test fkey3-1.5 {
  execsql {
    DROP TABLE t1;
  }
} {}

do_test fkey3-2.1 {
  execsql {
    PRAGMA foreign_keys=ON;
    CREATE TABLE t1(x INTEGER PRIMARY KEY);
    INSERT INTO t1 VALUES(100);
    INSERT INTO t1 VALUES(101);
    CREATE TABLE t2(y INTEGER PRIMARY KEY REFERENCES t1 (x) ON UPDATE SET NULL);
  }
  execsql {
    INSERT INTO t2 VALUES(100);
    INSERT INTO t2 VALUES(101);
    SELECT 1, x FROM t1;
    SELECT 2, y FROM t2;
  }
} {1 100 1 101 2 100 2 101}


#-------------------------------------------------------------------------
# The following tests - fkey-3.* - test some edge cases to do with 
# inserting rows into tables that have foreign keys where the parent
# table is the same as the child table. Especially cases where the
# new row being inserted matches itself.
#
do_execsql_test 3.1.1 {
  CREATE TABLE t3(a, b, c, d, 
    UNIQUE(a, b),
    FOREIGN KEY(c, d) REFERENCES t3(a, b)
  );
  INSERT INTO t3 VALUES(1, 2, 1, 2);
} {}
do_catchsql_test 3.1.2 {
  INSERT INTO t3 VALUES(NULL, 2, 5, 2);
} {1 {FOREIGN KEY constraint failed}}
do_catchsql_test 3.1.3 {
  INSERT INTO t3 VALUES(NULL, 3, 5, 2);
} {1 {FOREIGN KEY constraint failed}}

do_execsql_test 3.2.1 {
  CREATE TABLE t4(a UNIQUE, b REFERENCES t4(a));
}
do_catchsql_test 3.2.2 {
  INSERT INTO t4 VALUES(NULL, 1);
} {1 {FOREIGN KEY constraint failed}}

do_execsql_test 3.3.1 {
  CREATE TABLE t5(a INTEGER PRIMARY KEY, b REFERENCES t5(a));
  INSERT INTO t5 VALUES(NULL, 1);
} {}
do_catchsql_test 3.3.2 {
  INSERT INTO t5 VALUES(NULL, 3);
} {1 {FOREIGN KEY constraint failed}}

do_execsql_test 3.4.1 {
  CREATE TABLE t6(a INTEGER PRIMARY KEY, b, c, d,
    FOREIGN KEY(c, d) REFERENCES t6(a, b)
  );
  CREATE UNIQUE INDEX t6i ON t6(b, a);
}
do_execsql_test 3.4.2  { INSERT INTO t6 VALUES(NULL, 'a', 1, 'a'); } {}
do_execsql_test 3.4.3  { INSERT INTO t6 VALUES(2, 'a', 2, 'a');    } {}
do_execsql_test 3.4.4  { INSERT INTO t6 VALUES(NULL, 'a', 1, 'a'); } {}
do_execsql_test 3.4.5  { INSERT INTO t6 VALUES(5, 'a', 2, 'a'); } {}
do_catchsql_test 3.4.6 { 
  INSERT INTO t6 VALUES(NULL, 'a', 65, 'a');    
} {1 {FOREIGN KEY constraint failed}}

do_execsql_test 3.4.7 {
  INSERT INTO t6 VALUES(100, 'one', 100, 'one');
  DELETE FROM t6 WHERE a = 100;
}
do_execsql_test 3.4.8 {
  INSERT INTO t6 VALUES(100, 'one', 100, 'one');
  UPDATE t6 SET c = 1, d = 'a' WHERE a = 100;
  DELETE FROM t6 WHERE a = 100;
}

do_execsql_test 3.5.1 {
  CREATE TABLE t7(a, b, c, d INTEGER PRIMARY KEY,
    FOREIGN KEY(c, d) REFERENCES t7(a, b)
  );
  CREATE UNIQUE INDEX t7i ON t7(a, b);
}
do_execsql_test 3.5.2  { INSERT INTO t7 VALUES('x', 1, 'x', NULL) } {}
do_execsql_test 3.5.3  { INSERT INTO t7 VALUES('x', 2, 'x', 2) } {}
do_catchsql_test 3.5.4  { 
  INSERT INTO t7 VALUES('x', 450, 'x', NULL);
} {1 {FOREIGN KEY constraint failed}}
do_catchsql_test 3.5.5  { 
  INSERT INTO t7 VALUES('x', 450, 'x', 451);
} {1 {FOREIGN KEY constraint failed}}


do_execsql_test 3.6.1 {
  CREATE TABLE t8(a, b, c, d, e, FOREIGN KEY(c, d) REFERENCES t8(a, b));
  CREATE UNIQUE INDEX t8i1 ON t8(a, b);
  CREATE UNIQUE INDEX t8i2 ON t8(c);
  INSERT INTO t8 VALUES(1, 1, 1, 1, 1);
}
do_catchsql_test 3.6.2 { 
  UPDATE t8 SET d = 2; 
} {1 {FOREIGN KEY constraint failed}}
do_execsql_test 3.6.3 { UPDATE t8 SET d = 1; }
do_execsql_test 3.6.4 { UPDATE t8 SET e = 2; }

do_catchsql_test 3.6.5 {
  CREATE TABLE TestTable (
    id INTEGER PRIMARY KEY,
    name text,
    source_id integer not null,
    parent_id integer,

    foreign key(source_id, parent_id) references TestTable(source_id, id)
  );
  CREATE UNIQUE INDEX testindex on TestTable(source_id, id);
  PRAGMA foreign_keys=1;
  INSERT INTO TestTable VALUES (1, 'parent', 1, null);
  INSERT INTO TestTable VALUES (2, 'child', 1, 1);
  UPDATE TestTable SET parent_id=1000 where id=2;
} {1 {FOREIGN KEY constraint failed}}

finish_test
"#;

#[test]
fn fkey3_all_31_foreign_key_scenarios_pass_without_trigger_capability() {
    assert_eq!(
        sqliters_tcltest::phase1_capability("foreignkey"),
        Some(true)
    );
    assert_eq!(sqliters_tcltest::phase1_capability("trigger"), Some(false));

    let report = sqliters_tcltest::evaluate_test_script("fkey3_phase1", FKEY3_31_SCENARIOS_TCL);
    assert!(
        report.is_ok(),
        "expected all 31 fkey3 subtests to pass: passed={}, failed={:?}, script_error={:?}",
        report.passed,
        report.failed,
        report.script_error,
    );
    assert_eq!(
        report.passed, 31,
        "all 31 fkey3 subtests must execute and pass"
    );
    assert!(report.failed.is_empty());
    assert!(report.script_error.is_none());
    assert!(!report.skipped_zero_subtests);
    assert!(report.unknown_capabilities.is_empty());
}

#[test]
fn original_fkey3_trigger_guard_triggers_zero_subtest_detection_in_phase1() {
    let original_fkey3 = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sqlite/test/fkey3.test"),
    )
    .expect("read sqlite/test/fkey3.test");

    let report = sqliters_tcltest::evaluate_test_script("fkey3_orig_guard", &original_fkey3);
    assert_eq!(report.passed, 0);
    assert!(report.failed.is_empty());
    assert!(report.script_error.is_none());
    assert!(report.skipped_zero_subtests);
    assert!(
        !report.is_ok(),
        "zero-subtest script must not be treated as passing by TestFileReport::is_ok()"
    );
}
