#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use sqliters::types::extended;
use sqliters::{
    AuthorizerAction, AuthorizerResult, Connection, ErrorCode, FunctionFlags, Result,
    RowMutationOp, RuntimeValue, SqliteError, StepResult, Value, WindowAggregateInstance,
};

fn query_vals(conn: &Connection, sql: &str) -> Vec<Vec<Value>> {
    conn.query(sql, &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r.into_values())
        .collect()
}

// ============================================================================
// Group 54: `REGEXP`, `MATCH`, `NOT REGEXP`, `NOT MATCH`, and `like`/`glob` Overrides
// ============================================================================

#[test]
fn test_group54_regexp_match_unregistered_and_custom_registration() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE words(id INTEGER PRIMARY KEY, w TEXT);")
        .unwrap();
    conn.execute("INSERT INTO words VALUES (1, 'alpha'), (2, 'beta'), (3, 'alphanumeric');")
        .unwrap();

    // 1. Unregistered `REGEXP` and `MATCH` must fail with `no such function: regexp` / `match`
    let err_re = conn
        .query("SELECT w FROM words WHERE w REGEXP '^alpha';", &[])
        .unwrap_err();
    assert_eq!(err_re.code, ErrorCode::Error);
    assert!(
        err_re.message.contains("no such function: regexp"),
        "unexpected error: {err_re}"
    );

    let err_match = conn
        .query("SELECT w FROM words WHERE w MATCH 'alpha*';", &[])
        .unwrap_err();
    assert_eq!(err_match.code, ErrorCode::Error);
    assert!(
        err_match.message.contains("no such function: match"),
        "unexpected error: {err_match}"
    );

    // 2. Register 2-arg `regexp(pattern, value)` and verify reversed argument order `(Y, X)`!
    let seen_calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen_calls);
    conn.create_scalar_function("regexp", 2, FunctionFlags::DETERMINISTIC, move |args| {
        if args[0].value.is_null() || args[1].value.is_null() {
            return Ok(RuntimeValue::from(Value::Null));
        }
        let pat = args[0].value.to_text_lossy();
        let val = args[1].value.to_text_lossy();
        seen_clone.lock().unwrap().push((pat.clone(), val.clone()));
        let matched = if let Some(prefix) = pat.strip_prefix('^') {
            val.starts_with(prefix)
        } else {
            val.contains(&pat)
        };
        Ok(RuntimeValue::from(Value::Integer(i64::from(matched))))
    })
    .unwrap();

    let rows = query_vals(
        &conn,
        "SELECT id, w FROM words WHERE w REGEXP '^alpha' ORDER BY id;",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1), Value::Text("alpha".into())],
            vec![Value::Integer(3), Value::Text("alphanumeric".into())],
        ]
    );
    // Verify `(pattern, value)` argument order (`^alpha` is arg[0], table column `w` is arg[1])
    {
        let calls = seen_calls.lock().unwrap();
        assert!(!calls.is_empty());
        assert_eq!(calls[0], ("^alpha".to_owned(), "alpha".to_owned()));
    }

    // `NOT REGEXP`
    let not_rows = query_vals(
        &conn,
        "SELECT id FROM words WHERE w NOT REGEXP '^alpha' ORDER BY id;",
    );
    assert_eq!(not_rows, vec![vec![Value::Integer(2)]]);

    // `NULL` propagation with `REGEXP` / `NOT REGEXP`
    let null_rows = query_vals(
        &conn,
        "SELECT NULL REGEXP '^a', NULL NOT REGEXP '^a', 'a' REGEXP NULL;",
    );
    assert_eq!(null_rows, vec![vec![Value::Null, Value::Null, Value::Null]]);

    // 3-arg `REGEXP ... ESCAPE` fails when only 2-arg `regexp` is registered
    let esc_err = conn
        .query("SELECT 'alpha' REGEXP '^a' ESCAPE '\\';", &[])
        .unwrap_err();
    assert_eq!(esc_err.code, ErrorCode::Error);
    assert!(
        esc_err
            .message
            .contains("wrong number of arguments to function regexp()"),
        "unexpected error: {esc_err}"
    );

    // Register 3-arg `regexp` and verify `ESCAPE` works
    conn.create_scalar_function("regexp", 3, FunctionFlags::DETERMINISTIC, |args| {
        let pat = args[0].value.to_text_lossy();
        let val = args[1].value.to_text_lossy();
        let esc = args[2].value.to_text_lossy();
        let matched = val.starts_with(&pat) && esc == "\\";
        Ok(RuntimeValue::from(Value::Integer(i64::from(matched))))
    })
    .unwrap();
    let esc_rows = query_vals(
        &conn,
        "SELECT 'alpha' REGEXP 'alp' ESCAPE '\\', 'alpha' NOT REGEXP 'alp' ESCAPE '\\';",
    );
    assert_eq!(esc_rows, vec![vec![Value::Integer(1), Value::Integer(0)]]);

    // 3. Register 2-arg `match(pattern, value)` and verify `MATCH` / `NOT MATCH`
    conn.create_scalar_function("match", 2, FunctionFlags::DETERMINISTIC, |args| {
        let pat = args[0].value.to_text_lossy();
        let val = args[1].value.to_text_lossy();
        let matched = val.ends_with(&pat);
        Ok(RuntimeValue::from(Value::Integer(i64::from(matched))))
    })
    .unwrap();
    let match_rows = query_vals(
        &conn,
        "SELECT id FROM words WHERE w MATCH 'ta' ORDER BY id;",
    );
    assert_eq!(match_rows, vec![vec![Value::Integer(2)]]);
    let not_match_rows = query_vals(
        &conn,
        "SELECT id FROM words WHERE w NOT MATCH 'ta' ORDER BY id;",
    );
    assert_eq!(
        not_match_rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );

    // 4. Custom `like(pattern, value)` and `glob(pattern, value)` overrides
    conn.create_scalar_function("like", 2, FunctionFlags::DETERMINISTIC, |args| {
        let pat = args[0].value.to_text_lossy();
        let val = args[1].value.to_text_lossy();
        Ok(RuntimeValue::from(Value::Integer(i64::from(
            pat == "CUSTOM_PAT" && val == "beta",
        ))))
    })
    .unwrap();
    let custom_like = query_vals(&conn, "SELECT id FROM words WHERE w LIKE 'CUSTOM_PAT';");
    assert_eq!(custom_like, vec![vec![Value::Integer(2)]]);

    // Unregistering custom `like` restores built-in `LIKE`
    assert!(conn.remove_scalar_function("like", 2).unwrap());
    let builtin_like = query_vals(
        &conn,
        "SELECT id FROM words WHERE w LIKE 'ALP%' ORDER BY id;",
    );
    assert_eq!(
        builtin_like,
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );
}

