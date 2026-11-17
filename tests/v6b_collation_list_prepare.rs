use sqliters::{Connection, StepResult};

#[test]
fn collation_list_freezes_new_names_but_reprepares_for_replacement() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("PRAGMA encoding='{enc}';")).unwrap();
        let mut list = conn.prepare("PRAGMA collation_list").unwrap();
        conn.register_collation("Added", str::cmp).unwrap();
        let mut names = Vec::new();
        while let StepResult::Row(row) = list.step().unwrap() {
            names.push(row.get_str(1).unwrap().to_owned());
        }
        assert_eq!(names, ["RTRIM", "NOCASE", "BINARY"]);
        list.reset().unwrap();
        assert!(matches!(list.step().unwrap(), StepResult::Row(_)));
        let err = conn.remove_collation("Added").unwrap_err();
        assert_eq!(err.message, "unable to delete/modify collation sequence due to active statements");
        list.reset().unwrap();
        conn.register_collation("Later", str::cmp).unwrap();
        conn.register_collation("ADDED", str::cmp).unwrap();
        names.clear();
        while let StepResult::Row(row) = list.step().unwrap() {
            names.push(row.get_str(1).unwrap().to_owned());
        }
        assert_eq!(names, ["Later", "BINARY", "NOCASE", "RTRIM", "Added"]);
        conn.remove_collation("added").unwrap();
        list.reset().unwrap();
        names.clear();
        while let StepResult::Row(row) = list.step().unwrap() {
            names.push(row.get_str(1).unwrap().to_owned());
        }
        assert!(names.iter().any(|n| n == "Added"));
    }
}
