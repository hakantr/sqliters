//! Locking regression tests for the online backup entry (step 5i).
//!
//! `sqlite3_backup_init`, `sqlite3_backup_step` and `sqlite3_backup_finish` take the source and the
//! destination connection mutex and *wait* for them (`sqlite/src/backup.c:164-165, 334-337,
//! 606-609`); they never return `SQLITE_BUSY` because another thread happens to be inside one of
//! the two connections. C takes them in source-then-destination order, so two backups running in
//! opposite directions between the same two handles deadlock there. The port takes the two gates
//! in ascending `conn_id` order and waits, which gives C's results without C's deadlock.
//!
//! Before the fix `ConnectionShared::enter_dual` used a try-lock on both gates: a momentary use of
//! either connection by another thread made `Backup::init` / `Backup::finish` fail with
//! `SQLITE_BUSY` ("database connection is busy"), and a refused `Backup::finish` / `Blob::close`
//! leaked the handle's registrations for good.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sqliters_types::{ErrorCode, Value};

use crate::backup::{Backup, BackupStepResult};
use crate::{Connection, dual_entry_test_hook};

/// Upper bound for every wait in this file: a regression must show up as a failed assertion, not
/// as a test run that never ends.
const LIMIT: Duration = Duration::from_secs(20);

fn count(conn: &Connection, sql: &str) -> i64 {
    match conn.query(sql, &[]).unwrap().rows[0].values()[0] {
        Value::Integer(n) => n,
        ref other => panic!("expected an integer, got {other:?}"),
    }
}

fn source_with_rows(table: &str, rows: usize) -> Arc<Connection> {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(&format!(
        "CREATE TABLE {table}(id INTEGER PRIMARY KEY, v BLOB);"
    ))
    .unwrap();
    for i in 1..=rows {
        conn.execute(&format!("INSERT INTO {table} VALUES ({i}, zeroblob(800));"))
            .unwrap();
    }
    Arc::new(conn)
}

/// Another thread sitting inside `conn`'s execution gate (as it would be while running a
/// statement) until [`GateHolder::release`].
struct GateHolder {
    release: mpsc::Sender<()>,
    thread: thread::JoinHandle<()>,
}

impl GateHolder {
    fn hold(conn: &Arc<Connection>) -> Self {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let conn = Arc::clone(conn);
        let thread = thread::spawn(move || {
            conn.execute_and_invoke_hook_for_testing("", |_| {
                entered_tx.send(()).unwrap();
                let _ = release_rx.recv_timeout(LIMIT);
            })
            .unwrap();
        });
        entered_rx.recv_timeout(LIMIT).unwrap();
        Self {
            release: release_tx,
            thread,
        }
    }

    fn release(self) {
        let _ = self.release.send(());
        self.thread.join().unwrap();
    }
}

/// Runs `op` on a new thread while another thread is inside `held`'s gate. The gate is released
/// only once `op` is seen parked on it (or has already returned). Returns `op`'s result and
/// whether it was seen waiting.
fn run_while_gate_held<T, F>(held: &Arc<Connection>, op: F) -> (T, bool)
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let holder = GateHolder::hold(held);
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = done_tx.send(op());
    });

    let deadline = Instant::now() + LIMIT;
    let mut early = None;
    let mut waited = false;
    while Instant::now() < deadline {
        if held.shared().gate_waiters.load(Ordering::SeqCst) > 0 {
            waited = true;
            break;
        }
        if let Ok(v) = done_rx.try_recv() {
            early = Some(v);
            break;
        }
        thread::sleep(Duration::from_micros(200));
    }
    holder.release();
    let out = match early {
        Some(v) => v,
        None => done_rx
            .recv_timeout(LIMIT)
            .expect("the backup call never returned after the gate was released"),
    };
    worker.join().unwrap();
    (out, waited)
}