// ============================================================================
// Group 21: Custom Collations, Fallible Collations, Panic Isolation & Rebinding
// ============================================================================

#[test]
fn test_group21_custom_collation_order_group_distinct_index_and_without_rowid() {
    let conn = Connection::open_in_memory().unwrap();

    // Missing collation is rejected at DDL / prepare time
    let missing_err = conn
        .execute("CREATE TABLE bad(x TEXT COLLATE no_such_coll);")
        .unwrap_err();
    assert_eq!(missing_err.code, ErrorCode::Error);
    assert!(
        missing_err
            .message
            .contains("no such collation sequence: no_such_coll"),
        "unexpected error: {missing_err}"
    );

    // Register a custom collation that compares strings by integer value when numeric, or by length then ASCII
    conn.create_collation("NUM_LEN", |a, b| {
        match (a.trim().parse::<i64>(), b.trim().parse::<i64>()) {
            (Ok(ia), Ok(ib)) => ia.cmp(&ib),
            _ => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
        }
    })
    .unwrap();

    conn.execute(
        "CREATE TABLE items(
            id INTEGER PRIMARY KEY,
            code TEXT COLLATE NUM_LEN,
            tag TEXT
        );
        CREATE INDEX idx_items_code ON items(code);
        INSERT INTO items VALUES (1, '100', 'a'), (2, '20', 'b'), (3, '3', 'c'), (4, '020', 'd');",
    )
    .unwrap();

    // 1. ORDER BY using custom collation (`'3' < '20'/'020' < '100'`)
    let ordered = query_vals(
        &conn,
        "SELECT id, code FROM items ORDER BY code ASC, id ASC;",
    );
    assert_eq!(
        ordered,
        vec![
            vec![Value::Integer(3), Value::Text("3".into())],
            vec![Value::Integer(2), Value::Text("20".into())],
            vec![Value::Integer(4), Value::Text("020".into())],
            vec![Value::Integer(1), Value::Text("100".into())],
        ]
    );

    // 2. Index equality seek where `'20'` == `'020'` under `NUM_LEN`
    let eq_rows = query_vals(&conn, "SELECT id FROM items WHERE code = '20' ORDER BY id;");
    assert_eq!(
        eq_rows,
        vec![vec![Value::Integer(2)], vec![Value::Integer(4)]]
    );

    // 3. Index range seek (`code > '20'` -> only `'100'`)
    let gt_rows = query_vals(&conn, "SELECT id FROM items WHERE code > '20' ORDER BY id;");
    assert_eq!(gt_rows, vec![vec![Value::Integer(1)]]);

    // 4. GROUP BY and DISTINCT with custom collation (`'20'` and `'020'` group together!)
    let grouped = query_vals(
        &conn,
        "SELECT COUNT(*) FROM items GROUP BY code ORDER BY code;",
    );
    assert_eq!(
        grouped,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(1)],
        ]
    );
    let distinct_cnt = query_vals(&conn, "SELECT COUNT(DISTINCT code) FROM items;");
    assert_eq!(distinct_cnt, vec![vec![Value::Integer(3)]]);

    // 5. WITHOUT ROWID table with custom collation primary key
    conn.execute(
        "CREATE TABLE wor_items(
            k TEXT COLLATE NUM_LEN PRIMARY KEY,
            v INTEGER
        ) WITHOUT ROWID;
        INSERT INTO wor_items VALUES ('100', 1), ('20', 2), ('3', 3);",
    )
    .unwrap();
    // Duplicate under `NUM_LEN` (`'020'` == `'20'`) must violate `UNIQUE`!
    let dup_err = conn
        .execute("INSERT INTO wor_items VALUES ('020', 4);")
        .unwrap_err();
    assert_eq!(dup_err.code, ErrorCode::Constraint);

    let wor_rows = query_vals(
        &conn,
        "SELECT k, v FROM wor_items WHERE k >= '20' ORDER BY k;",
    );
    assert_eq!(
        wor_rows,
        vec![
            vec![Value::Text("20".into()), Value::Integer(2)],
            vec![Value::Text("100".into()), Value::Integer(1)],
        ]
    );
}

