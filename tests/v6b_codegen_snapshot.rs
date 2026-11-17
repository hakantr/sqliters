//! The generator snapshot uses the metadata C sees at actual prepare.
use sqliters::{Connection, StepResult, Value};

#[test]
fn snapshot_sees_fixed_encoding_after_literal_insert() {
    let c=Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES(17)").unwrap();
    c.c_generated_program("PRAGMA encoding='unknown'").unwrap();
    c.prepare("PRAGMA encoding='unknown'").unwrap();
    assert_eq!(c.query("PRAGMA encoding",&[]).unwrap().rows[0].values()[0],Value::Text("UTF-8".into()));
}

#[test]
fn snapshot_connects_fresh_pragma_virtual_tables() {
    for name in ["pragma_temp_store","pragma_encoding","pragma_locking_mode"] {
        let c=Connection::open_in_memory().unwrap();
        c.c_generated_program(&format!("SELECT * FROM {name}")).unwrap();
        assert_eq!(c.query(&format!("SELECT * FROM {name}"),&[]).unwrap().rows.len(),1);
    }
}

#[test]
fn cleared_schema_is_loaded_before_snapshot() {
    let c=Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE t(a); CREATE TEMP TABLE q(a)").unwrap();
    c.prepare("PRAGMA temp_store=FILE").unwrap();
    let sql="SELECT * FROM pragma_temp_store";
    let before=c.c_generated_program(sql).unwrap();
    assert_eq!(c.query(sql,&[]).unwrap().rows[0].values()[0],Value::Integer(1));
    let after=c.c_generated_program(sql).unwrap();
    assert_eq!(sqliters_vdbe::progdump::dump(&before,0),sqliters_vdbe::progdump::dump(&after,0));
}

#[test]
fn snapshot_discards_top_level_prepare_effects() {
    let c=Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TEMP TABLE q(a); INSERT INTO q VALUES(17)").unwrap();
    c.c_generated_program("PRAGMA temp_store=MEMORY").unwrap();
    assert_eq!(c.query("PRAGMA temp_store",&[]).unwrap().rows[0].values()[0],Value::Integer(0));
    assert_eq!(c.query("SELECT * FROM q",&[]).unwrap().rows[0].values()[0],Value::Integer(17));
    let empty=Connection::open_in_memory().unwrap();
    let mut old=empty.prepare("PRAGMA busy_timeout=17").unwrap();
    empty.prepare("PRAGMA busy_timeout=31").unwrap();
    empty.c_generated_program("PRAGMA encoding='UTF-16be'").unwrap();
    assert_eq!(empty.query("PRAGMA encoding",&[]).unwrap().rows[0].values()[0],Value::Text("UTF-8".into()));
    assert!(matches!(old.step().unwrap(),StepResult::Row(_)));
    assert!(matches!(old.step().unwrap(),StepResult::Done));
    assert_eq!(empty.query("PRAGMA busy_timeout",&[]).unwrap().rows[0].values()[0],Value::Integer(31));
}

#[test]
fn create_name_collisions_keep_the_input_token_and_byte_offset() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE \"Tç\"(a); CREATE VIEW \"Vç\" AS SELECT a FROM \"Tç\";")
        .unwrap();

    for (sql, token, message) in [
        ("/* ş */ CREATE TABLE main.[Tç](a)", "[Tç]", "table [Tç] already exists"),
        ("CREATE TABLE \"Vç\" AS SELECT 1", "\"Vç\"", "view \"Vç\" already exists"),
        ("CREATE TABLE other.\"Tç\"(a)", "other", "unknown database other"),
    ] {
        let err = db.prepare(sql).err().expect("prepare should fail");
        assert_eq!(err.message, message);
        assert_eq!(err.c_offset(), sql.find(token).unwrap() as i64);
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS main.[Tç](a);").unwrap();
}

