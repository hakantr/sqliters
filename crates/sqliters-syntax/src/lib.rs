//! `sqliters-syntax`: SQLite 3.54.1 lexical scanner, `is_complete` checker, AST, and SQL parser.
#![forbid(unsafe_code)]

pub mod ast;
pub mod complete;
pub mod parser;
pub mod rewrite;
pub mod token;

pub use ast::*;
pub use complete::is_complete;
pub use parser::{
    IdentSpanMap, TokenSpan, parse_one, parse_one_with_spans, parse_statements, split_vtab_raw_args,
};
pub use rewrite::{
    apply_span_edits, format_replacement_ident, remove_column_def_from_create_table_sql,
};
pub use token::{Token, TokenKind, dequote, keyword_kind, tokenize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_complete_matches_sqlite_complete_c() {
        assert!(!is_complete(""));
        assert!(!is_complete("   -- comment without newline"));
        assert!(!is_complete("SELECT 1"));
        assert!(is_complete("SELECT 1;"));
        assert!(!is_complete("SELECT 'unterminated;"));
        assert!(is_complete("SELECT 'a;b';"));
        assert!(!is_complete(
            "CREATE TRIGGER tr AFTER INSERT ON t BEGIN INSERT INTO log VALUES(1);"
        ));
        assert!(is_complete(
            "CREATE TRIGGER tr AFTER INSERT ON t BEGIN INSERT INTO log VALUES(1); END;"
        ));
        assert!(!is_complete(
            "CREATE TEMP TRIGGER tr AFTER INSERT ON t WHEN CASE WHEN 1 THEN 1 END BEGIN SELECT CASE WHEN 1 THEN 2 END;"
        ));
        assert!(is_complete(
            "CREATE TEMP TRIGGER tr AFTER INSERT ON t WHEN CASE WHEN 1 THEN 1 END BEGIN SELECT CASE WHEN 1 THEN 2 END; END;"
        ));
    }

    #[test]
    fn test_sqlite_3_54_digit_separators_and_schema_ddl() {
        let sql = "CREATE TABLE sqlite_schema(type text,name text,tbl_name text,rootpage int,sql text); \
                   SELECT 1_000_000, 0xdead_beef, X'4142', :user_id FROM sqlite_schema WHERE rootpage BETWEEN 1 AND 10;";
        let stmts = parse_statements(sql).unwrap();
        assert_eq!(stmts.len(), 2);
        let Stmt::CreateTable(ct) = &stmts[0] else {
            panic!("expected CreateTable");
        };
        assert_eq!(ct.name, QualifiedName::unqualified("sqlite_schema"));
        assert_eq!(
            ct.catalog_sql,
            "CREATE TABLE sqlite_schema(type text,name text,tbl_name text,rootpage int,sql text)"
        );
        assert_eq!(ct.columns.len(), 5);
    }

    #[test]
    fn test_parse_structural_qualified_names_and_catalog_sql() {
        let Stmt::CreateTable(ct) = parse_one(
            "CREATE TABLE IF NOT EXISTS main.\"a.b\"(id INT PRIMARY KEY, \"c.d\" TEXT) STRICT /* comment */;",
        )
        .unwrap()
        .0
        .unwrap()
        else {
            panic!("expected CreateTable");
        };
        assert_eq!(ct.name, QualifiedName::qualified("main", "a.b"));
        assert_eq!(
            ct.catalog_sql,
            "CREATE TABLE \"a.b\"(id INT PRIMARY KEY, \"c.d\" TEXT) STRICT"
        );

        let Stmt::CreateIndex(ci) = parse_one(
            "CREATE UNIQUE INDEX IF NOT EXISTS main.idx_q ON \"a.b\"(\"c.d\") WHERE \"c.d\" IS NOT NULL;",
        )
        .unwrap()
        .0
        .unwrap()
        else {
            panic!("expected CreateIndex");
        };
        assert_eq!(ci.name, QualifiedName::qualified("main", "idx_q"));
        assert_eq!(ci.table_name, "a.b");
        assert_eq!(
            ci.catalog_sql,
            "CREATE UNIQUE INDEX idx_q ON \"a.b\"(\"c.d\") WHERE \"c.d\" IS NOT NULL"
        );

        let Stmt::AttachDatabase {
            expr,
            schema_name,
            key,
        } = parse_one("ATTACH DATABASE 'aux.db' AS aux KEY 'secret';")
            .unwrap()
            .0
            .unwrap()
        else {
            panic!("expected AttachDatabase");
        };
        assert_eq!(expr, Expr::String("aux.db".to_owned()));
        assert_eq!(schema_name, Expr::Column(ColumnRef::unqualified("aux")));
        assert_eq!(key, Some(Expr::String("secret".to_owned())));

        let Stmt::DetachDatabase { schema_name } =
            parse_one("DETACH DATABASE aux;").unwrap().0.unwrap()
        else {
            panic!("expected DetachDatabase");
        };
        assert_eq!(schema_name, Expr::Column(ColumnRef::unqualified("aux")));
    }

    #[test]
    fn test_parse_vacuum_all_forms() {
        // 1. Bare `VACUUM`
        assert_eq!(
            parse_one("VACUUM;").unwrap().0,
            Some(Stmt::Vacuum {
                schema: None,
                into: None,
            })
        );

        // 2. `VACUUM <schema>` (`main`, `temp`, `nosuchdb`, trailing `into`)
        for s in ["main", "temp", "nosuchdb", "into"] {
            assert_eq!(
                parse_one(&format!("VACUUM {s};")).unwrap().0,
                Some(Stmt::Vacuum {
                    schema: Some(s.to_owned()),
                    into: None,
                })
            );
        }

        // 3. `VACUUM INTO <expr>`
        assert_eq!(
            parse_one("VACUUM INTO 'out.db';").unwrap().0,
            Some(Stmt::Vacuum {
                schema: None,
                into: Some(Expr::String("out.db".to_owned())),
            })
        );

        // 4. `VACUUM <schema> INTO ?1`
        assert_eq!(
            parse_one("VACUUM main INTO ?1;").unwrap().0,
            Some(Stmt::Vacuum {
                schema: Some("main".to_owned()),
                into: Some(Expr::Bind(BindParam::Numbered(1))),
            })
        );

        // 5. `VACUUM main INTO (SELECT path FROM cfg)`
        let Some(Stmt::Vacuum { schema, into }) =
            parse_one("VACUUM main INTO (SELECT path FROM cfg);")
                .unwrap()
                .0
        else {
            panic!("expected Stmt::Vacuum");
        };
        assert_eq!(schema.as_deref(), Some("main"));
        assert!(matches!(into, Some(Expr::Subquery(_))));
    }

    #[test]
    fn test_parse_view_drop_view_and_alter_drop_column() {
        let Stmt::CreateView(cv) = parse_one(
            "CREATE TEMPORARY VIEW IF NOT EXISTS temp.\"v.1\" (c1, \"c.2\") AS SELECT a, b + 1 FROM t /* cmt */;",
        )
        .unwrap()
        .0
        .unwrap()
        else {
            panic!("expected CreateView");
        };
        assert!(cv.temp);
        assert!(cv.if_not_exists);
        assert_eq!(cv.name, QualifiedName::qualified("temp", "v.1"));
        assert_eq!(cv.columns, Some(vec!["c1".to_owned(), "c.2".to_owned()]));
        assert_eq!(
            cv.catalog_sql,
            "CREATE VIEW \"v.1\" (c1, \"c.2\") AS SELECT a, b + 1 FROM t"
        );

        assert_eq!(
            parse_one("DROP VIEW IF EXISTS main.v1;").unwrap().0,
            Some(Stmt::DropView {
                if_exists: true,
                name: QualifiedName::qualified("main", "v1"),
            })
        );

        for sql in [
            "ALTER TABLE main.t DROP c;",
            "ALTER TABLE main.t DROP COLUMN c;",
        ] {
            assert_eq!(
                parse_one(sql).unwrap().0,
                Some(Stmt::AlterTable {
                    table: QualifiedName::qualified("main", "t"),
                    action: AlterTableAction::DropColumn("c".to_owned()),
                })
            );
        }

        assert_eq!(
            remove_column_def_from_create_table_sql("CREATE TABLE t1(a, b, c)", 1).unwrap(),
            "CREATE TABLE t1(a, c)"
        );
        assert_eq!(
            remove_column_def_from_create_table_sql("CREATE TABLE t1(a, c)", 1).unwrap(),
            "CREATE TABLE t1(a)"
        );
        assert_eq!(
            remove_column_def_from_create_table_sql(
                "CREATE TABLE t1(c, b, a, PRIMARY KEY(b, a)) WITHOUT ROWID",
                0
            )
            .unwrap(),
            "CREATE TABLE t1(b, a, PRIMARY KEY(b, a)) WITHOUT ROWID"
        );
    }

    #[test]
    fn test_parse_materialized_cte_and_soft_keyword_ident() {
        assert_eq!(keyword_kind("materialized"), Some(TokenKind::Materialized));

        let Stmt::Select(sel) = parse_one(
            "WITH c1 AS MATERIALIZED (VALUES(1)), c2 AS NOT MATERIALIZED (VALUES(2)), c3 AS (VALUES(3)) SELECT * FROM c1, c2, c3;",
        )
        .unwrap()
        .0
        .unwrap()
        else {
            panic!("expected Select");
        };
        assert_eq!(sel.ctes.len(), 3);
        assert_eq!(sel.ctes[0].materialized, Some(true));
        assert_eq!(sel.ctes[1].materialized, Some(false));
        assert_eq!(sel.ctes[2].materialized, None);

        // `materialized` remains valid as an unquoted table/column/alias identifier
        let Stmt::CreateTable(ct) = parse_one("CREATE TABLE materialized(materialized INT);")
            .unwrap()
            .0
            .unwrap()
        else {
            panic!("expected CreateTable");
        };
        assert_eq!(ct.name.name, "materialized");
        assert_eq!(ct.columns[0].name, "materialized");

        assert!(parse_one("WITH c AS NOT (VALUES(1)) SELECT * FROM c;").is_err());
    }

    #[test]
    fn test_parse_chained_upsert_update_from_and_row_value_set() {
        let Stmt::Insert(ins) = parse_one(
            "INSERT INTO main.t AS al(a, b, c) VALUES(1, 2, 3) \
             ON CONFLICT(a COLLATE nocase) WHERE b > 0 DO NOTHING \
             ON CONFLICT(b, c) DO UPDATE SET (b, c) = (excluded.b, excluded.c + 1), a = 9 WHERE al.c < 10 \
             ON CONFLICT DO NOTHING RETURNING *;",
        )
        .unwrap()
        .0
        .unwrap()
        else {
            panic!("expected Insert");
        };
        assert_eq!(ins.alias.as_deref(), Some("al"));
        assert_eq!(ins.upsert_clauses.len(), 3);
        assert!(matches!(
            ins.upsert_clauses[0].action,
            UpsertAction::DoNothing
        ));
        assert!(
            ins.upsert_clauses[0]
                .target
                .as_ref()
                .unwrap()
                .where_clause
                .is_some()
        );
        match &ins.upsert_clauses[1].action {
            UpsertAction::DoUpdate {
                assignments,
                where_clause,
            } => {
                assert_eq!(assignments.len(), 2);
                assert!(assignments[0].row_value);
                assert_eq!(assignments[0].columns, vec!["b".to_owned(), "c".to_owned()]);
                assert!(!assignments[1].row_value);
                assert!(where_clause.is_some());
            }
            UpsertAction::DoNothing => panic!("expected DoUpdate"),
        }
        assert!(ins.upsert_clauses[2].target.is_none());
        assert_eq!(ins.returning.len(), 1);

        // Join-ON ambiguity vs WHERE / JOIN ON + UPSERT
        let err_no_where = parse_one("INSERT INTO t SELECT * FROM src ON CONFLICT(a) DO NOTHING;")
            .unwrap_err()
            .to_string();
        assert!(
            err_no_where.contains("near \"DO\": syntax error"),
            "{err_no_where}"
        );

        assert!(
            parse_one("INSERT INTO t SELECT * FROM src WHERE 1 ON CONFLICT(a) DO NOTHING;").is_ok()
        );
        assert!(
            parse_one(
                "INSERT INTO t SELECT s1.a FROM s1 JOIN s2 ON s1.a = s2.a ON CONFLICT(a) DO NOTHING;"
            )
            .is_ok()
        );
        assert!(parse_one("INSERT INTO t DEFAULT VALUES ON CONFLICT DO NOTHING;").is_err());
        assert!(
            parse_one("INSERT INTO t VALUES(1) ON CONFLICT DO NOTHING ON CONFLICT(a) DO NOTHING;")
                .is_err()
        );

        // UPDATE ... FROM ... with row-value SET (including 1-column parenthesized SET)
        let Stmt::Update(upd) = parse_one(
            "UPDATE t1 AS tgt SET (a, b) = (src.x, src.y), (c) = 42 FROM src JOIN s2 ON src.id = s2.id WHERE tgt.id = src.id RETURNING *;",
        )
        .unwrap()
        .0
        .unwrap()
        else {
            panic!("expected Update");
        };
        assert_eq!(upd.alias.as_deref(), Some("tgt"));
        assert_eq!(upd.assignments.len(), 2);
        assert!(upd.assignments[0].row_value);
        assert_eq!(
            upd.assignments[0].columns,
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert!(upd.assignments[1].row_value);
        assert_eq!(upd.assignments[1].columns, vec!["c".to_owned()]);
        assert!(upd.from.is_some());
        assert!(upd.selection.is_some());
        assert_eq!(upd.returning.len(), 1);
    }

    #[test]
    fn test_parse_create_and_drop_trigger_and_raise() {
        let Stmt::CreateTrigger(ct) = parse_one(
            "CREATE TEMPORARY TRIGGER IF NOT EXISTS \"tr.1\" /* c1 */ UPDATE OF a, \"b.c\" ON aux.t1 \
             FOR EACH ROW WHEN new.a > 0 BEGIN \
               SELECT RAISE(ABORT, 'bad: ' || new.a); -- c2\n\
               INSERT INTO log AS l(x) VALUES(CASE WHEN new.a = 1 THEN 10 ELSE 20 END) ON CONFLICT(x) DO NOTHING; \
               UPDATE log SET x = x + 1 FROM t2 WHERE x = old.a; \
               DELETE FROM log WHERE x < 0; \
             END ;",
        )
        .unwrap()
        .0
        .unwrap()
        else {
            panic!("expected CreateTrigger");
        };
        assert!(ct.temp);
        assert!(ct.if_not_exists);
        assert_eq!(ct.name, QualifiedName::unqualified("tr.1"));
        assert_eq!(ct.name_token_text, "\"tr.1\"");
        assert_eq!(ct.timing, TriggerTiming::Before);
        assert_eq!(
            ct.event,
            TriggerEvent::Update {
                columns: Some(vec!["a".to_owned(), "b.c".to_owned()])
            }
        );
        assert_eq!(ct.on_table, QualifiedName::qualified("aux", "t1"));
        assert!(ct.for_each_row);
        assert!(ct.when_clause.is_some());
        assert_eq!(ct.body.len(), 4);
        assert!(
            ct.catalog_sql
                .starts_with("CREATE TRIGGER \"tr.1\" /* c1 */ UPDATE OF a")
        );
        assert!(ct.catalog_sql.ends_with("END"));

        assert_eq!(
            parse_one("DROP TRIGGER IF EXISTS main.tr1;").unwrap().0,
            Some(Stmt::DropTrigger {
                if_exists: true,
                name: QualifiedName::qualified("main", "tr1"),
            })
        );

        let err_idx = parse_one(
            "CREATE TRIGGER tr AFTER INSERT ON t BEGIN UPDATE t INDEXED BY i1 SET a = 1; END;",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err_idx.contains(
                "the INDEXED BY clause is not allowed on UPDATE or DELETE statements within triggers"
            ),
            "{err_idx}"
        );

        let err_not_idx =
            parse_one("CREATE TRIGGER tr AFTER INSERT ON t BEGIN DELETE FROM t NOT INDEXED; END;")
                .unwrap_err()
                .to_string();
        assert!(
            err_not_idx.contains(
                "the NOT INDEXED clause is not allowed on UPDATE or DELETE statements within triggers"
            ),
            "{err_not_idx}"
        );

        let err_ret = parse_one(
            "CREATE TRIGGER tr AFTER INSERT ON t BEGIN INSERT INTO t VALUES(1) RETURNING *; END;",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err_ret.contains("cannot use RETURNING in a trigger"),
            "{err_ret}"
        );
    }

    #[test]
    fn test_parse_window_clauses_frames_and_soft_keywords() {
        let sql = "SELECT
            rank() OVER w1,
            sum(x) FILTER (WHERE y > 0) OVER (
                w1 ORDER BY z DESC NULLS FIRST
                GROUPS BETWEEN 2 PRECEDING AND 1 FOLLOWING EXCLUDE TIES
            ),
            first_value(x) OVER (ROWS UNBOUNDED PRECEDING EXCLUDE CURRENT ROW),
            last_value(x) OVER (RANGE CURRENT ROW EXCLUDE GROUP),
            count(*) OVER (ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING EXCLUDE NO OTHERS)
          FROM tbl
          WINDOW w1 AS (PARTITION BY a, b), w2 AS (w1 ORDER BY c ASC);";
        let Stmt::Select(sel) = parse_one(sql).unwrap().0.unwrap() else {
            panic!("expected Select");
        };
        let SelectCore::Select {
            projection,
            windows,
            ..
        } = &sel.body.head
        else {
            panic!("expected SelectCore::Select");
        };
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].0, "w1");
        assert_eq!(windows[0].1.partition_by.len(), 2);
        assert_eq!(windows[1].0, "w2");
        assert_eq!(windows[1].1.base_window.as_deref(), Some("w1"));
        assert_eq!(windows[1].1.order_by.len(), 1);
        assert_eq!(projection.len(), 5);

        // Soft-keyword disambiguation (`FROM tbl window`, `SELECT sum(x) over FROM tbl`, etc.)
        let Stmt::Select(alias_win) =
            parse_one("SELECT window.a FROM tbl window WHERE window.a = 1;")
                .unwrap()
                .0
                .unwrap()
        else {
            panic!("expected Select");
        };
        let SelectCore::Select {
            from: Some(fc),
            windows: empty_wins,
            ..
        } = &alias_win.body.head
        else {
            panic!("expected SelectCore::Select");
        };
        assert!(empty_wins.is_empty());
        assert!(matches!(
            &fc.head,
            TableOrSubquery::Table {
                alias: Some(al),
                ..
            } if al == "window"
        ));

        let Stmt::Select(alias_over) =
            parse_one("SELECT sum(x) over, count(*) filter, a rows FROM tbl groups;")
                .unwrap()
                .0
                .unwrap()
        else {
            panic!("expected Select");
        };
        let SelectCore::Select {
            projection: p2,
            from: Some(fc2),
            ..
        } = &alias_over.body.head
        else {
            panic!("expected SelectCore::Select");
        };
        assert!(matches!(
            &p2[0],
            ResultColumn::Expr {
                expr: Expr::FunctionCall { over: None, .. },
                alias: Some(al),
                ..
            } if al == "over"
        ));
        assert!(matches!(
            &p2[1],
            ResultColumn::Expr {
                expr: Expr::FunctionCall { filter: None, over: None, .. },
                alias: Some(al),
                ..
            } if al == "filter"
        ));
        assert!(matches!(
            &p2[2],
            ResultColumn::Expr {
                alias: Some(al),
                ..
            } if al == "rows"
        ));
        assert!(matches!(
            &fc2.head,
            TableOrSubquery::Table {
                alias: Some(al),
                ..
            } if al == "groups"
        ));

        // Frame bound validation errors matching `sqlite/src/window.c` & `parse.y`
        for bad_frame in [
            "SELECT count(*) OVER (ROWS 2 FOLLOWING) FROM t;",
            "SELECT count(*) OVER (ROWS BETWEEN CURRENT ROW AND 1 PRECEDING) FROM t;",
            "SELECT count(*) OVER (ROWS BETWEEN 2 FOLLOWING AND CURRENT ROW) FROM t;",
            "SELECT count(*) OVER (ROWS BETWEEN 2 FOLLOWING AND 1 PRECEDING) FROM t;",
        ] {
            let err = parse_one(bad_frame).unwrap_err();
            assert_eq!(
                err.message, "unsupported frame specification",
                "{bad_frame}"
            );
        }

        let err_dist = parse_one("SELECT count(DISTINCT x) OVER () FROM t;").unwrap_err();
        assert_eq!(
            err_dist.message,
            "DISTINCT is not supported for window functions"
        );
    }

    /// `ALTER TABLE ... ADD [COLUMN]` keeps the column definition as written — from the column
    /// name to the end of the statement, minus trailing `;` and whitespace — because that text,
    /// not a rendering of the AST, is what goes into the stored `CREATE TABLE` statement
    /// (`sqlite3AlterFinishAddColumn`).
    #[test]
    fn alter_table_add_column_keeps_the_definition_text() {
        for (sql, want) in [
            (
                "ALTER TABLE t ADD COLUMN b INT DEFAULT 7;",
                "b INT DEFAULT 7",
            ),
            ("ALTER TABLE t ADD b", "b"),
            (
                "ALTER TABLE t ADD COLUMN \"k k\" DEFAULT x'00'  ;  ",
                "\"k k\" DEFAULT x'00'",
            ),
            (
                "ALTER TABLE t ADD COLUMN b /* c */ INT /* d */ ;",
                "b /* c */ INT /* d */",
            ),
            (
                "ALTER TABLE t ADD COLUMN b\n  INT\n  DEFAULT 5\n;SELECT 1;",
                "b\n  INT\n  DEFAULT 5",
            ),
            ("ALTER TABLE t ADD COLUMN c INT -- tail\n", "c INT -- tail"),
            (
                "ALTER TABLE main.t ADD g INT NOT NULL DEFAULT 0 CHECK(g >= 0);",
                "g INT NOT NULL DEFAULT 0 CHECK(g >= 0)",
            ),
            (
                "ALTER TABLE t ADD COLUMN i GENERATED ALWAYS AS (a+1) ;",
                "i GENERATED ALWAYS AS (a+1)",
            ),
            (
                "ALTER TABLE t ADD COLUMN m VARCHAR(10, 2) DEFAULT 'a''b';;",
                "m VARCHAR(10, 2) DEFAULT 'a''b'",
            ),
        ] {
            let (stmt, _) = parse_one(sql).unwrap();
            let Some(Stmt::AlterTable {
                action: AlterTableAction::AddColumn { column_sql, .. },
                ..
            }) = stmt
            else {
                panic!("{sql}: not an ADD COLUMN statement");
            };
            assert_eq!(column_sql, want, "{sql}");
        }

        // A DEFAULT that is a bare or quoted identifier is a string literal, and the most
        // negative integer is an integer (`parse.y`, `ccons ::= DEFAULT ...`).
        for (def, want) in [
            ("b DEFAULT abc", Expr::String("abc".to_owned())),
            ("b DEFAULT \"q r\"", Expr::String("q r".to_owned())),
            ("b DEFAULT -9223372036854775808", Expr::Integer(i64::MIN)),
            ("b DEFAULT TRUE", Expr::Bool(true)),
        ] {
            let (stmt, _) = parse_one(&format!("ALTER TABLE t ADD COLUMN {def}")).unwrap();
            let Some(Stmt::AlterTable {
                action: AlterTableAction::AddColumn { column, .. },
                ..
            }) = stmt
            else {
                panic!("{def}");
            };
            assert_eq!(
                column.constraints,
                [ColumnConstraint::Default(want)],
                "{def}"
            );
        }
    }
}