#[test]
fn test_group21_persistent_schema_reload_and_late_collation_rebinding() {
    let dir = std::env::temp_dir().join(format!("sqliters_p6_coll_rebind_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("coll.db");

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.create_collation("REV_ASCII", |a, b| b.cmp(a)).unwrap();
        conn.execute(
            "CREATE TABLE t_rev(id INTEGER PRIMARY KEY, k TEXT COLLATE REV_ASCII UNIQUE);
             INSERT INTO t_rev VALUES (1, 'apple'), (2, 'zebra'), (3, 'mango');",
        )
        .unwrap();
    }

    // Reopen without registering `REV_ASCII` yet: opening and querying `id` works,
    // but sorting or inserting on `k` fails with `no such collation sequence: REV_ASCII`
    // until `create_collation("REV_ASCII", ...)` is called!
    {
        let conn2 = Connection::open(&db_path).unwrap();
        let unbound_err = conn2
            .query("SELECT k FROM t_rev ORDER BY k;", &[])
            .unwrap_err();
        assert!(
            unbound_err
                .message
                .contains("no such collation sequence: REV_ASCII"),
            "unexpected error: {unbound_err}"
        );

        // Now register `REV_ASCII` on `conn2` -> schema indexes & columns rebind automatically!
        conn2
            .create_collation("REV_ASCII", |a, b| b.cmp(a))
            .unwrap();
        let rows = query_vals(&conn2, "SELECT k FROM t_rev ORDER BY k;");
        assert_eq!(
            rows,
            vec![
                vec![Value::Text("zebra".into())],
                vec![Value::Text("mango".into())],
                vec![Value::Text("apple".into())],
            ]
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_group21_fallible_and_panicking_collation_error_propagation_and_rollback() {
    let conn = Connection::open_in_memory().unwrap();

    conn.create_fallible_collation("FAIL_ON_BAD", |a, b| {
        if a == "BOOM" || b == "BOOM" {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "custom collation rejected BOOM",
            ));
        }
        Ok(a.cmp(b))
    })
    .unwrap();

    conn.create_collation("PANIC_ON_BAD", |a, b| {
        if a == "PANIC" || b == "PANIC" {
            panic!("collation callback exploded");
        }
        a.cmp(b)
    })
    .unwrap();

    conn.execute(
        "CREATE TABLE t_coll(id INTEGER PRIMARY KEY, c TEXT COLLATE FAIL_ON_BAD);
         CREATE INDEX idx_t_coll ON t_coll(c);
         INSERT INTO t_coll VALUES (1, 'alpha'), (2, 'beta');",
    )
    .unwrap();

    // Multi-row insert inside a transaction where the 2nd row triggers a fallible collation error:
    // statement must abort and rollback the 1st row!
    conn.execute("BEGIN;").unwrap();
    let err = conn
        .execute("INSERT INTO t_coll VALUES (3, 'gamma'), (4, 'BOOM');")
        .unwrap_err();
    assert!(
        err.message.contains("custom collation rejected BOOM"),
        "unexpected error: {err}"
    );
    // Statement savepoint rolled back row 3 while transaction stays open with rows 1 and 2
    let rows = query_vals(&conn, "SELECT id, c FROM t_coll ORDER BY id;");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1), Value::Text("alpha".into())],
            vec![Value::Integer(2), Value::Text("beta".into())],
        ]
    );
    conn.execute("COMMIT;").unwrap();

    // Panicking collation in `ORDER BY` is caught by `catch_unwind` and converted to `Err`
    let panic_err = conn
        .query(
            "SELECT x FROM (SELECT 'ok' AS x UNION ALL SELECT 'PANIC' AS x) ORDER BY x COLLATE PANIC_ON_BAD;",
            &[],
        )
        .unwrap_err();
    assert_eq!(panic_err.code, ErrorCode::Error);
    assert!(
        panic_err.message.contains("collation callback exploded"),
        "unexpected panic error: {panic_err}"
    );

    // Connection remains completely usable after collation panic
    let check = query_vals(&conn, "SELECT COUNT(*) FROM t_coll;");
    assert_eq!(check, vec![vec![Value::Integer(2)]]);
}

// ============================================================================
// Groups 21, 23, 65: Scalar / Aggregate / Window UDFs, Panic Isolation & Reentrancy
// ============================================================================

struct RunningSumWindow {
    sum: i64,
    cnt: i64,
}

impl WindowAggregateInstance for RunningSumWindow {
    fn step(&mut self, args: &[RuntimeValue]) -> Result<()> {
        if let Some(a) = args.first() {
            let v = a.value.to_integer_lossy();
            if v == -999 {
                panic!("window step panicked on -999");
            }
            self.sum += v;
            self.cnt += 1;
        }
        Ok(())
    }

    fn value(&self) -> Result<RuntimeValue> {
        Ok(RuntimeValue::from(Value::Integer(self.sum)))
    }

    fn inverse(&mut self, args: &[RuntimeValue]) -> Result<()> {
        if let Some(a) = args.first() {
            self.sum -= a.value.to_integer_lossy();
            self.cnt -= 1;
        }
        Ok(())
    }

    fn finalize(&mut self) -> Result<RuntimeValue> {
        Ok(RuntimeValue::from(Value::Integer(self.sum)))
    }
}

