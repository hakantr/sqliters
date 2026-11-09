#![forbid(unsafe_code)]

#[test]
fn test_affinity3() {
    let raw = include_str!("../../sqlite/test/affinity3.test");
    let script = sqliters_tcltest::strip_test_comments(raw);
    sqliters_tcltest::run_test_script_in_root("sqlite/test", "affinity3", &script);
}
