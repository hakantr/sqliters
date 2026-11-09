//! `sqliters-syntax`: SQLite 3.54.1 lexical scanner, `is_complete` checker, AST, and SQL parser.
#![forbid(unsafe_code)]

pub mod ast;
pub mod complete;
pub mod parser;
pub mod token;

pub use ast::*;
pub use complete::is_complete;
pub use parser::{parse_one, parse_statements};
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
}