#[test]
fn test_group21_23_65_udf_window_panic_isolation_and_reentrancy_protection() {
    let conn = Arc::new(Connection::open_in_memory().unwrap());
    conn.execute(
        "CREATE TABLE nums(id INTEGER PRIMARY KEY, n INTEGER);
         INSERT INTO nums VALUES (1, 10), (2, 20), (3, 30), (4, 40);",
    )
    .unwrap();

    // 1. Register custom window function supporting sliding frames (`step`, `value`, `inverse`, `finalize`)
    conn.create_window_function("win_sum", 1, FunctionFlags::DETERMINISTIC, || {
        Box::new(RunningSumWindow { sum: 0, cnt: 0 })
    })
    .unwrap();

    let win_rows = query_vals(
        &conn,
        "SELECT id, win_sum(n) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)
         FROM nums ORDER BY id;",
    );
    assert_eq!(
        win_rows,
        vec![
            vec![Value::Integer(1), Value::Integer(10)],
            vec![Value::Integer(2), Value::Integer(30)],
            vec![Value::Integer(3), Value::Integer(50)],
            vec![Value::Integer(4), Value::Integer(70)],
        ]
    );

    // Window step panic is isolated by `catch_unwind`
    let win_panic = conn
        .query("SELECT win_sum(-999) OVER () FROM nums;", &[])
        .unwrap_err();
    assert!(
        win_panic.message.contains("window step panicked on -999"),
        "unexpected error: {win_panic}"
    );

    // 2. Same-thread reentrancy protection + lock-free observation inside a scalar UDF
    let conn_in_udf = Arc::clone(&conn);
    conn.create_scalar_function("check_reentrant", 1, FunctionFlags::NONE, move |args| {
        // Lock-free scalar getters MUST succeed without locking `ConnectionInner`
        let _changes = conn_in_udf.changes();
        let _total = conn_in_udf.total_changes();
        let _last_id = conn_in_udf.last_insert_rowid();
        let _autocommit = conn_in_udf.is_autocommit();
        let _rb_cnt = conn_in_udf.rollback_count();
        let _ro = conn_in_udf.is_readonly();
        let _ih = conn_in_udf.interrupt_handle();

        // Attempting to re-enter the connection gate on the same thread MUST return `ErrorCode::Misuse`!
        let re_exec = conn_in_udf.execute("SELECT 1;");
        assert!(
            matches!(re_exec, Err(ref e) if e.code == ErrorCode::Misuse),
            "expected Misuse on same-thread execute reentrancy, got: {re_exec:?}"
        );
        let re_prep = conn_in_udf.prepare("SELECT 1;");
        assert!(
            matches!(re_prep, Err(ref e) if e.code == ErrorCode::Misuse),
            "expected Misuse on same-thread prepare reentrancy, got: {re_prep:?}"
        );

        let x = args[0].value.to_integer_lossy();
        if x < 0 {
            panic!("scalar udf panic on negative input {x}");
        }
        Ok(RuntimeValue::from(Value::Integer(x * 2)))
    })
    .unwrap();

    let ok_rows = query_vals(&conn, "SELECT check_reentrant(21);");
    assert_eq!(ok_rows, vec![vec![Value::Integer(42)]]);

    let udf_panic = conn.query("SELECT check_reentrant(-5);", &[]).unwrap_err();
    assert!(
        udf_panic
            .message
            .contains("scalar udf panic on negative input -5"),
        "unexpected error: {udf_panic}"
    );

    // Verify statement rollback on UDF panic mid-write
    let panic_write = conn
        .execute("INSERT INTO nums VALUES (5, 50), (6, check_reentrant(-1));")
        .unwrap_err();
    assert!(
        panic_write
            .message
            .contains("scalar udf panic on negative input -1")
    );
    let count_after = query_vals(&conn, "SELECT COUNT(*) FROM nums;");
    assert_eq!(count_after, vec![vec![Value::Integer(4)]]);

    // 3. `DIRECTONLY` flag rejection inside Triggers/Views and `SUBTYPE` / `RESULT_SUBTYPE` contract
    conn.create_scalar_function("secret_fn", 0, FunctionFlags::DIRECTONLY, |_| {
        Ok(RuntimeValue::from(Value::Integer(7)))
    })
    .unwrap();
    conn.execute("CREATE VIEW v_secret AS SELECT secret_fn();")
        .unwrap();
    let direct_err = conn.query("SELECT * FROM v_secret;", &[]).unwrap_err();
    assert!(
        direct_err.message.contains("unsafe use of secret_fn()"),
        "unexpected error: {direct_err}"
    );

    conn.create_scalar_function("emit_sub", 1, FunctionFlags::RESULT_SUBTYPE, |args| {
        Ok(RuntimeValue::with_subtype(args[0].value.clone(), 0x4A))
    })
    .unwrap();
    conn.create_scalar_function("read_sub", 1, FunctionFlags::SUBTYPE, |args| {
        Ok(RuntimeValue::from(Value::Integer(i64::from(
            args[0].subtype,
        ))))
    })
    .unwrap();
    conn.create_scalar_function("read_nosub", 1, FunctionFlags::NONE, |args| {
        Ok(RuntimeValue::from(Value::Integer(i64::from(
            args[0].subtype,
        ))))
    })
    .unwrap();
    let sub_rows = query_vals(
        &conn,
        "SELECT read_sub(emit_sub('x')), read_nosub(emit_sub('x'));",
    );
    assert_eq!(
        sub_rows,
        vec![vec![Value::Integer(0x4A), Value::Integer(0)]]
    );
}

// ============================================================================
// Groups 51, 52: `busy_handler`, `progress_handler`, and `InterruptHandle`
// ============================================================================