#[test]
fn generated_column_constraints_fail_at_prepare_in_parser_order() {
    let db = Connection::open_in_memory().unwrap();
    for (sql, message) in [
        (
            "CREATE TABLE t(a, g AS (missing) PRIMARY KEY, a)",
            "generated columns cannot be part of the PRIMARY KEY",
        ),
        (
            "CREATE TABLE t(a, g INTEGER PRIMARY KEY AS (a + 1) STORED)",
            "generated columns cannot be part of the PRIMARY KEY",
        ),
        (
            "CREATE TABLE t(a, g AS (a) DEFAULT 0 PRIMARY KEY)",
            "cannot use DEFAULT on a generated column",
        ),
        (
            "CREATE TABLE t(a, g AS (a) STORED, PRIMARY KEY (a, g))",
            "generated columns cannot be part of the PRIMARY KEY",
        ),
    ] {
        let err = db.prepare(sql).err().expect("prepare should fail");
        assert_eq!(err.message, message);
        assert_eq!(err.c_offset(), -1);
    }
    db.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES (9);")
        .unwrap();
    let mut noop = db
        .prepare("CREATE TABLE IF NOT EXISTS t(a, g AS (a) PRIMARY KEY)")
        .unwrap();
    assert!(matches!(noop.step().unwrap(), StepResult::Done));
    assert_eq!(
        db.query("SELECT a FROM t", &[]).unwrap().rows[0].values(),
        &[Value::Integer(9)]
    );
}

#[test]
fn first_schema_page_does_not_expire_a_v1_create() {
    let db = Connection::open_in_memory().unwrap();
    let mut create = db.prepare_v1("CREATE TABLE fresh(a)").unwrap();
    assert!(matches!(create.step().unwrap(), StepResult::Done));

    let mut stale = db.prepare_v1("SELECT name FROM sqlite_schema").unwrap();
    db.execute_batch("CREATE TABLE later(a)").unwrap();
    assert!(stale.step().is_err());
    assert_eq!(
        db.query("SELECT name FROM sqlite_schema ORDER BY name", &[])
            .unwrap()
            .rows
            .len(),
        2
    );
}

#[test]
fn locking_mode_tvf_rechecks_the_hidden_schema_constraint() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("ATTACH ':memory:' AS aux").unwrap();

    let implicit = db
        .query("SELECT schema FROM pragma_locking_mode('aux')", &[])
        .unwrap();
    assert_eq!(implicit.rows[0].values(), &[Value::Null]);
    let contradictory = db
        .query(
            "SELECT locking_mode FROM pragma_locking_mode('aux') WHERE schema = 'main'",
            &[],
        )
        .unwrap();
    assert!(contradictory.rows.is_empty());

    let err = db
        .query("SELECT * FROM pragma_locking_mode('missing')", &[])
        .err()
        .expect("the inner PRAGMA should reject the schema");
    assert_eq!(err.message, "unknown database 'missing'");
}

#[test]
fn module_list_reflects_the_registration_of_used_sources() {
    let db = Connection::open_in_memory().unwrap();
    let names = || {
        db.query("PRAGMA module_list", &[])
            .unwrap()
            .rows
            .iter()
            .map(|row| row.values()[0].to_text_lossy())
            .collect::<Vec<_>>()
    };
    let initial = names();
    assert_eq!(initial, ["dbstat", "fts5", "fts5vocab", "rtree", "rtree_i32"]);
    assert!(db.prepare("SELECT * FROM generate_series(1, 2)").is_err());
    db.prepare("WITH unused AS (SELECT * FROM json_each('[1]')) SELECT 1")
        .unwrap();
    assert_eq!(names(), initial);

    let mut snapshot = db.prepare("PRAGMA module_list").unwrap();
    db.prepare("WITH used AS (SELECT * FROM json_each('[1]')) SELECT * FROM used")
        .unwrap();
    assert!(names().contains(&"json_each".to_owned()));
    let mut prepared_names = Vec::new();
    while let StepResult::Row(row) = snapshot.step().unwrap() {
        prepared_names.push(row.values()[0].to_text_lossy());
    }
    assert_eq!(prepared_names, initial);
}