/// `sqlite3_backup_init` while another thread is inside the source (lower `conn_id`: the first
/// gate) or the destination (the second gate, i.e. the wait happens while the first is owned).
#[test]
fn backup_init_waits_for_a_connection_in_use_by_another_thread() {
    for hold_source in [true, false] {
        let src = source_with_rows("t", 15);
        let dst = Arc::new(Connection::open_in_memory().unwrap());
        let held = if hold_source { &src } else { &dst };

        let (s, d) = (Arc::clone(&src), Arc::clone(&dst));
        let (res, waited) = run_while_gate_held(held, move || Backup::init(&s, "main", &d, "main"));
        assert!(
            waited,
            "hold_source={hold_source}: init returned {:?} instead of waiting",
            res.as_ref().map(|_| ()).map_err(|e| e.code)
        );
        let mut bkp = res.expect("init must succeed once the other thread has left");
        assert_eq!(bkp.step(-1).unwrap(), BackupStepResult::Done);
        bkp.finish().unwrap();
        assert_eq!(count(&dst, "SELECT count(*) FROM t;"), 15);
    }
}

/// `sqlite3_backup_step` and `sqlite3_backup_finish` wait the same way, and a finish that had to
/// wait releases everything it holds.
#[test]
fn backup_step_and_finish_wait_for_a_connection_in_use_by_another_thread() {
    for hold_source in [true, false] {
        let src = source_with_rows("t", 15);
        let dst = Arc::new(Connection::open_in_memory().unwrap());
        let held = if hold_source { &src } else { &dst };

        let bkp = Backup::init(&src, "main", &dst, "main").unwrap();
        let ((bkp, step_res), waited) = run_while_gate_held(held, move || {
            let mut bkp = bkp;
            let res = bkp.step(1);
            (bkp, res)
        });
        assert!(waited, "hold_source={hold_source}: step did not wait");
        assert_eq!(step_res.unwrap(), BackupStepResult::More);
        assert!(bkp.remaining() > 0);

        // The destination now carries the backup's write transaction; finish has to roll it back.
        let (fin_res, waited) = run_while_gate_held(held, move || bkp.finish());
        assert!(waited, "hold_source={hold_source}: finish did not wait");
        fin_res.unwrap();

        // Nothing is left registered: the destination accepts a new backup and both handles close.
        let mut again = Backup::init(&src, "main", &dst, "main").unwrap();
        assert_eq!(again.step(-1).unwrap(), BackupStepResult::Done);
        again.finish().unwrap();
        assert_eq!(count(&dst, "SELECT count(*) FROM t;"), 15);
        let src = Arc::try_unwrap(src).expect("source still shared");
        let dst = Arc::try_unwrap(dst).expect("destination still shared");
        assert!(src.try_close().is_ok());
        assert!(dst.try_close().is_ok());
    }
}

