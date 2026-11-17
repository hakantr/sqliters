use sqliters::{Connection, Statement, StepResult};

fn strings(stmt: &mut Statement) -> Vec<String> {
    let mut out = Vec::new();
    while let StepResult::Row(row) = stmt.step().unwrap() {
        out.push(row.get_str(0).unwrap().to_owned());
    }
    out
}

#[test]
fn compile_options_are_reusable_and_share_the_scalar_diagnostic_profile() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}';"))
            .unwrap();
        let mut list = db.prepare("PRAGMA compile_options").unwrap();
        let first = strings(&mut list);
        assert_eq!(first.len(), 42);
        list.reset().unwrap();
        assert_eq!(strings(&mut list), first);
        assert_eq!(list.machine_stmt_status(5, false), 0);
        let mut get = db.prepare("SELECT sqlite_compileoption_get(4294967296), sqlite_compileoption_used('sqlite_threadsafe'), sqlite_compileoption_used('COMPILER=gcc'), sqlite_compileoption_used('THREADSAFE$'), sqlite_compileoption_used(NULL)").unwrap();
        let StepResult::Row(row) = get.step().unwrap() else {
            panic!("one diagnostic row")
        };
        assert_eq!(row.get_str(0), Some(first[0].as_str()));
        assert_eq!(row.get_i64(1), Some(1));
        assert_eq!(row.get_i64(2), Some(1));
        assert_eq!(row.get_i64(3), Some(0));
        assert!(row.get(4).unwrap().is_null());
        drop(get);
        for sql in [
            "SELECT sqlite_compileoption_get()",
            "SELECT sqlite_compileoption_used(1,2)",
        ] {
            let err = match db.prepare(sql) {
                Ok(_) => panic!("C rejects the argument count"),
                Err(e) => e,
            };
            assert_eq!(err.c_offset(), 7);
        }
    }
}
