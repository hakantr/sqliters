use sqliters::{Connection, Statement, StepResult};

fn names(stmt: &mut Statement) -> Vec<(i64, String, String)> {
    let mut out = Vec::new();
    while let StepResult::Row(row) = stmt.step().unwrap() {
        out.push((row.get_i64(0).unwrap(), row.get_str(1).unwrap().to_owned(), row.get_str(2).unwrap().to_owned()));
    }
    out
}

#[test]
fn database_list_freezes_before_attach_and_reprepares_after_reset_or_detach() {
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("PRAGMA encoding='{encoding}';")).unwrap();
        let main = (0, "main".to_owned(), String::new());
        let aux = (2, "aü😀".to_owned(), String::new());
        let mut list = conn.prepare("PRAGMA database_list").unwrap();
        conn.execute_batch("ATTACH ':memory:' AS \"aü😀\";").unwrap();
        assert_eq!(names(&mut list), [main.clone()]);
        list.reset().unwrap();
        assert_eq!(names(&mut list), [main.clone(), aux.clone()]);
        let mut before_detach = conn.prepare("PRAGMA database_list").unwrap();
        conn.execute_batch("DETACH \"aü😀\";").unwrap();
        assert_eq!(names(&mut before_detach), [main.clone()]);
        let mut temp = conn.prepare("PRAGMA temp.database_list").unwrap();
        assert_eq!(names(&mut temp), [main, (1, "temp".to_owned(), String::new())]);
    }
}
