#![forbid(unsafe_code)]

#[test]
fn test_types2() {
    let raw = include_str!("../../sqlite/test/types2.test");
    let script = sqliters_tcltest::strip_test_comments(raw);
    sqliters_tcltest::run_test_script_in_root("sqlite/test", "types2", &script);
}
