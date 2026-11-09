#![forbid(unsafe_code)]

#[test]
fn test_rowvalue7() {
    let raw = include_str!("../../sqlite/test/rowvalue7.test");
    let script = sqliters_tcltest::strip_test_comments(raw);
    sqliters_tcltest::run_test_script_in_root("sqlite/test", "rowvalue7", &script);
}