/// The root cause of the flaky `backup_concurrent_bidirectional_steps_never_deadlock`: backup A
/// copies c1 -> c2 while backup B copies c2 -> c1. A is paused between its two gate acquisitions
/// (it owns c1, the lower `conn_id`); B then enters. B must queue behind A on c1 (one global
/// order) and may not touch c2: taking its own source first, as C does, is the ABBA deadlock, and
/// refusing with `SQLITE_BUSY`, as the try-lock did, is a result C never returns here.
#[test]
fn opposite_direction_backups_take_both_connections_in_one_order() {
    let c1 = source_with_rows("c1_main", 15);
    let c2 = source_with_rows("c2_main", 15);
    c1.execute("ATTACH ':memory:' AS dst_on_1;").unwrap();
    c2.execute("ATTACH ':memory:' AS dst_on_2;").unwrap();
    let (id1, id2) = (c1.shared().conn_id, c2.shared().conn_id);
    assert!(id1 < id2);

    let mut bkp_a = Backup::init(&c1, "main", &c2, "dst_on_2").unwrap();
    let mut bkp_b = Backup::init(&c2, "main", &c1, "dst_on_1").unwrap();

    let (paused_tx, paused_rx) = mpsc::channel::<()>();
    let paused_tx = Mutex::new(paused_tx);
    let fired = Arc::new(AtomicBool::new(false));
    // (B was seen parked on c1, c2's gate was free at that moment)
    let seen = Arc::new(Mutex::new((false, false)));
    {
        let (c1, c2) = (Arc::clone(&c1), Arc::clone(&c2));
        let (fired, seen) = (Arc::clone(&fired), Arc::clone(&seen));
        dual_entry_test_hook::set(
            id1,
            id2,
            Arc::new(move || {
                if fired.swap(true, Ordering::SeqCst) {
                    return; // only A's first entry is paused
                }
                paused_tx.lock().unwrap().send(()).unwrap();
                let deadline = Instant::now() + LIMIT;
                while Instant::now() < deadline {
                    if c1.shared().gate_waiters.load(Ordering::SeqCst) > 0 {
                        let c2_free = !c2.shared().gate_busy.load(Ordering::SeqCst);
                        *seen.lock().unwrap() = (true, c2_free);
                        return;
                    }
                    thread::sleep(Duration::from_micros(200));
                }
            }),
        );
    }

    let ta = thread::spawn(move || {
        let res = bkp_a.step(1);
        (bkp_a, res)
    });
    paused_rx
        .recv_timeout(LIMIT)
        .expect("A never reached the pause point");
    let tb = thread::spawn(move || {
        let res = bkp_b.step(1);
        (bkp_b, res)
    });
    let (mut bkp_a, res_a) = ta.join().unwrap();
    let (mut bkp_b, res_b) = tb.join().unwrap();
    dual_entry_test_hook::clear(id1, id2);

    let (b_waited_on_c1, c2_was_free) = *seen.lock().unwrap();
    assert!(
        b_waited_on_c1,
        "B did not queue on c1: step returned {:?}",
        res_b.as_ref().map_err(|e| e.code)
    );
    assert!(c2_was_free, "B took c2 before c1 (lock-order inversion)");
    assert_eq!(res_a.unwrap(), BackupStepResult::More);
    assert_eq!(res_b.unwrap(), BackupStepResult::More);

    assert_eq!(bkp_a.step(-1).unwrap(), BackupStepResult::Done);
    assert_eq!(bkp_b.step(-1).unwrap(), BackupStepResult::Done);
    bkp_a.finish().unwrap();
    bkp_b.finish().unwrap();
    assert_eq!(count(&c2, "SELECT count(*) FROM dst_on_2.c1_main;"), 15);
    assert_eq!(count(&c1, "SELECT count(*) FROM dst_on_1.c2_main;"), 15);
}

/// A thread that is already inside some connection (here: in a callback of a third connection)
/// must not wait for another connection's gate: it keeps the immediate `SQLITE_BUSY`. The refused
/// `finish` still releases everything through the deferred-drop queues.
#[test]
fn backup_calls_from_inside_a_callback_never_wait_and_never_leak() {
    let src = source_with_rows("t", 15);
    let dst = Arc::new(Connection::open_in_memory().unwrap());
    let third = Connection::open_in_memory().unwrap();

    let mut bkp = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(bkp.step(1).unwrap(), BackupStepResult::More);

    let holder = GateHolder::hold(&dst);
    let started = Instant::now();
    let (step_err, fin_err) = third
        .execute_and_invoke_hook_for_testing("", move |_| {
            let step_err = bkp.step(1).unwrap_err();
            let fin_err = bkp.finish().unwrap_err();
            (step_err, fin_err)
        })
        .unwrap();
    assert!(started.elapsed() < LIMIT / 2, "a nested backup call waited");
    assert_eq!(step_err.code, ErrorCode::Busy);
    assert_eq!(fin_err.code, ErrorCode::Busy);
    assert_eq!(dst.shared().gate_waiters.load(Ordering::SeqCst), 0);
    holder.release();

    let mut again = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(again.step(-1).unwrap(), BackupStepResult::Done);
    again.finish().unwrap();
    let src = Arc::try_unwrap(src).expect("source still shared");
    let dst = Arc::try_unwrap(dst).expect("destination still shared");
    assert!(src.try_close().is_ok());
    assert!(dst.try_close().is_ok());
    assert!(third.try_close().is_ok());
}

