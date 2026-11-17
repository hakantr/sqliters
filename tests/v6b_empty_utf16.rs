//! C's eliminated table subquery needs no text operation or Btree access.
use sqliters::{Connection, ErrorCode, StepResult, Value};

fn setup(encoding: &str) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(&format!("PRAGMA encoding='{encoding}'; CREATE TABLE t(a); PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql='broken' WHERE name='t';")).unwrap();
    c.prepare("PRAGMA writable_schema=RESET").unwrap();
    c
}

#[test]
fn eliminated_subquery_completes_without_reading_malformed_utf16_schema() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let c = setup(enc);
        for sql in [
            "SELECT 17 WHERE 0 AND EXISTS(SELECT * FROM t)",
            "SELECT 'İ' AS Örnek WHERE 0 AND EXISTS(SELECT * FROM t)",
            "SELECT randomblob(1), length('İ') WHERE 0 AND EXISTS(SELECT * FROM t)",
        ] {
            let mut s = c.prepare(sql).unwrap_or_else(|e| panic!("{enc}: {sql}: {e:?}"));
            assert!(
                matches!(s.step().unwrap(), StepResult::Done),
                "{enc}: {sql}"
            );
            s.reset().unwrap();
            assert!(
                matches!(s.step().unwrap(), StepResult::Done),
                "{enc}: {sql}"
            );
        }
        assert_eq!(
            c.prepare("SELECT * FROM t").err().unwrap().code,
            ErrorCode::Corrupt
        );
    }
}

#[test]
fn discarded_parameters_survive_reset_and_advisory_reprepare() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let c = setup(enc);
        let mut s = c
            .prepare("SELECT ?1 WHERE 0 AND (?2 IS NULL OR EXISTS(SELECT * FROM t))")
            .unwrap();
        s.bind(1, Value::Text("İ".into())).unwrap();
        s.bind(2, Value::Integer(23)).unwrap();
        assert!(matches!(s.step().unwrap(), StepResult::Done));
        let steps = s.machine_stmt_status(4, false);
        assert_eq!(s.machine_stmt_status(6, false), 1);
        s.reset().unwrap();
        c.execute_batch("PRAGMA automatic_index=OFF").unwrap();
        assert!(matches!(s.step().unwrap(), StepResult::Done));
        assert_eq!(s.machine_stmt_status(4, true), 2 * steps);
        assert_eq!(s.machine_stmt_status(6, false), 2);
        assert_eq!(s.machine_stmt_status(5, false), 1);
        s.reset().unwrap();
        c.execute_batch("PRAGMA automatic_index=ON").unwrap();
        assert!(matches!(s.step().unwrap(), StepResult::Done));
        assert_eq!(s.machine_stmt_status(4, false), steps);
        assert_eq!(s.machine_stmt_status(6, false), 3);
        assert_eq!(s.machine_stmt_status(5, false), 2);
    }
}

#[test]
fn live_values_keep_the_connection_encoding_after_reset() {
    for (enc, bytes) in [
        ("UTF-8", &[0xc4, 0xb0][..]),
        ("UTF-16le", &[0x30, 0x01][..]),
        ("UTF-16be", &[0x01, 0x30][..]),
    ] {
        let c = setup(enc);
        let row = c
            .query("SELECT CAST('İ' AS BLOB)", &[])
            .unwrap()
            .rows
            .remove(0);
        assert_eq!(row.values()[0], Value::Blob(bytes.to_vec().into()));
    }
}