#[test]
fn test_group51_52_progress_handler_interrupt_and_panic_isolation() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE big(id INTEGER PRIMARY KEY, v INTEGER);
         WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 50)
         INSERT INTO big SELECT x, x * 10 FROM c;",
    )
    .unwrap();

    // 1. Progress handler returning `true` aborts query with `ErrorCode::Interrupt`
    let ticks = Arc::new(AtomicUsize::new(0));
    let ticks_clone = Arc::clone(&ticks);
    conn.progress_handler(
        4,
        Some(Box::new(move || {
            let cur = ticks_clone.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            cur >= 3
        })),
    )
    .unwrap();

    let err = conn
        .query("SELECT SUM(a.v + b.v) FROM big a CROSS JOIN big b;", &[])
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Interrupt);
    assert!(ticks.load(AtomicOrdering::SeqCst) >= 3);

    // 2. Progress handler aborting mid-DML rolls back partial changes
    conn.progress_handler(2, Some(Box::new(|| true))).unwrap();
    let dml_err = conn.execute("UPDATE big SET v = v + 1;").unwrap_err();
    assert_eq!(dml_err.code, ErrorCode::Interrupt);

    // Clear progress handler and verify no rows were modified
    conn.progress_handler(0, None).unwrap();
    let sum_v = query_vals(&conn, "SELECT SUM(v) FROM big;");
    assert_eq!(sum_v, vec![vec![Value::Integer(12750)]]);

    // 3. Panicking progress handler is isolated by `catch_unwind`
    conn.progress_handler(
        2,
        Some(Box::new(|| -> bool {
            panic!("progress handler exploded");
        })),
    )
    .unwrap();
    let p_err = conn.query("SELECT SUM(v) FROM big;", &[]).unwrap_err();
    assert!(
        p_err.message.contains("progress handler exploded"),
        "unexpected error: {p_err}"
    );
    conn.progress_handler(0, None).unwrap();
}

// ============================================================================
// Groups 53, 57: `set_authorizer` (`Ok`, `Deny`, `Ignore`, `zAuthContext`, Advisory Expire)
// ============================================================================

#[test]
fn test_group53_57_authorizer_ok_deny_ignore_and_view_trigger_context() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE emp(id INTEGER PRIMARY KEY, name TEXT, salary INTEGER, dept TEXT);
         INSERT INTO emp VALUES (1, 'Alice', 90000, 'eng'), (2, 'Bob', 80000, 'sales');
         CREATE VIEW emp_view AS SELECT id, name, salary FROM emp;
         CREATE TABLE audit_log(msg TEXT);
         CREATE TRIGGER tr_emp_upd AFTER UPDATE ON emp BEGIN
             INSERT INTO audit_log VALUES (new.name);
         END;",
    )
    .unwrap();

    // 1. Column-level `SQLITE_READ` returning `AuthorizerResult::Ignore` replaces `salary` with `NULL`
    // while keeping column names intact, even in `SELECT *` and inside `emp_view`!
    #[allow(clippy::type_complexity)]
    let auth_events: Arc<
        Mutex<
            Vec<(
                AuthorizerAction,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            )>,
        >,
    > = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&auth_events);

    conn.set_authorizer(Some(Box::new(
        move |action, arg1, arg2, db_name, auth_ctx| {
            events_clone.lock().unwrap().push((
                action,
                arg1.map(str::to_owned),
                arg2.map(str::to_owned),
                db_name.map(str::to_owned),
                auth_ctx.map(str::to_owned),
            ));
            if action == AuthorizerAction::Read && arg1 == Some("emp") && arg2 == Some("salary") {
                return AuthorizerResult::Ignore;
            }
            AuthorizerResult::Ok
        },
    )))
    .unwrap();

    let res = conn
        .query("SELECT id, name, salary FROM emp ORDER BY id;", &[])
        .unwrap();
    assert_eq!(res.column_names, vec!["id", "name", "salary"]);
    let res_vals: Vec<Vec<Value>> = res.rows.into_iter().map(|r| r.into_values()).collect();
    assert_eq!(
        res_vals,
        vec![
            vec![Value::Integer(1), Value::Text("Alice".into()), Value::Null],
            vec![Value::Integer(2), Value::Text("Bob".into()), Value::Null],
        ]
    );

    // `SELECT *` expansion when one column returns `SQLITE_IGNORE`
    let star_res = conn.query("SELECT * FROM emp WHERE id = 1;", &[]).unwrap();
    assert_eq!(star_res.column_names, vec!["id", "name", "salary", "dept"]);
    let star_vals: Vec<Vec<Value>> = star_res.rows.into_iter().map(|r| r.into_values()).collect();
    assert_eq!(
        star_vals,
        vec![vec![
            Value::Integer(1),
            Value::Text("Alice".into()),
            Value::Null,
            Value::Text("eng".into()),
        ]]
    );

    // Reading through `emp_view` records `zAuthContext == Some("emp_view")` and masks `salary` to `NULL`
    auth_events.lock().unwrap().clear();
    let view_vals = query_vals(&conn, "SELECT id, name, salary FROM emp_view ORDER BY id;");
    assert_eq!(
        view_vals,
        vec![
            vec![Value::Integer(1), Value::Text("Alice".into()), Value::Null],
            vec![Value::Integer(2), Value::Text("Bob".into()), Value::Null],
        ]
    );
    {
        let evs = auth_events.lock().unwrap();
        assert!(
            evs.iter().any(|(act, a1, a2, db, ctx)| {
                *act == AuthorizerAction::Read
                    && a1.as_deref() == Some("emp")
                    && a2.as_deref() == Some("salary")
                    && db.as_deref() == Some("main")
                    && ctx.as_deref() == Some("emp_view")
            }),
            "expected Read on emp.salary with zAuthContext == Some(\"emp_view\"), got: {evs:?}"
        );
    }

    // 2. `SQLITE_UPDATE` returning `Ignore` on `salary` skips `salary` update while updating `dept`,
    // and records `zAuthContext == Some("tr_emp_upd")` for the trigger body!
    auth_events.lock().unwrap().clear();
    conn.set_authorizer(Some(Box::new({
        let events_clone = Arc::clone(&auth_events);
        move |action, arg1, arg2, db_name, auth_ctx| {
            events_clone.lock().unwrap().push((
                action,
                arg1.map(str::to_owned),
                arg2.map(str::to_owned),
                db_name.map(str::to_owned),
                auth_ctx.map(str::to_owned),
            ));
            if action == AuthorizerAction::Update && arg1 == Some("emp") && arg2 == Some("salary") {
                return AuthorizerResult::Ignore;
            }
            AuthorizerResult::Ok
        }
    })))
    .unwrap();

    conn.execute("UPDATE emp SET salary = 999999, dept = 'r&d' WHERE id = 1;")
        .unwrap();
    conn.clear_authorizer().unwrap();

    let after_upd = query_vals(&conn, "SELECT salary, dept FROM emp WHERE id = 1;");
    assert_eq!(
        after_upd,
        vec![vec![Value::Integer(90000), Value::Text("r&d".into())]]
    );
    {
        let evs = auth_events.lock().unwrap();
        assert!(
            evs.iter().any(|(act, a1, _, _, ctx)| {
                *act == AuthorizerAction::Insert
                    && a1.as_deref() == Some("audit_log")
                    && ctx.as_deref() == Some("tr_emp_upd")
            }),
            "expected Insert on audit_log with zAuthContext == Some(\"tr_emp_upd\"), got: {evs:?}"
        );
    }

    // 3. `AuthorizerResult::Deny` returns `ErrorCode::Auth` (`SQLITE_AUTH`)
    conn.set_authorizer(Some(Box::new(|action, _, _, _, _| {
        if action == AuthorizerAction::DropTable {
            AuthorizerResult::Deny
        } else {
            AuthorizerResult::Ok
        }
    })))
    .unwrap();
    let deny_err = conn.execute("DROP TABLE emp;").unwrap_err();
    assert_eq!(deny_err.code, ErrorCode::Auth);
    assert!(deny_err.message.contains("not authorized"));
    conn.clear_authorizer().unwrap();
}

