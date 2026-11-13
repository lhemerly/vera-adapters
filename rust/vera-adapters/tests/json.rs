#![cfg(feature = "json")]
use vera_adapters::{json, Document};
#[test]
fn exact_numbers_and_reserved_looking_keys_are_not_a_serde_escape() {
    for input in [
        r#"{"big":340282366920938463463374607431768211455,"decimal":0.123456789012345678901234567890}"#,
        r#"{"$serde_json::private::Number":"123","a":[true,null,"hi"]}"#,
    ] {
        let doc = json::decode(input.as_bytes()).unwrap();
        assert_eq!(json::decode(&json::encode(&doc).unwrap()).unwrap(), doc);
    }
}
#[test]
fn malformed_ambiguous_and_trailing_documents_fail() {
    for input in [
        r#"{"a":1,"a":2}"#,
        r#"{"a":1,"\u0061":2}"#,
        r#"{"x":{"a":1,"a":1}}"#,
        "true false",
        "[]x",
        "[1,]",
        "01",
        "1e9999999999999999999999",
        "NaN",
        "",
    ] {
        assert!(json::decode(input.as_bytes()).is_err(), "{input}");
    }
    assert!(json::encode(&Document::Bytes(vec![1])).is_err());
}
#[test]
fn nested_data_has_no_128_level_format_policy_ceiling() {
    let input = format!("{}0{}", "[".repeat(512), "]".repeat(512));
    let doc = json::decode(input.as_bytes()).unwrap();
    assert_eq!(json::encode(&doc).unwrap(), input.as_bytes());
}
