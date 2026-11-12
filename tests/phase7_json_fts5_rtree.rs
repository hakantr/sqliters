#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters::{Connection, ErrorCode, Value};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p7b_{prefix}_{pid}_{id}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self { path: dir }
    }

    fn db_path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sqlite_ref_bin() -> PathBuf {
    let bin = workspace_root().join("target/sqlite-ref/sqlite3");
    assert!(
        bin.exists(),
        "C SQLite 3.54.1 reference binary missing at {}",
        bin.display()
    );
    bin
}

fn sqlite_ext_bin() -> &'static Path {
    static EXT_BIN: OnceLock<PathBuf> = OnceLock::new();
    EXT_BIN.get_or_init(|| {
        let root = workspace_root();
        let bin = root.join("target/sqlite-ref/sqlite3_ext");
        if bin.exists() {
            return bin;
        }
        let amalg_dir = root.join("target/sqlite-ref/amalg");
        let shell_c = amalg_dir.join("shell.c");
        let sqlite3_c = amalg_dir.join("sqlite3.c");
        assert!(
            shell_c.exists() && sqlite3_c.exists(),
            "amalgamation sources missing under {}",
            amalg_dir.display()
        );
        let status = Command::new("cc")
            .args([
                "-O2",
                "-DSQLITE_ENABLE_FTS5",
                "-DSQLITE_ENABLE_RTREE",
                "-DSQLITE_ENABLE_DBSTAT_VTAB",
                "-DSQLITE_ENABLE_MATH_FUNCTIONS",
                "-DSQLITE_THREADSAFE=1",
            ])
            .arg(&shell_c)
            .arg(&sqlite3_c)
            .arg("-o")
            .arg(&bin)
            .args(["-lm", "-lpthread", "-ldl"])
            .status()
            .expect("invoke cc to compile sqlite3_ext");
        assert!(status.success(), "compiling sqlite3_ext failed");
        bin
    })
}

fn run_csqlite_lines(bin: &Path, db: &str, sql: &str) -> Vec<String> {
    let out = Command::new(bin)
        .args([
            "-batch",
            "-noheader",
            "-list",
            "-separator",
            "|",
            "-newline",
            "\x1e",
            db,
            sql,
        ])
        .output()
        .expect("execute C sqlite3 binary");
    assert!(
        out.status.success(),
        "C sqlite3 failed on SQL:\n{sql}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let trimmed = stdout.strip_suffix('\x1e').unwrap_or(&stdout);
    if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed
            .split('\x1e')
            .map(|l| l.trim_end_matches('\r').to_owned())
            .collect()
    }
}