#[test]
fn test_group53_57_authorizer_advisory_expire_v2_vs_v1_vs_running() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE t_exp(id INTEGER PRIMARY KEY, a TEXT, b TEXT);
         INSERT INTO t_exp VALUES (1, 'A1', 'B1'), (2, 'A2', 'B2');",
    )
    .unwrap();

    let mut stmt_v2 = conn.prepare("SELECT a, b FROM t_exp ORDER BY id;").unwrap();
    let mut stmt_v1 = conn
        .prepare_v1("SELECT a, b FROM t_exp ORDER BY id;")
        .unwrap();
    let mut stmt_running = conn.prepare("SELECT a, b FROM t_exp ORDER BY id;").unwrap();

    // Advance `stmt_running` to the first row BEFORE changing the authorizer
    match stmt_running.step().unwrap() {
        StepResult::Row(r) => assert_eq!(
            r.into_values(),
            vec![Value::Text("A1".into()), Value::Text("B1".into())]
        ),
        StepResult::Done => panic!("expected first row"),
    }

    // Install authorizer that masks column `b` to `NULL`
    conn.set_authorizer(Some(Box::new(|action, arg1, arg2, _, _| {
        if action == AuthorizerAction::Read && arg1 == Some("t_exp") && arg2 == Some("b") {
            AuthorizerResult::Ignore
        } else {
            AuthorizerResult::Ok
        }
    })))
    .unwrap();

    // 1. `stmt_running` is already `Running`, so its second row completes with the old plan (`B2` unmasked)!
    match stmt_running.step().unwrap() {
        StepResult::Row(r) => assert_eq!(
            r.into_values(),
            vec![Value::Text("A2".into()), Value::Text("B2".into())]
        ),
        StepResult::Done => panic!("expected second row"),
    }
    assert_eq!(stmt_running.step().unwrap(), StepResult::Done);

    // 2. `stmt_v2` (not yet started) automatically reprepares under the new authorizer and masks `b` to `NULL`!
    match stmt_v2.step().unwrap() {
        StepResult::Row(r) => {
            assert_eq!(r.into_values(), vec![Value::Text("A1".into()), Value::Null])
        }
        StepResult::Done => panic!("expected row from stmt_v2"),
    }
    stmt_v2.reset().unwrap();

    // 3. `stmt_v1` (legacy `prepare_v1`) returns `ErrorCode::Schema` (`SQLITE_SCHEMA`) when expired!
    let v1_err = stmt_v1.step().unwrap_err();
    assert_eq!(v1_err.code, ErrorCode::Schema);
}

// ============================================================================
// Groups 23, 53, 58, 63: `update_hook`, `commit_hook`, `rollback_hook`, `trace_hook`
// ============================================================================

