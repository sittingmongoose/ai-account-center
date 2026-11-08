use aac_status::ops;

#[test]
fn unknown_method_is_refused_with_a_fixed_code() {
    let error = ops::dispatch("nope", "{}").unwrap_err();
    assert_eq!(error.code, "unknown_method");
}