fn format_value_for_list(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => sqliters::types::format_sqlite_real(*f),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn run_sqliters_lines(conn: &Connection, setup_sql: &str, query_sql: &str) -> Vec<String> {
    if !setup_sql.trim().is_empty() {
        conn.execute(setup_sql).expect("sqliters setup SQL");
    }
    let res = conn.query(query_sql, &[]).expect("sqliters query SQL");
    res.rows
        .iter()
        .map(|r| {
            r.values()
                .iter()
                .map(format_value_for_list)
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

fn assert_differential_in_memory(bin: &Path, setup_sql: &str, query_sql: &str) {
    let conn = Connection::open_in_memory().expect("open in-memory sqliters");
    let rust_lines = run_sqliters_lines(&conn, setup_sql, query_sql);
    let combined = if setup_sql.trim().is_empty() {
        query_sql.to_owned()
    } else {
        format!("{setup_sql};\n{query_sql};")
    };
    let c_lines = run_csqlite_lines(bin, ":memory:", &combined);
    assert_eq!(
        rust_lines, c_lines,
        "differential mismatch for query:\n{query_sql}"
    );
}

// ============================================================================
// 1. JSON & JSONB C SQLite 3.54.1 Cross-Engine Binary & Functional Parity
// ============================================================================

#[test]
fn test_jsonb_wire_format_cross_engine_sqliters_to_csqlite() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let dir = TempDirGuard::new("jsonb_r2c");
    let db_file = dir.db_path("r2c.db");
    let db_str = db_file.to_str().unwrap();

    let medium_str = "merhaba_kirtasiye_".repeat(20); // ~360 bytes -> 2-byte BE size header (code 13)
    let large_str = "uzun_icerik_verisi_".repeat(3600); // ~68,400 bytes -> 4-byte BE size header (code 14)

    {
        let conn = Connection::open(&db_file).unwrap();
        conn.execute(
            "CREATE TABLE belgeler(id INTEGER PRIMARY KEY, ad TEXT, veri BLOB);
             INSERT INTO belgeler VALUES(
                 1,
                 'temel',
                 jsonb('{\"etiket\":\"kirtasiye\",\"fiyat\":50,\"aktif\":true,\"iptal\":false,\"ek\":null,\"oran\":12.5,\"liste\":[1,-42,true,null,\"kalem\\nsatir\"]}')
             );",
        )
        .unwrap();

        let med_json = format!(r#"{{"tip":"orta","metin":"{medium_str}","sayilar":[10,20,30]}}"#);
        let big_json = format!(r#"{{"tip":"buyuk","metin":"{large_str}","son":99}}"#);
        conn.query(
            "INSERT INTO belgeler VALUES(2, 'orta', jsonb(?1))",
            &[Value::Text(med_json)],
        )
        .unwrap();
        conn.query(
            "INSERT INTO belgeler VALUES(3, 'buyuk', jsonb(?1))",
            &[Value::Text(big_json)],
        )
        .unwrap();
    }

    let check_sql = "
        PRAGMA integrity_check;
        SELECT id, typeof(veri), json_valid(veri, 4), json_valid(veri, 1) FROM belgeler ORDER BY id;
        SELECT json(veri), veri ->> '$.etiket', veri -> '$.liste[4]', json_type(veri, '$.oran') FROM belgeler WHERE id = 1;
        SELECT veri ->> '$.tip', length(veri ->> '$.metin'), veri ->> '$.sayilar[2]' FROM belgeler WHERE id = 2;
        SELECT veri ->> '$.tip', length(veri ->> '$.metin'), veri ->> '$.son' FROM belgeler WHERE id = 3;
        SELECT fullkey, type, coalesce(cast(atom as text), 'NULL') FROM json_tree((SELECT veri FROM belgeler WHERE id = 1)) ORDER BY id;
    ";

    let c_out = run_csqlite_lines(&sqlite_ref_bin(), db_str, check_sql);
    assert_eq!(c_out[0], "ok");
    assert_eq!(c_out[1], "1|blob|1|0");
    assert_eq!(c_out[2], "2|blob|1|0");
    assert_eq!(c_out[3], "3|blob|1|0");

    let conn = Connection::open(&db_file).unwrap();
    let mut rust_out = Vec::new();
    for stmt in check_sql.split(';') {
        let s = stmt.trim();
        if !s.is_empty() {
            rust_out.extend(run_sqliters_lines(&conn, "", s));
        }
    }
    assert_eq!(rust_out, c_out);
}

#[test]
fn test_jsonb_wire_format_cross_engine_csqlite_to_sqliters() {
    let dir = TempDirGuard::new("jsonb_c2r");
    let db_file = dir.db_path("c2r.db");
    let db_str = db_file.to_str().unwrap();

    let init_sql = r#"
        CREATE TABLE c_jsonb(id INTEGER PRIMARY KEY, jb BLOB);
        INSERT INTO c_jsonb VALUES
            (1, jsonb('{"etiket":"defter","fiyat":75,"kdv":0.18,"etiketler":["okul","ofis"],"bos":null,"durum":true}')),
            (2, jsonb_set(jsonb('{"a":[1,2,3]}'), '$.a[#]', 4, '$.ek', json('{"ic":true}'))),
            (3, jsonb_patch(jsonb('{"x":1,"y":2,"sil":3}'), '{"y":20,"sil":null,"z":[false,null,"u\u00e7"]}'));
        INSERT INTO c_jsonb VALUES
            (4, jsonb('{unquoted: ''single'', hex: 0x2a, trail: [1, 2,], /* comment */ neg_val: -0.5, specials: [Infinity, -Infinity, NaN]}'));
    "#;
    let _ = run_csqlite_lines(&sqlite_ref_bin(), db_str, init_sql);

    let verify_sql = "
        SELECT id, json_valid(jb, 4), json_valid(jb, 8), json(jb) FROM c_jsonb ORDER BY id;
        SELECT id, jb -> '$.etiket', jb ->> '$.etiket', jb ->> '$.etiketler[1]' FROM c_jsonb WHERE id = 1;
        SELECT id, json(jb -> '$.a'), jb ->> '$.ek.ic' FROM c_jsonb WHERE id = 2;
        SELECT id, jb ->> '$.y', json_type(jb, '$.sil'), jb ->> '$.z[2]' FROM c_jsonb WHERE id = 3;
        SELECT id, jb ->> '$.unquoted', jb ->> '$.hex', json(jb -> '$.trail') FROM c_jsonb WHERE id = 4;
        SELECT c.id, e.key, e.type, coalesce(cast(e.value as text), 'NULL')
          FROM c_jsonb c, json_each(c.jb) e
         ORDER BY c.id, e.key;
    ";

    let c_lines = run_csqlite_lines(&sqlite_ref_bin(), db_str, verify_sql);
    let conn = Connection::open(&db_file).unwrap();
    let ic = conn.query("PRAGMA integrity_check", &[]).unwrap();
    assert_eq!(ic.rows[0].get_str(0), Some("ok"));

    let mut rust_lines = Vec::new();
    for stmt in verify_sql.split(';') {
        let s = stmt.trim();
        if !s.is_empty() {
            rust_lines.extend(run_sqliters_lines(&conn, "", s));
        }
    }
    assert_eq!(rust_lines, c_lines);
}

#[test]
fn test_json_and_jsonb_differential_scalars_and_json5() {
    let bin = sqlite_ref_bin();
    let queries = [
        "SELECT json(' { \"a\" : [ 1 , true , false , null , \"x\" ] } '), hex(jsonb('{\"a\":[1,true,false,null,\"x\"]}'))",
        "SELECT json('{a: 0x10, b: ''tek'', c: [1,2,], // line\n d: /* blk */ +2.5}'), json_valid('{a:1}', 1), json_valid('{a:1}', 2), json_valid('{a:1}', 6)",
        "SELECT json_array(1, 'iki', null, true, 3.5), hex(jsonb_array(1, 'iki', null, true, 3.5))",
        "SELECT json_array_length('[10,20,30,40]'), json_array_length('{\"k\":[1,2,3]}', '$.k'), json_array_length('{\"k\":1}', '$.k')",
        "SELECT json_error_position('{\"a\":1}'), json_error_position('{\"a\":}'), json_error_position('[1, 2, ]'), json_error_position('[1, 2, x]')",
        "SELECT json_extract('{\"a\":[10,{\"b\":\"merhaba\"}]}', '$.a[1].b'), json_extract('{\"a\":[10,20,30]}', '$.a[0]', '$.a[#-1]', '$.yok')",
        "SELECT json(jsonb_extract(jsonb('{\"a\":[10,20,30]}'), '$.a[0]', '$.a[#-1]'))",
        "SELECT json_insert('{\"a\":1}', '$.a', 99, '$.b', 2), json_replace('{\"a\":1}', '$.a', 99, '$.b', 2)",
        "SELECT json_set('{\"arr\":[10,20]}', '$.arr[#]', 30, '$.arr[#-2]', 99, '$.nested.key', 'val')",
        "SELECT json(jsonb_set(jsonb('{\"arr\":[10,20]}'), '$.arr[#]', 30, '$.nested.key', 'val'))",
        "SELECT json_remove('[0,1,2,3,4]', '$[1]', '$[#-1]'), json(jsonb_remove(jsonb('{\"a\":1,\"b\":2,\"c\":3}'), '$.b'))",
        "SELECT json_object('k1', 1, 'k2', 'v2', 'k3', null), hex(jsonb_object('k1', 1, 'k2', 'v2'))",
        "SELECT json_patch('{\"a\":1,\"b\":{\"x\":10,\"y\":20}}', '{\"b\":{\"y\":null,\"z\":30},\"c\":[1,2]}'), json(jsonb_patch(jsonb('{\"a\":1,\"b\":2}'), '{\"b\":null,\"d\":4}'))",
        "SELECT json_pretty('{\"a\":[1,2],\"b\":{\"c\":true}}'), json_pretty('[1,2,3]', '  ')",
        "SELECT json_quote(null), json_quote(42), json_quote(3.25), json_quote('satir\n\"tirnak\"'), json_quote(json('[1,2]'))",
        "SELECT json_type('{\"a\":null,\"b\":true,\"c\":false,\"d\":7,\"e\":2.5,\"f\":\"s\",\"g\":[],\"h\":{}}', '$.a'), json_type('{\"d\":7}', '$.d'), json_type('{\"e\":2.5}', '$.e'), json_type('{\"x\":1}', '$.missing')",
        "SELECT '{\"x\":[10,{\"y\":\"evet\"}]}' -> '$.x[1].y', '{\"x\":[10,{\"y\":\"evet\"}]}' ->> '$.x[1].y', '{\"x\":[10,20]}' -> 0, '[100,200]' ->> 1",
    ];
    for q in queries {
        let p3_q = q
            .replace("hex(jsonb(", "json(jsonb(")
            .replace("hex(jsonb_array(", "json(jsonb_array(")
            .replace("hex(jsonb_object(", "json(jsonb_object(");
        assert_differential_in_memory(&bin, "", &p3_q);
    }
    {
        let _legacy = sqliters::types::LegacyFormat3Guard::enter();
        for q in queries {
            assert_differential_in_memory(&bin, "", q);
        }
    }

    let conn = Connection::open_in_memory().unwrap();
    let odd_err = conn
        .query("SELECT json_object('only_key')", &[])
        .unwrap_err();
    assert!(odd_err.message.contains("even number of arguments"));
    let null_key_err = conn.query("SELECT json_object(NULL, 1)", &[]).unwrap_err();
    assert!(null_key_err.message.contains("labels must be TEXT"));
}

#[test]
fn test_json_subtype_74_preservation_and_stripping() {
    let bin = sqlite_ref_bin();
    let setup = "
        CREATE TABLE items(id INTEGER PRIMARY KEY, doc TEXT, jb BLOB);
        INSERT INTO items VALUES
            (1, '{\"sub\":{\"k\":1},\"txt\":\"hello\"}', jsonb('{\"sub\":{\"k\":2},\"txt\":\"world\"}'));
    ";
    let queries = [
        "SELECT json_array(json('{\"a\":1}'), '{\"a\":1}', json_quote('hi'), json_quote(json('[1,2]')))",
        "SELECT json_array(doc -> '$.sub', doc ->> '$.sub', jb -> '$.sub', jb ->> '$.sub') FROM items",
        "SELECT json_object('raw', CAST(json('[1,2]') AS TEXT), 'concat', json('[1,2]') || '', 'kept', json('[1,2]'))",
        "SELECT json_array(e.value) FROM json_each('{\"a\":[1,2],\"b\":\"str\"}') e ORDER BY e.key",
    ];
    for q in queries {
        assert_differential_in_memory(&bin, setup, q);
    }
}

#[test]
fn test_json_group_aggregates_and_sliding_window_frames() {
    let bin = sqlite_ref_bin();
    let setup = "
        CREATE TABLE sales(grp TEXT, seq INT, k TEXT, v INT, jtxt TEXT);
        INSERT INTO sales VALUES
            ('A', 1, 'k1', 10, '{\"ok\":true}'),
            ('A', 2, 'k2', 20, '[1,2]'),
            ('A', 3, 'k3', 30, '\"plain\"'),
            ('B', 1, 'x1', 5,  'null'),
            ('B', 2, 'x2', 15, '42');
    ";
    let queries = [
        "SELECT grp, json_group_array(v), json(jsonb_group_array(v)), json_group_object(k, json(jtxt)), json(jsonb_group_object(k, v)) FROM sales GROUP BY grp ORDER BY grp",
        "SELECT json_object('nested_arr', json_group_array(json(jtxt))) FROM sales WHERE grp = 'A'",
        "SELECT grp, seq,
                json_group_array(v) OVER (PARTITION BY grp ORDER BY seq ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS w_arr,
                json(jsonb_group_array(v) OVER (PARTITION BY grp ORDER BY seq ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)) AS wb_arr,
                json_group_object(k, v) OVER (PARTITION BY grp ORDER BY seq ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS w_obj,
                json(jsonb_group_object(k, json(jtxt)) OVER (PARTITION BY grp ORDER BY seq ROWS BETWEEN CURRENT ROW AND 1 FOLLOWING)) AS wb_obj
           FROM sales
          ORDER BY grp, seq",
    ];
    for q in queries {
        assert_differential_in_memory(&bin, setup, q);
    }
}

#[test]
fn test_json_each_and_json_tree_lateral_joins_and_expression_indexes() {
    let bin = sqlite_ref_bin();
    let setup = "
        CREATE TABLE orders(id INTEGER PRIMARY KEY, payload TEXT, payload_b BLOB, log_cnt INT DEFAULT 0);
        CREATE INDEX idx_orders_tag ON orders(payload ->> '$.tag');
        CREATE VIEW v_order_items AS
            SELECT o.id AS order_id, o.payload ->> '$.tag' AS tag, j.key AS idx, j.value ->> '$.sku' AS sku, CAST(j.value ->> '$.qty' AS INT) AS qty
              FROM orders o, json_each(o.payload, '$.items') AS j;
        CREATE TRIGGER trg_orders_ai AFTER INSERT ON orders
        BEGIN
            UPDATE orders SET log_cnt = json_array_length(NEW.payload, '$.items') WHERE id = NEW.id;
        END;
        INSERT INTO orders(id, payload, payload_b) VALUES
            (1, '{\"tag\":\"acil\",\"items\":[{\"sku\":\"KL-1\",\"qty\":3},{\"sku\":\"DF-2\",\"qty\":5}]}',
                jsonb('{\"tag\":\"acil\",\"items\":[{\"sku\":\"KL-1\",\"qty\":3},{\"sku\":\"DF-2\",\"qty\":5}]}')),
            (2, '{\"tag\":\"standart\",\"items\":[{\"sku\":\"SL-9\",\"qty\":1}]}',
                jsonb('{\"tag\":\"standart\",\"items\":[{\"sku\":\"SL-9\",\"qty\":1}]}'));
    ";
    let queries = [
        "SELECT id, log_cnt FROM orders WHERE (payload ->> '$.tag') = 'acil'",
        "SELECT order_id, tag, idx, sku, qty FROM v_order_items ORDER BY order_id, idx",
        "SELECT o.id, t.fullkey, t.type, coalesce(cast(t.atom as text), 'CONTAINER')
           FROM orders o, json_tree(o.payload_b, '$.items') t
          ORDER BY o.id, t.id",
    ];
    for q in queries {
        assert_differential_in_memory(&bin, setup, q);
    }
}

#[test]
fn test_json_depth_limit_1000_and_corrupt_jsonb_fuzz_safety() {
    let conn = Connection::open_in_memory().unwrap();

    // Depth 999 succeeds; depth 1001 fails cleanly (`JSON_MAXIMUM_DEPTH = 1000`)
    let ok_nested = format!("{}0{}", "[".repeat(999), "]".repeat(999));
    let ok_res = conn
        .query(
            "SELECT json_valid(?1), length(jsonb(?1)) > 0",
            &[Value::Text(ok_nested)],
        )
        .unwrap();
    assert_eq!(ok_res.rows[0].get_i64(0), Some(1));
    assert_eq!(ok_res.rows[0].get_i64(1), Some(1));

    let too_deep = format!("{}0{}", "[".repeat(1001), "]".repeat(1001));
    assert_eq!(
        conn.query("SELECT json_valid(?1)", &[Value::Text(too_deep.clone())])
            .unwrap()
            .rows[0]
            .get_i64(0),
        Some(0)
    );
    assert!(
        conn.query("SELECT json(?1)", &[Value::Text(too_deep.clone())])
            .is_err()
    );
    assert!(
        conn.query("SELECT jsonb(?1)", &[Value::Text(too_deep)])
            .is_err()
    );

    // Build a deeply nested raw JSONB blob (1002 nested 1-element arrays) and verify safe rejection
    let mut deep_jb = vec![0x03, b'0']; // INT "0"
    for _ in 0..1002 {
        let payload_len = deep_jb.len();
        let mut next = Vec::with_capacity(payload_len + 5);
        if payload_len <= 11 {
            next.push(((payload_len as u8) << 4) | 11);
        } else if payload_len <= 0xff {
            next.push((12 << 4) | 11);
            next.push(payload_len as u8);
        } else {
            next.push((13 << 4) | 11);
            next.extend_from_slice(&(payload_len as u16).to_be_bytes());
        }
        next.extend_from_slice(&deep_jb);
        deep_jb = next;
    }
    assert_eq!(
        conn.query("SELECT json_valid(?1, 4)", &[Value::Blob(deep_jb.clone())])
            .unwrap()
            .rows[0]
            .get_i64(0),
        Some(1)
    );
    assert_eq!(
        conn.query("SELECT json_valid(?1, 8)", &[Value::Blob(deep_jb.clone())])
            .unwrap()
            .rows[0]
            .get_i64(0),
        Some(0)
    );
    assert!(
        conn.query("SELECT json(?1)", &[Value::Blob(deep_jb)])
            .is_err()
    );

    // Corrupt / truncated / out-of-bounds header / invalid UTF-8 JSONB byte fuzzing — NEVER panic!
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut crafted: Vec<Vec<u8>> = vec![
        vec![],
        vec![0xff], // 8-byte header truncated
        vec![0xfb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], // u64::MAX array size
        vec![0xeb, 0x7f, 0xff, 0xff, 0xff], // u32 huge array size
        vec![0x1c, 0x07], // odd-element object (truncated value)
        vec![0x2c, 0x00, 0x01], // object with NULL label
        vec![0x27, 0xff, 0xfe], // invalid UTF-8 in TEXT
        vec![0x0f], // reserved element type 15
        vec![0x0d], // reserved element type 13
        vec![0x13, b'-'], // bare '-' INT
    ];
    for len in 1..=48 {
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            buf.push((seed >> 56) as u8);
        }
        crafted.push(buf);
    }

    for blob in crafted {
        let param = [Value::Blob(blob)];
        let _ = conn
            .query("SELECT json_valid(?1, 4), json_valid(?1, 8)", &param)
            .unwrap();
        let _ = conn.query("SELECT json(?1)", &param);
        let _ = conn.query("SELECT json_type(?1)", &param);
        let _ = conn.query("SELECT json_extract(?1, '$.a[0]')", &param);
        let _ = conn.query("SELECT ?1 -> 0, ?1 ->> '$.x'", &param);
        let _ = conn.query("SELECT count(*) FROM json_each(?1)", &param);
        let _ = conn.query("SELECT count(*) FROM json_tree(?1)", &param);
    }
}

// ============================================================================
// 2. FTS5 Full-Text Search & Shadow Table Lifecycle
// ============================================================================

#[test]
fn test_fts5_shadow_tables_pragma_table_list_and_defensive_mode() {
    let ext_bin = sqlite_ext_bin();
    let setup = "CREATE VIRTUAL TABLE t2 USING fts5(a, b)";
    let list_q = "
        SELECT name, type, ncol, wr, strict
          FROM pragma_table_list
         WHERE name LIKE 't2%'
         ORDER BY name
    ";
    assert_differential_in_memory(ext_bin, setup, list_q);

    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE VIRTUAL TABLE docs_fts USING fts5(title, body);")
        .unwrap();
    let ver = conn
        .query("SELECT v FROM docs_fts_config WHERE k = 'version'", &[])
        .unwrap();
    assert_eq!(ver.rows[0].get_i64(0), Some(4));

    // Enable defensive mode: direct shadow writes must fail, while FTS5 DML succeeds
    conn.set_defensive(true).unwrap();
    assert!(conn.execute("DELETE FROM docs_fts_data;").is_err());
    assert!(conn.execute("DROP TABLE docs_fts_idx;").is_err());
    assert!(
        conn.execute("INSERT INTO docs_fts_config VALUES('x', 1);")
            .is_err()
    );

    conn.execute("INSERT INTO docs_fts(rowid, title, body) VALUES(1, 'Rust Motoru', 'Tam metin arama dizini');")
        .unwrap();
    let found = conn
        .query(
            "SELECT rowid, title FROM docs_fts WHERE docs_fts MATCH 'arama'",
            &[],
        )
        .unwrap();
    assert_eq!(found.rows.len(), 1);
    assert_eq!(found.rows[0].get_i64(0), Some(1));

    // Atomic xRename rollback when target shadow conflict exists
    conn.set_defensive(false).unwrap();
    conn.execute("CREATE TABLE blocked_fts_data(x INT);")
        .unwrap();
    assert!(
        conn.execute("ALTER TABLE docs_fts RENAME TO blocked_fts;")
            .is_err()
    );
    // Original table and shadow tables must remain intact after failed rename
    let still_ok = conn
        .query(
            "SELECT title FROM docs_fts WHERE docs_fts MATCH 'rust'",
            &[],
        )
        .unwrap();
    assert_eq!(still_ok.rows.len(), 1);
    conn.execute("DROP TABLE blocked_fts_data;").unwrap();

    // Clean atomic rename + DROP TABLE cleanup
    conn.execute("ALTER TABLE docs_fts RENAME TO archive_fts;")
        .unwrap();
    let renamed_shadows = conn
        .query(
            "SELECT name FROM pragma_table_list WHERE name LIKE 'archive_fts%' ORDER BY name",
            &[],
        )
        .unwrap();
    assert_eq!(renamed_shadows.rows.len(), 6);
    let after_rename = conn
        .query("SELECT title FROM archive_fts('metin')", &[])
        .unwrap();
    assert_eq!(after_rename.rows[0].get_str(0), Some("Rust Motoru"));

    conn.execute("DROP TABLE archive_fts;").unwrap();
    let remaining = conn
        .query(
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'archive_fts%'",
            &[],
        )
        .unwrap();
    assert_eq!(remaining.rows[0].get_i64(0), Some(0));
}

#[test]
fn test_fts5_tokenizers_unicode61_ascii_porter_trigram_and_options() {
    let ext_bin = sqlite_ext_bin();

    // 1. unicode61 diacritics + UNINDEXED + prefix='2,3'
    let setup_u61 = "
        CREATE VIRTUAL TABLE ft_u61 USING fts5(
            baslik,
            icerik,
            gizli UNINDEXED,
            prefix = '2,3',
            tokenize = 'unicode61 remove_diacritics 2'
        );
        INSERT INTO ft_u61(rowid, baslik, icerik, gizli) VALUES
            (1, 'Café Résumé', 'Çiçek ve güneşli günler', 'gizli_kelime'),
            (2, 'Kütüphane', 'Bilgisayar programlama rehberi', 'gizli_kelime');
    ";
    assert_differential_in_memory(
        ext_bin,
        setup_u61,
        "SELECT rowid FROM ft_u61 WHERE ft_u61 MATCH 'cafe AND resume' ORDER BY rowid",
    );
    assert_differential_in_memory(
        ext_bin,
        setup_u61,
        "SELECT rowid FROM ft_u61 WHERE ft_u61 MATCH 'gizli_kelime' ORDER BY rowid",
    );
    assert_differential_in_memory(
        ext_bin,
        setup_u61,
        "SELECT rowid FROM ft_u61 WHERE ft_u61 MATCH 'bi* AND pro*' ORDER BY rowid",
    );

    // 2. porter stemmer + ascii tokenizer
    let setup_porter = "
        CREATE VIRTUAL TABLE ft_porter USING fts5(body, tokenize = 'porter unicode61');
        INSERT INTO ft_porter(rowid, body) VALUES
            (1, 'The runners were running rapidly through libraries'),
            (2, 'Relational databases connection pooling');
    ";
    assert_differential_in_memory(
        ext_bin,
        setup_porter,
        "SELECT rowid FROM ft_porter WHERE ft_porter MATCH 'run AND library' ORDER BY rowid",
    );
    assert_differential_in_memory(
        ext_bin,
        setup_porter,
        "SELECT rowid FROM ft_porter WHERE ft_porter MATCH 'connect AND database' ORDER BY rowid",
    );

    // 3. trigram tokenizer (MATCH, LIKE, GLOB)
    let setup_tri = "
        CREATE VIRTUAL TABLE ft_tri USING fts5(code, tokenize = 'trigram');
        INSERT INTO ft_tri(rowid, code) VALUES
            (1, 'alpha_beta_gamma_2026'),
            (2, 'delta_epsilon_zeta_99'),
            (3, 'ab');
    ";
    assert_differential_in_memory(
        ext_bin,
        setup_tri,
        "SELECT rowid FROM ft_tri WHERE ft_tri MATCH 'beta_gam' ORDER BY rowid",
    );
    assert_differential_in_memory(
        ext_bin,
        setup_tri,
        "SELECT rowid FROM ft_tri WHERE code LIKE '%epsilon%' ORDER BY rowid",
    );
    assert_differential_in_memory(
        ext_bin,
        setup_tri,
        "SELECT rowid FROM ft_tri WHERE code GLOB '*alpha*2026*' ORDER BY rowid",
    );
}

#[test]
fn test_fts5_query_grammar_ranking_bm25_highlight_snippet_and_table_valued() {
    let ext_bin = sqlite_ext_bin();
    let setup = "
        CREATE VIRTUAL TABLE articles USING fts5(title, body);
        INSERT INTO articles(rowid, title, body) VALUES
            (1, 'SQLite Storage Engine', 'B-tree pager rollback journal and wal recovery engine design'),
            (2, 'Full Text Search', 'Inverted index postings list bm25 ranking and phrase search'),
            (3, 'Query Optimizer', 'Cost based join ordering b-tree index selection engine'),
            (4, 'Spatial Indexing', 'R-tree bounding box overlap search without b-tree');
    ";

    let queries = [
        // Phrase, `+` concatenation, `^` initial token, prefix, NEAR, AND/OR/NOT, column filters
        "SELECT rowid FROM articles WHERE articles MATCH '\"rollback journal\"' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH 'inverted + index + postings' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH '^sqlite' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH '^pager' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH 'NEAR(pager recovery, 4)' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH 'NEAR(pager design, 1)' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH 'engine NOT rollback' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH '(posting* OR spatial) AND search' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH 'title : search' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH '{title body} : optimizer' ORDER BY rowid",
        "SELECT rowid FROM articles WHERE articles MATCH '-body : engine' ORDER BY rowid",
        // Eponymous table-valued syntax `FROM articles('...')`
        "SELECT rowid, title FROM articles('search') ORDER BY rowid",
        // Highlight & snippet & bm25 ordering
        "SELECT rowid, highlight(articles, 0, '[', ']'), highlight(articles, 1, '<', '>')
           FROM articles WHERE articles MATCH 'search' ORDER BY rowid",
        "SELECT rowid, snippet(articles, 1, '<b>', '</b>', '...', 5)
           FROM articles WHERE articles MATCH 'rollback' ORDER BY rowid",
        "SELECT rowid, printf('%.6f', bm25(articles, 10.0, 1.0))
           FROM articles WHERE articles MATCH 'engine OR search' ORDER BY bm25(articles, 10.0, 1.0), rowid",
    ];
    for q in queries {
        assert_differential_in_memory(ext_bin, setup, q);
    }

    // Custom `'rank'` configuration + `ORDER BY rank` + bounded recursion depth guard
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(setup).unwrap();
    conn.execute("INSERT INTO articles(articles, rank) VALUES('rank', 'bm25(10.0, 1.0)');")
        .unwrap();
    let ranked = conn
        .query(
            "SELECT rowid FROM articles WHERE articles MATCH 'search' ORDER BY rank, rowid",
            &[],
        )
        .unwrap();
    // Row 2 has 'Search' in BOTH title (weight 10.0) and body (weight 1.0), so it must rank ahead of Row 4!
    assert_eq!(ranked.rows[0].get_i64(0), Some(2));
    assert_eq!(ranked.rows[1].get_i64(0), Some(4));

    let src_id = conn.query("SELECT fts5_source_id()", &[]).unwrap();
    assert!(src_id.rows[0].get_str(0).unwrap().starts_with("fts5:"));

    let deep_query = format!("{}engine{}", "(".repeat(300), ")".repeat(300));
    assert!(
        conn.query(
            "SELECT rowid FROM articles WHERE articles MATCH ?1",
            &[Value::Text(deep_query)],
        )
        .is_err()
    );
}

#[test]
fn test_fts5_external_content_contentless_integrity_check_and_fts5vocab() {
    let ext_bin = sqlite_ext_bin();
    let setup_ext = "
        CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT, body TEXT);
        CREATE VIRTUAL TABLE docs_idx USING fts5(title, body, content='docs', content_rowid='id');
        CREATE TRIGGER docs_ai AFTER INSERT ON docs BEGIN
            INSERT INTO docs_idx(rowid, title, body) VALUES (new.id, new.title, new.body);
        END;
        CREATE TRIGGER docs_ad AFTER DELETE ON docs BEGIN
            INSERT INTO docs_idx(docs_idx, rowid, title, body) VALUES('delete', old.id, old.title, old.body);
        END;
        CREATE TRIGGER docs_au AFTER UPDATE ON docs BEGIN
            INSERT INTO docs_idx(docs_idx, rowid, title, body) VALUES('delete', old.id, old.title, old.body);
            INSERT INTO docs_idx(rowid, title, body) VALUES (new.id, new.title, new.body);
        END;
        INSERT INTO docs VALUES
            (1, 'Alpha Guide', 'alpha beta gamma'),
            (2, 'Beta Manual', 'beta delta alpha');
        UPDATE docs SET body = 'beta omega' WHERE id = 2;
        DELETE FROM docs WHERE id = 1;
        INSERT INTO docs VALUES (3, 'Gamma Notes', 'gamma omega alpha');
        INSERT INTO docs_idx(docs_idx) VALUES('optimize');
        INSERT INTO docs_idx(docs_idx) VALUES('integrity-check');

        CREATE VIRTUAL TABLE vocab_row USING fts5vocab(docs_idx, 'row');
        CREATE VIRTUAL TABLE vocab_col USING fts5vocab(docs_idx, 'col');
        CREATE VIRTUAL TABLE vocab_inst USING fts5vocab(docs_idx, 'instance');
    ";
    assert_differential_in_memory(
        ext_bin,
        setup_ext,
        "SELECT rowid, title, body FROM docs_idx WHERE docs_idx MATCH 'omega' ORDER BY rowid",
    );
    assert_differential_in_memory(
        ext_bin,
        setup_ext,
        "SELECT term, doc, cnt FROM vocab_row ORDER BY term",
    );
    assert_differential_in_memory(
        ext_bin,
        setup_ext,
        "SELECT term, col, doc, cnt FROM vocab_col ORDER BY term, col",
    );
    assert_differential_in_memory(
        ext_bin,
        setup_ext,
        "SELECT term, doc, col, offset FROM vocab_inst ORDER BY term, doc, col, offset",
    );

    // Contentless FTS5 + `'rebuild'` + `'integrity-check'` corruption detection
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE VIRTUAL TABLE cl_ft USING fts5(msg, content='', contentless_delete=1);
         INSERT INTO cl_ft(rowid, msg) VALUES(10, 'ephemeral log token'), (20, 'another token');
         DELETE FROM cl_ft WHERE rowid = 10;",
    )
    .unwrap();
    let cl_res = conn
        .query(
            "SELECT rowid, msg FROM cl_ft WHERE cl_ft MATCH 'token'",
            &[],
        )
        .unwrap();
    assert_eq!(cl_res.rows.len(), 1);
    assert_eq!(cl_res.rows[0].get_i64(0), Some(20));
    assert_eq!(cl_res.rows[0].get(1), Some(&Value::Null));

    // Normal FTS5 integrity-check must detect tampered shadow content!
    conn.execute(
        "CREATE VIRTUAL TABLE check_ft USING fts5(body);
         INSERT INTO check_ft(rowid, body) VALUES(1, 'saglam veri kontrolu');
         INSERT INTO check_ft(check_ft) VALUES('integrity-check');",
    )
    .unwrap();
    conn.execute("UPDATE check_ft_content SET c0 = 'degistirilmis sahte veri' WHERE id = 1;")
        .unwrap();
    let corrupt_err = conn
        .execute("INSERT INTO check_ft(check_ft) VALUES('integrity-check');")
        .unwrap_err();
    assert_eq!(corrupt_err.code, ErrorCode::Corrupt);
}

// ============================================================================
// 3. R-Tree (`rtree` & `rtree_i32`) Spatial Index & C SQLite Differential Parity
// ============================================================================

#[test]
fn test_rtree_and_rtree_i32_splits_updates_deletes_and_differential_parity() {
    let ext_bin = sqlite_ext_bin();
    let mut sql = String::from(
        "CREATE VIRTUAL TABLE boxes USING rtree(id, minX, maxX, minY, maxY, +label, +weight);
         CREATE VIRTUAL TABLE boxes_i32 USING rtree_i32(id, x0, x1, y0, y1, z0, z1, +tag);\n",
    );
    // Insert 180 rectangles/cuboids to trigger multiple R*-tree node splits (> 51 cells per node)
    for i in 1..=180 {
        let x0 = (i * 7) % 200;
        let y0 = (i * 13) % 200;
        let z0 = (i * 5) % 100;
        let w = (i % 15) + 1;
        let h = (i % 11) + 2;
        sql.push_str(&format!(
            "INSERT INTO boxes VALUES({i}, {x0}.5, {}.5, {y0}.25, {}.25, 'b_{i}', {});\n",
            x0 + w,
            y0 + h,
            i * 10
        ));
        sql.push_str(&format!(
            "INSERT INTO boxes_i32 VALUES({i}, {x0}, {}, {y0}, {}, {z0}, {}, 'i_{i}');\n",
            x0 + w,
            y0 + h,
            z0 + 5
        ));
    }
    // Exercise updates, deletes (condensing nodes), OR REPLACE, and OR IGNORE
    sql.push_str(
        "DELETE FROM boxes WHERE id % 4 = 0;
         UPDATE boxes SET minX = 10.0, maxX = 25.0, label = 'updated' WHERE id IN (5, 25, 85, 145);
         INSERT OR REPLACE INTO boxes VALUES(5, 12.0, 22.0, 15.0, 30.0, 'replaced_5', 999);
         INSERT OR IGNORE INTO boxes VALUES(5, 0.0, 1.0, 0.0, 1.0, 'ignored_5', 0);
         DELETE FROM boxes_i32 WHERE id % 3 = 0;
         UPDATE boxes_i32 SET x0 = -50, x1 = -10, tag = 'moved' WHERE id IN (2, 50, 110);\n",
    );

    let queries = [
        "SELECT rtreecheck('boxes'), rtreecheck('main', 'boxes_i32')",
        "SELECT rtreedepth(data) > 0 FROM boxes_node WHERE nodeno = 1",
        "SELECT id, minX, maxX, minY, maxY, label, weight FROM boxes WHERE maxX >= 20.0 AND minX <= 60.0 AND maxY >= 20.0 AND minY <= 70.0 ORDER BY id",
        "SELECT id, label, weight FROM boxes WHERE id = 5",
        "SELECT id, x0, x1, y0, y1, z0, z1, tag FROM boxes_i32 WHERE x1 >= 30 AND x0 <= 90 AND y1 >= 40 AND y0 <= 110 AND z1 >= 20 AND z0 <= 60 ORDER BY id",
        "SELECT id, tag FROM boxes_i32 WHERE x0 < 0 ORDER BY id",
    ];
    for q in queries {
        assert_differential_in_memory(ext_bin, &sql, q);
    }

    // Verify min > max constraint error code & exact error message
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE VIRTUAL TABLE r_err USING rtree(id, minX, maxX, minY, maxY);")
        .unwrap();
    let err = conn
        .execute("INSERT INTO r_err VALUES(1, 50.0, 10.0, 0.0, 5.0);")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Constraint);
    assert_eq!(err.message, "rtree constraint failed: r_err.(minX<=maxX)");
}

#[test]
fn test_rtree_cross_engine_disk_compatibility_and_lifecycle() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let ext_bin = sqlite_ext_bin();
    let dir = TempDirGuard::new("rtree_xeng");

    // Direction A: `sqliters` builds multi-level R-Tree `.db` -> C SQLite 3.54.1 `rtreecheck` & spatial queries
    let db_a = dir.db_path("rust_rtree.db");
    let db_a_str = db_a.to_str().unwrap();
    {
        let conn = Connection::open(&db_a).unwrap();
        conn.execute("CREATE VIRTUAL TABLE r1 USING rtree(id, x0, x1, y0, y1, +meta);")
            .unwrap();
        for i in 1..=140 {
            let x = (i * 11) % 300;
            let y = (i * 17) % 300;
            conn.query(
                "INSERT INTO r1 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                &[
                    Value::Integer(i),
                    Value::Real(x as f64),
                    Value::Real((x + 15) as f64),
                    Value::Real(y as f64),
                    Value::Real((y + 20) as f64),
                    Value::Text(format!("m{i}")),
                ],
            )
            .unwrap();
        }
    }
    let check_a = "
        PRAGMA integrity_check;
        SELECT rtreecheck('r1');
        SELECT id, x0, x1, y0, y1, meta FROM r1 WHERE x1 >= 50 AND x0 <= 120 AND y1 >= 40 AND y0 <= 130 ORDER BY id;
    ";
    let c_a = run_csqlite_lines(ext_bin, db_a_str, check_a);
    assert_eq!(c_a[0], "ok");
    assert_eq!(c_a[1], "ok");
    {
        let conn = Connection::open(&db_a).unwrap();
        let mut r_a = Vec::new();
        for stmt in check_a.split(';') {
            let s = stmt.trim();
            if !s.is_empty() {
                r_a.extend(run_sqliters_lines(&conn, "", s));
            }
        }
        assert_eq!(r_a, c_a);
    }

    // Direction B: C SQLite 3.54.1 builds multi-level `rtree` + `rtree_i32` `.db` -> `sqliters` reads & `rtreecheck`s
    let db_b = dir.db_path("c_rtree.db");
    let db_b_str = db_b.to_str().unwrap();
    let mut build_b = String::from(
        "CREATE VIRTUAL TABLE cr USING rtree(id, x0, x1, y0, y1, +note);
         CREATE VIRTUAL TABLE ci USING rtree_i32(id, a0, a1, b0, b1);\n",
    );
    for i in 1..=140 {
        let x = (i * 9) % 250;
        let y = (i * 19) % 250;
        build_b.push_str(&format!(
            "INSERT INTO cr VALUES({i}, {x}.25, {}.75, {y}.0, {}.5, 'n{i}');\n",
            x + 12,
            y + 8
        ));
        build_b.push_str(&format!(
            "INSERT INTO ci VALUES({i}, {x}, {}, {y}, {});\n",
            x + 12,
            y + 8
        ));
    }
    let _ = run_csqlite_lines(ext_bin, db_b_str, &build_b);

    let check_b = "
        SELECT rtreecheck('cr'), rtreecheck('ci');
        SELECT rtreenode(2, data) FROM cr_node WHERE nodeno = 1;
        SELECT id, note FROM cr WHERE x1 >= 60 AND x0 <= 140 AND y1 >= 60 AND y0 <= 140 ORDER BY id;
        SELECT id, a0, a1 FROM ci WHERE a1 >= 60 AND a0 <= 140 AND b1 >= 60 AND b0 <= 140 ORDER BY id;
    ";
    let c_b = run_csqlite_lines(ext_bin, db_b_str, check_b);
    assert_eq!(c_b[0], "ok|ok");
    {
        let conn = Connection::open(&db_b).unwrap();
        let mut r_b = Vec::new();
        for stmt in check_b.split(';') {
            let s = stmt.trim();
            if !s.is_empty() {
                r_b.extend(run_sqliters_lines(&conn, "", s));
            }
        }
        assert_eq!(r_b, c_b);

        // Defensive mode blocks direct shadow writes on R-Tree shadow tables
        conn.set_defensive(true).unwrap();
        assert!(
            conn.execute("DELETE FROM cr_node WHERE nodeno = 1;")
                .is_err()
        );
        assert!(conn.execute("DELETE FROM cr_rowid;").is_err());
        assert!(conn.execute("DROP TABLE cr_parent;").is_err());

        // Atomic RENAME rollback on conflict, followed by clean RENAME and DROP TABLE
        conn.set_defensive(false).unwrap();
        conn.execute("CREATE TABLE conflict_r_rowid(x INT);")
            .unwrap();
        assert!(
            conn.execute("ALTER TABLE cr RENAME TO conflict_r;")
                .is_err()
        );
        assert_eq!(
            conn.query("SELECT rtreecheck('cr')", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        conn.execute("DROP TABLE conflict_r_rowid;").unwrap();

        conn.execute("ALTER TABLE cr RENAME TO cr_renamed;")
            .unwrap();
        assert_eq!(
            conn.query("SELECT rtreecheck('cr_renamed')", &[])
                .unwrap()
                .rows[0]
                .get_str(0),
            Some("ok")
        );

        // Corrupt a shadow mapping and verify `rtreecheck` reports an error instead of "ok"
        conn.execute("DELETE FROM ci_rowid WHERE rowid = 1;")
            .unwrap();
        let bad_report = conn.query("SELECT rtreecheck('ci')", &[]).unwrap();
        assert_ne!(bad_report.rows[0].get_str(0), Some("ok"));

        conn.execute("DROP TABLE cr_renamed;").unwrap();
        let left = conn
            .query(
                "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'cr_renamed%'",
                &[],
            )
            .unwrap();
        assert_eq!(left.rows[0].get_i64(0), Some(0));
    }
}

#[test]
fn test_wal_jsonb_window_interoperability_vacuum_savepoint_and_trusted_schema() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let ext_bin = sqlite_ext_bin();
    let dir = TempDirGuard::new("phase7b_wal_vac");

    // 1. WAL + JSONB + Window `rank() OVER` -> C SQLite 3.54.1 cross-engine smoke verification
    let wal_db = dir.db_path("tam_test.db");
    let wal_db_str = wal_db.to_str().unwrap();
    {
        let conn = Connection::open(&wal_db).unwrap();
        conn.execute(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE urunler(
                id INTEGER PRIMARY KEY,
                ad TEXT,
                fiyat INT,
                meta BLOB
            );
            INSERT INTO urunler
            VALUES
                (1, 'Kalem', 50, jsonb('{"etiket":"kirtasiye"}')),
                (2, 'Defter', 120, jsonb('{"etiket":"okul"}'));
            "#,
        )
        .unwrap();

        let res = conn
            .query(
                "SELECT ad, rank() OVER (ORDER BY fiyat DESC), meta ->> '$.etiket' FROM urunler ORDER BY fiyat DESC",
                &[],
            )
            .unwrap();
        assert_eq!(res.rows.len(), 2);
        assert_eq!(res.rows[0].get_str(0), Some("Defter"));
        assert_eq!(res.rows[0].get_i64(1), Some(1));
        assert_eq!(res.rows[0].get_str(2), Some("okul"));
        assert_eq!(res.rows[1].get_str(0), Some("Kalem"));
        assert_eq!(res.rows[1].get_i64(1), Some(2));
        assert_eq!(res.rows[1].get_str(2), Some("kirtasiye"));
    }

    let c_wal_out = run_csqlite_lines(
        &sqlite_ref_bin(),
        wal_db_str,
        "PRAGMA integrity_check; PRAGMA journal_mode; SELECT id, ad, json(meta) FROM urunler ORDER BY id;",
    );
    assert_eq!(
        c_wal_out,
        vec![
            "ok",
            "wal",
            "1|Kalem|{\"etiket\":\"kirtasiye\"}",
            "2|Defter|{\"etiket\":\"okul\"}",
        ]
    );

    // 2. Nested SAVEPOINT / ROLLBACK TO + VACUUM / VACUUM INTO + trusted_schema=OFF with FTS5, R-Tree, JSON
    let vtab_db = dir.db_path("vtab_vac.db");
    let vac_into_db = dir.db_path("vtab_vac_into.db");
    let vtab_db_str = vtab_db.to_str().unwrap();
    let vac_into_str = vac_into_db.to_str().unwrap();
    {
        let conn = Connection::open(&vtab_db).unwrap();
        conn.execute(
            "CREATE VIRTUAL TABLE ft_sp USING fts5(title, body);
             CREATE VIRTUAL TABLE rt_sp USING rtree(id, x0, x1, y0, y1, +tag);
             INSERT INTO ft_sp(rowid, title, body) VALUES(1, 'Alpha', 'kalici metin');
             INSERT INTO rt_sp VALUES(1, 0, 10, 0, 10, 't1');",
        )
        .unwrap();

        conn.execute("SAVEPOINT sp_outer;").unwrap();
        conn.execute("INSERT INTO ft_sp(rowid, title, body) VALUES(2, 'Beta', 'korunan satir');")
            .unwrap();
        conn.execute("INSERT INTO rt_sp VALUES(2, 20, 30, 20, 30, 't2');")
            .unwrap();

        conn.execute("SAVEPOINT sp_inner;").unwrap();
        conn.execute("INSERT INTO ft_sp(rowid, title, body) VALUES(3, 'Gamma', 'geri alinacak');")
            .unwrap();
        conn.execute("INSERT INTO rt_sp VALUES(3, 40, 50, 40, 50, 't3');")
            .unwrap();
        conn.execute("DELETE FROM ft_sp WHERE rowid = 1;").unwrap();
        conn.execute("DELETE FROM rt_sp WHERE id = 1;").unwrap();
        conn.execute("ROLLBACK TO sp_inner;").unwrap();
        conn.execute("RELEASE sp_inner;").unwrap();
        conn.execute("RELEASE sp_outer;").unwrap();

        // Innocuous virtual tables (`json_each`, `json_tree`, `fts5`, `rtree`) must work inside VIEWs under `trusted_schema = OFF`
        conn.execute(
            "CREATE VIEW v_innocuous AS
             SELECT f.rowid AS fid, r.tag AS rtag, j.value AS jv
               FROM ft_sp f
               JOIN rt_sp r ON r.id = f.rowid
               JOIN json_each('[100]') j
              WHERE ft_sp MATCH 'kalici OR korunan';
             PRAGMA trusted_schema = OFF;",
        )
        .unwrap();
        let v_rows = conn
            .query("SELECT fid, rtag, jv FROM v_innocuous ORDER BY fid", &[])
            .unwrap();
        assert_eq!(v_rows.rows.len(), 2);
        assert_eq!(v_rows.rows[0].get_i64(0), Some(1));
        assert_eq!(v_rows.rows[1].get_i64(0), Some(2));

        conn.execute("PRAGMA trusted_schema = ON;").unwrap();
        conn.execute("VACUUM;").unwrap();
        conn.execute(&format!(
            "VACUUM INTO '{}';",
            vac_into_str.replace('\'', "''")
        ))
        .unwrap();
    }

    for target_path in [vtab_db_str, vac_into_str] {
        // Cold-reopen in `sqliters`: verify B-tree, FTS5 `integrity-check`, `rtreecheck`, `MATCH`, and `VIEW`
        {
            let reopened = Connection::open(Path::new(target_path)).unwrap();
            reopened
                .execute("INSERT INTO ft_sp(ft_sp) VALUES('integrity-check');")
                .unwrap();
            let r_chk = run_sqliters_lines(
                &reopened,
                "PRAGMA trusted_schema = OFF",
                "SELECT (SELECT rtreecheck('rt_sp')), fid, rtag, jv FROM v_innocuous ORDER BY fid",
            );
            assert_eq!(r_chk, vec!["ok|1|t1|100", "ok|2|t2|100"]);
        }

        // Verify in C SQLite 3.54.1: B-tree & FTS5 `PRAGMA integrity_check`, `MATCH`, `highlight`, `bm25`, `rtreecheck`
        let c_chk = run_csqlite_lines(
            ext_bin,
            target_path,
            "PRAGMA integrity_check;
             SELECT rtreecheck('rt_sp');
             SELECT rowid, title, highlight(ft_sp, 1, '[', ']'), printf('%.6f', bm25(ft_sp))
               FROM ft_sp WHERE ft_sp MATCH 'kalici OR korunan' ORDER BY rowid;
             SELECT id, tag FROM rt_sp ORDER BY id;",
        );
        let r_fts_chk = {
            let reopened = Connection::open(Path::new(target_path)).unwrap();
            run_sqliters_lines(
                &reopened,
                "",
                "SELECT rowid, title, highlight(ft_sp, 1, '[', ']'), printf('%.6f', bm25(ft_sp))
                   FROM ft_sp WHERE ft_sp MATCH 'kalici OR korunan' ORDER BY rowid",
            )
        };
        assert_eq!(c_chk[0], "ok");
        assert_eq!(c_chk[1], "ok");
        assert_eq!(&c_chk[2..4], r_fts_chk.as_slice());
        assert_eq!(&c_chk[4..], &["1|t1", "2|t2"]);
    }

    // 3. Reverse direction: C SQLite 3.54.1 builds multi-segment FTS5 `.db` -> `sqliters` reads & verifies
    let c_fts_db = dir.db_path("csqlite_fts5_to_rust.db");
    let c_fts_str = c_fts_db.to_str().unwrap();
    run_csqlite_lines(
        ext_bin,
        c_fts_str,
        "CREATE VIRTUAL TABLE ft_c USING fts5(title, body, meta UNINDEXED, prefix='2 3');
         INSERT INTO ft_c(rowid, title, body, meta) VALUES(1, 'Rust Engine', 'safe btree pager wal', 'm1');
         INSERT INTO ft_c(rowid, title, body, meta) VALUES(2, 'JSON Binary', 'jsonb wire format codec', 'm2');
         INSERT INTO ft_c(rowid, title, body, meta) VALUES(3, 'Obsolete Row', 'temporary draft text', 'm3');
         DELETE FROM ft_c WHERE rowid = 3;
         INSERT INTO ft_c(rowid, title, body, meta) VALUES(4, 'Spatial Index', 'rtree bounding box', 'm4');
         CREATE VIRTUAL TABLE ft_c_v USING fts5vocab(ft_c, 'row');",
    );
    let expected_c_fts = run_csqlite_lines(
        ext_bin,
        c_fts_str,
        "SELECT rowid, title, meta, highlight(ft_c, 1, '<', '>'), printf('%.6f', bm25(ft_c))
           FROM ft_c WHERE ft_c MATCH 'bt* OR wire OR rt*' ORDER BY rank, rowid;
         SELECT term, doc, cnt FROM ft_c_v ORDER BY term;",
    );
    {
        let rconn = Connection::open(&c_fts_db).unwrap();
        let ic = run_sqliters_lines(&rconn, "", "PRAGMA integrity_check");
        assert_eq!(ic, vec!["ok"]);
        rconn
            .execute("INSERT INTO ft_c(ft_c) VALUES('integrity-check');")
            .unwrap();
        let mut got_r_fts = run_sqliters_lines(
            &rconn,
            "",
            "SELECT rowid, title, meta, highlight(ft_c, 1, '<', '>'), printf('%.6f', bm25(ft_c))
               FROM ft_c WHERE ft_c MATCH 'bt* OR wire OR rt*' ORDER BY rank, rowid",
        );
        got_r_fts.extend(run_sqliters_lines(
            &rconn,
            "",
            "SELECT term, doc, cnt FROM ft_c_v ORDER BY term",
        ));
        assert_eq!(got_r_fts, expected_c_fts);
    }

    // 4. Two-executor parity (VDBE bytecode VM on base table vs VIEW evaluator) on JSONB + error codes
    {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE jb_exec(id INTEGER PRIMARY KEY, jb BLOB);
             CREATE VIEW v_jb_exec AS SELECT id, jb FROM jb_exec WHERE id > 0;
             INSERT INTO jb_exec VALUES(1, jsonb('{\"etiket\":\"kalem\",\"n\":42,\"arr\":[10,20]}'));",
        )
        .unwrap();
        let vdbe_out = run_sqliters_lines(
            &conn,
            "",
            "SELECT json(jb), jb -> '$.arr', jb ->> '$.etiket', json_set(jb, '$.extra', json('[true]')) FROM jb_exec",
        );
        let view_out = run_sqliters_lines(
            &conn,
            "",
            "SELECT json(jb), jb -> '$.arr', jb ->> '$.etiket', json_set(jb, '$.extra', json('[true]')) FROM v_jb_exec",
        );
        assert_eq!(vdbe_out, view_out);

        conn.execute("INSERT INTO jb_exec VALUES(2, X'ff00ff');")
            .unwrap();
        let err_vdbe = conn
            .query("SELECT json(jb) FROM jb_exec WHERE id = 2", &[])
            .unwrap_err();
        let err_view = conn
            .query("SELECT json(jb) FROM v_jb_exec WHERE id = 2", &[])
            .unwrap_err();
        assert_eq!(err_vdbe.code, err_view.code);
        assert_eq!(err_vdbe.message, err_view.message);
    }
}