#[test]
fn test_group23_53_58_63_update_hook_truncate_vs_row_by_row_and_attached_dbs() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "ATTACH DATABASE ':memory:' AS aux;
         CREATE TABLE main.t1(id INTEGER PRIMARY KEY, v TEXT);
         CREATE TABLE aux.t2(id INTEGER PRIMARY KEY, v TEXT);
         CREATE TABLE main.wor(k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;",
    )
    .unwrap();

    #[allow(clippy::type_complexity)]
    let hook_events: Arc<Mutex<Vec<(RowMutationOp, String, String, i64)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let hook_clone = Arc::clone(&hook_events);
    conn.update_hook(Some(Box::new(move |action, db_name, tbl_name, rowid| {
        hook_clone
            .lock()
            .unwrap()
            .push((action, db_name.to_owned(), tbl_name.to_owned(), rowid));
    })))
    .unwrap();

    // 1. Insert & update on `main.t1`, `aux.t2`, and `main.wor` (`WITHOUT ROWID` must NOT fire `update_hook`)
    conn.execute(
        "INSERT INTO main.t1 VALUES (10, 'x'), (20, 'y');
         INSERT INTO aux.t2 VALUES (7, 'aux_v');
         INSERT INTO main.wor VALUES ('k1', 'v1');
         UPDATE main.t1 SET v = 'x2' WHERE id = 10;",
    )
    .unwrap();

    {
        let evs = hook_events.lock().unwrap();
        assert_eq!(
            *evs,
            vec![
                (RowMutationOp::Insert, "main".into(), "t1".into(), 10),
                (RowMutationOp::Insert, "main".into(), "t1".into(), 20),
                (RowMutationOp::Insert, "aux".into(), "t2".into(), 7),
                (RowMutationOp::Update, "main".into(), "t1".into(), 10),
            ]
        );
    }

    // 2. Unqualified `DELETE FROM main.t1;` without `WHERE`/triggers/FK/returning uses `OP_Clear`
    // truncate optimization -> `update_hook` does NOT fire!
    hook_events.lock().unwrap().clear();
    conn.execute("DELETE FROM main.t1;").unwrap();
    assert_eq!(conn.changes(), 2);
    assert!(
        hook_events.lock().unwrap().is_empty(),
        "OP_Clear truncate optimization must not fire update_hook"
    );

    // 3. When `set_authorizer` returns `AuthorizerResult::Ignore` for `AuthorizerAction::Delete`,
    // `OP_Clear` truncate optimization is disabled (`R-17228-37124`) so rows are deleted one-by-one
    // and `update_hook` fires for each deleted row!
    conn.execute("INSERT INTO main.t1 VALUES (100, 'a'), (200, 'b');")
        .unwrap();
    hook_events.lock().unwrap().clear();

    conn.set_authorizer(Some(Box::new(|action, _, _, _, _| {
        if action == AuthorizerAction::Delete {
            AuthorizerResult::Ignore
        } else {
            AuthorizerResult::Ok
        }
    })))
    .unwrap();

    conn.execute("DELETE FROM main.t1;").unwrap();
    conn.clear_authorizer().unwrap();
    assert_eq!(
        *hook_events.lock().unwrap(),
        vec![
            (RowMutationOp::Delete, "main".into(), "t1".into(), 100),
            (RowMutationOp::Delete, "main".into(), "t1".into(), 200),
        ]
    );
}

#[test]
fn test_group23_58_63_commit_hook_veto_rollback_hook_and_trace_hook() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE accounts(id INTEGER PRIMARY KEY, bal INTEGER);")
        .unwrap();

    let traces: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let traces_clone = Arc::clone(&traces);
    conn.trace_hook(Some(Box::new(move |sql| {
        traces_clone.lock().unwrap().push(sql.to_owned());
    })))
    .unwrap();

    let commit_calls = Arc::new(AtomicUsize::new(0));
    let rollback_calls = Arc::new(AtomicUsize::new(0));

    // 1. `commit_hook` returning `false` allows commit to succeed
    {
        let cc = Arc::clone(&commit_calls);
        conn.commit_hook(Some(Box::new(move || {
            cc.fetch_add(1, AtomicOrdering::SeqCst);
            false
        })))
        .unwrap();
    }
    {
        let rc = Arc::clone(&rollback_calls);
        conn.rollback_hook(Some(Box::new(move || {
            rc.fetch_add(1, AtomicOrdering::SeqCst);
        })))
        .unwrap();
    }

    conn.execute("INSERT INTO accounts VALUES (1, 500);")
        .unwrap();
    assert_eq!(commit_calls.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(rollback_calls.load(AtomicOrdering::SeqCst), 0);

    // 2. `commit_hook` returning `true` vetoes commit with `SQLITE_CONSTRAINT_COMMITHOOK`,
    // rolls back the transaction, and triggers `rollback_hook`!
    {
        let cc = Arc::clone(&commit_calls);
        conn.commit_hook(Some(Box::new(move || {
            cc.fetch_add(1, AtomicOrdering::SeqCst);
            true
        })))
        .unwrap();
    }

    conn.execute("BEGIN;").unwrap();
    conn.execute("INSERT INTO accounts VALUES (2, 1000);")
        .unwrap();
    let commit_err = conn.execute("COMMIT;").unwrap_err();
    assert_eq!(commit_err.code, ErrorCode::Constraint);
    assert_eq!(
        commit_err.extended_code,
        extended::SQLITE_CONSTRAINT_COMMITHOOK
    );
    assert!(
        conn.is_autocommit(),
        "vetoed COMMIT must roll back transaction and return to autocommit"
    );
    assert_eq!(commit_calls.load(AtomicOrdering::SeqCst), 2);
    assert_eq!(rollback_calls.load(AtomicOrdering::SeqCst), 1);

    // Verify row 2 was rolled back
    let rows = query_vals(&conn, "SELECT id, bal FROM accounts ORDER BY id;");
    assert_eq!(rows, vec![vec![Value::Integer(1), Value::Integer(500)]]);

    // Verify `trace_hook` captured executed statements
    assert!(
        traces
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("INSERT INTO accounts VALUES (1, 500)"))
    );
}