/// `Backup::finish` called from a callback of its own source connection is refused with
/// `SQLITE_MISUSE` (re-entry). It consumed the handle, so the cleanup must still happen: the
/// destination's write transaction is rolled back and both connections can be closed.
#[test]
fn backup_finish_refused_as_reentrant_does_not_leak() {
    let src = source_with_rows("t", 15);
    let dst = Connection::open_in_memory().unwrap();

    let mut bkp = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(bkp.step(1).unwrap(), BackupStepResult::More);

    let err = src
        .execute_and_invoke_hook_for_testing("", move |_| bkp.finish().unwrap_err())
        .unwrap();
    assert_eq!(err.code, ErrorCode::Misuse);

    let mut again = Backup::init(&src, "main", &dst, "main")
        .expect("the destination must not stay 'in use' after a refused finish");
    assert_eq!(again.step(-1).unwrap(), BackupStepResult::Done);
    again.finish().unwrap();
    assert_eq!(count(&dst, "SELECT count(*) FROM t;"), 15);
    let src = Arc::try_unwrap(src).expect("source still shared");
    assert!(src.try_close().is_ok());
    assert!(dst.try_close().is_ok());
}

/// Same defect in `Blob::close`: a refused close (re-entry, or another thread inside the
/// connection) dropped the handle id before entering and so never released the handle.
#[test]
fn blob_close_refused_does_not_leak() {
    // Refused as re-entrant.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b BLOB); INSERT INTO t VALUES (1, X'CAFE');",
    )
    .unwrap();
    let blob = conn.blob_open("main", "t", "b", 1, false).unwrap();
    let err = conn
        .execute_and_invoke_hook_for_testing("", move |_| blob.close().unwrap_err())
        .unwrap();
    assert_eq!(err.code, ErrorCode::Misuse);
    assert!(conn.try_close().is_ok());

    // Refused because another thread is inside the connection.
    let conn = Arc::new(Connection::open_in_memory().unwrap());
    conn.execute(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b BLOB); INSERT INTO t VALUES (1, X'CAFE');",
    )
    .unwrap();
    let blob = conn.blob_open("main", "t", "b", 1, true).unwrap();
    let holder = GateHolder::hold(&conn);
    assert_eq!(blob.close().unwrap_err().code, ErrorCode::Busy);
    holder.release();
    conn.execute("DROP TABLE t;").unwrap();
    let conn = Arc::try_unwrap(conn).expect("connection still shared");
    assert!(conn.try_close().is_ok());
}

/// Stress version of the bidirectional scenario: every call is unwrapped, because with ordered
/// waiting none of them may fail or block for good. (The same loop written against the C API
/// deadlocks within a few rounds.)
#[test]
fn bidirectional_backups_stress() {
    const ROUNDS: usize = 150;
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let c1 = source_with_rows("c1_main", 15);
        let c2 = source_with_rows("c2_main", 15);
        c1.execute("ATTACH ':memory:' AS dst_on_1;").unwrap();
        c2.execute("ATTACH ':memory:' AS dst_on_2;").unwrap();

        let worker = |src: Arc<Connection>, dst: Arc<Connection>, dst_db: &'static str| {
            thread::spawn(move || {
                for round in 0..ROUNDS {
                    let mut bkp = Backup::init(&src, "main", &dst, dst_db).unwrap();
                    let n = if round % 3 == 2 { 5 } else { 1 };
                    while bkp.step(n).unwrap() == BackupStepResult::More {
                        if round % 2 == 0 {
                            thread::yield_now();
                        }
                    }
                    assert_eq!(bkp.remaining(), 0);
                    bkp.finish().unwrap();
                }
            })
        };
        let t1 = worker(Arc::clone(&c1), Arc::clone(&c2), "dst_on_2");
        let t2 = worker(Arc::clone(&c2), Arc::clone(&c1), "dst_on_1");
        let ok = t1.join().is_ok() & t2.join().is_ok();
        let rows = (
            count(&c2, "SELECT count(*) FROM dst_on_2.c1_main;"),
            count(&c1, "SELECT count(*) FROM dst_on_1.c2_main;"),
        );
        let _ = done_tx.send((ok, rows));
    });
    let (ok, rows) = match done_rx.recv_timeout(Duration::from_secs(120)) {
        Ok(v) => v,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("bidirectional backups did not finish: deadlock or lost wake-up")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the stress driver panicked"),
    };
    assert!(ok, "a backup thread panicked");
    assert_eq!(rows, (15, 15));
}