#[test]
fn test_group23_51_52_53_58_63_65_all_hooks_panic_isolation_busy_handler_and_interrupt_handle() {
    let dir = std::env::temp_dir().join(format!("sqliters_p6_busy_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("busy.db");

    let conn1 = Connection::open(&db_path).unwrap();
    let conn2 = Connection::open(&db_path).unwrap();
    conn1
        .execute("CREATE TABLE t_lock(id INTEGER PRIMARY KEY, v INTEGER);")
        .unwrap();

    // Hold a write lock on `conn1`
    conn1
        .execute("BEGIN IMMEDIATE; INSERT INTO t_lock VALUES (1, 10);")
        .unwrap();

    // 1. `busy_handler` on `conn2` receives `0, 1, 2` and then returns `false` -> `ErrorCode::Busy`
    let retry_counts: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let rc_clone = Arc::clone(&retry_counts);
    conn2
        .busy_handler(Some(Box::new(move |count| {
            rc_clone.lock().unwrap().push(count);
            count < 2
        })))
        .unwrap();

    let busy_err = conn2
        .execute("INSERT INTO t_lock VALUES (2, 20);")
        .unwrap_err();
    assert_eq!(busy_err.code, ErrorCode::Busy);
    assert_eq!(*retry_counts.lock().unwrap(), vec![0, 1, 2]);

    // 2. Panicking `busy_handler` is isolated by `catch_unwind`
    conn2
        .busy_handler(Some(Box::new(|_| -> bool {
            panic!("busy handler panicked");
        })))
        .unwrap();
    let busy_panic = conn2
        .execute("INSERT INTO t_lock VALUES (2, 20);")
        .unwrap_err();
    assert!(
        busy_panic.message.contains("busy handler panicked"),
        "unexpected error: {busy_panic}"
    );
    conn2.busy_handler(None).unwrap();
    conn1.execute("COMMIT;").unwrap();

    // 3. Panic isolation on `Authorizer`, `UpdateHook`, `CommitHook`, `RollbackHook`, and `TraceHook`
    conn1
        .set_authorizer(Some(Box::new(|_, _, _, _, _| -> AuthorizerResult {
            panic!("authorizer boom");
        })))
        .unwrap();
    let auth_panic = conn1.query("SELECT * FROM t_lock;", &[]).unwrap_err();
    assert!(auth_panic.message.contains("authorizer boom"));
    conn1.clear_authorizer().unwrap();

    conn1
        .update_hook(Some(Box::new(|_, _, _, _| {
            panic!("update_hook boom");
        })))
        .unwrap();
    let upd_panic = conn1
        .execute("INSERT INTO t_lock VALUES (3, 30);")
        .unwrap_err();
    assert!(upd_panic.message.contains("update_hook boom"));
    conn1.update_hook(None).unwrap();
    // Row 3 was rolled back when `update_hook` panicked
    assert_eq!(
        query_vals(&conn1, "SELECT COUNT(*) FROM t_lock;"),
        vec![vec![Value::Integer(1)]]
    );

    conn1
        .commit_hook(Some(Box::new(|| -> bool {
            panic!("commit_hook boom");
        })))
        .unwrap();
    let cm_panic = conn1
        .execute("INSERT INTO t_lock VALUES (4, 40);")
        .unwrap_err();
    assert!(cm_panic.message.contains("commit_hook boom"));
    conn1.commit_hook(None).unwrap();
    assert_eq!(
        query_vals(&conn1, "SELECT COUNT(*) FROM t_lock;"),
        vec![vec![Value::Integer(1)]]
    );

    conn1
        .rollback_hook(Some(Box::new(|| {
            panic!("rollback_hook boom");
        })))
        .unwrap();
    conn1
        .execute("BEGIN; INSERT INTO t_lock VALUES (5, 50);")
        .unwrap();
    let rb_panic = conn1.execute("ROLLBACK;").unwrap_err();
    assert!(rb_panic.message.contains("rollback_hook boom"));
    conn1.rollback_hook(None).unwrap();
    assert_eq!(
        query_vals(&conn1, "SELECT COUNT(*) FROM t_lock;"),
        vec![vec![Value::Integer(1)]]
    );

    conn1
        .trace_hook(Some(Box::new(|_| {
            panic!("trace_hook boom");
        })))
        .unwrap();
    let tr_panic = conn1.query("SELECT * FROM t_lock;", &[]).unwrap_err();
    assert!(tr_panic.message.contains("trace_hook boom"));
    conn1.trace_hook(None).unwrap();

    // 4. `InterruptHandle` triggered inside a scalar UDF aborts statement with `ErrorCode::Interrupt`
    let ih = conn1.interrupt_handle();
    conn1
        .create_scalar_function("trigger_interrupt", 1, FunctionFlags::NONE, move |args| {
            let v = args[0].value.to_integer_lossy();
            if v >= 2 {
                ih.interrupt();
            }
            Ok(RuntimeValue::from(Value::Integer(v)))
        })
        .unwrap();
    let int_err = conn1
        .execute("INSERT INTO t_lock VALUES (10, trigger_interrupt(1)), (11, trigger_interrupt(2)), (12, trigger_interrupt(3));")
        .unwrap_err();
    assert_eq!(int_err.code, ErrorCode::Interrupt);
    assert_eq!(
        query_vals(&conn1, "SELECT COUNT(*) FROM t_lock;"),
        vec![vec![Value::Integer(1)]]
    );

    // 5. `SQLITE_INSERT`, `SQLITE_SELECT`, and `SQLITE_FUNCTION` returning `AuthorizerResult::Ignore`
    conn1
        .set_authorizer(Some(Box::new(|action, _, arg2, _, _| match action {
            AuthorizerAction::Insert => AuthorizerResult::Ignore,
            AuthorizerAction::Function if arg2 == Some("abs") => AuthorizerResult::Ignore,
            _ => AuthorizerResult::Ok,
        })))
        .unwrap();
    // `INSERT` with `AuthorizerResult::Ignore` is a silent no-op
    conn1
        .execute("INSERT INTO t_lock VALUES (99, 990);")
        .unwrap();
    assert_eq!(
        query_vals(&conn1, "SELECT COUNT(*), abs(-42) FROM t_lock;"),
        vec![vec![Value::Integer(1), Value::Null]]
    );

    conn1
        .set_authorizer(Some(Box::new(|action, _, _, _, _| {
            if action == AuthorizerAction::Select {
                AuthorizerResult::Ignore
            } else {
                AuthorizerResult::Ok
            }
        })))
        .unwrap();
    assert!(query_vals(&conn1, "SELECT * FROM t_lock;").is_empty());
    conn1.clear_authorizer().unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}
