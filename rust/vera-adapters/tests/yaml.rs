#![cfg(feature = "yaml")]
use std::collections::BTreeMap;
use vera_adapters::{yaml, Document, Number};

fn parse(text: &str) -> Document {
    yaml::decode(text.as_bytes()).unwrap()
}
fn number(text: &str) -> Document {
    Document::Number(Number::parse(text).unwrap())
}

#[test]
fn block_and_flow_config_preserve_exact_numbers_and_string_meanings() {
    let value = parse("count: 18446744073709551616\nprice: 12345678901234567890.123456789\nflags: [true, false, null]\nname: yes\ndate: 2026-11-13\nquoted: '42'\ntext: |\n  hello\n  world\n");
    let Document::Mapping(ref fields) = value else {
        panic!()
    };
    assert_eq!(fields["count"], number("18446744073709551616"));
    assert_eq!(fields["price"], number("12345678901234567890.123456789"));
    assert_eq!(fields["name"], Document::Text("yes".into()));
    assert_eq!(fields["date"], Document::Text("2026-11-13".into()));
    assert_eq!(fields["quoted"], Document::Text("42".into()));
    assert_eq!(fields["text"], Document::Text("hello\nworld\n".into()));
    assert_eq!(yaml::decode(&yaml::encode(&value).unwrap()).unwrap(), value);
}

#[test]
fn yaml_core_numbers_are_exact_and_do_not_use_float() {
    for (yaml, expected) in [
        ("+001", "1"),
        ("-0001", "-1"),
        (".5", "0.5"),
        ("1.", "1"),
        ("1.e+3", "1000"),
        (
            "0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
            "340282366920938463463374607431768211455",
        ),
        ("0o777", "511"),
        ("1e-300000", "1e-300000"),
        ("!!int '00042'", "42"),
        ("!!float '1.25'", "1.25"),
    ] {
        assert_eq!(parse(yaml), number(expected), "{yaml}");
    }
    assert_eq!(parse("!!str 123"), Document::Text("123".into()));
    assert_eq!(parse("!!bool 'TRUE'"), Document::Bool(true));
    assert_eq!(parse("!!null ~"), Document::Null);
}

#[test]
fn malformed_ambiguous_or_unsupported_input_is_rejected() {
    for input in [
        "",
        "# comment",
        "a: 1\na: 2",
        "a: 1\n'a': 2",
        "!!str a: 1\na: 2",
        "1: x",
        "? [a, b]\n: x",
        "x: &a 1",
        "x: *a",
        "<<: {a: 1}",
        "!custom x",
        "!!binary aGVsbG8=",
        "!!int abc",
        "!!bool yes",
        "!!null nope",
        ".nan",
        ".inf",
        "-.INF",
        "---\na: 1\n---\nb: 2",
        "[1,",
        "!!map [1]",
        "!!seq {a: 1}",
        "1e999999999999999999999999999",
    ] {
        assert!(
            yaml::decode(input.as_bytes()).is_err(),
            "accepted {input:?}"
        );
    }
    assert!(yaml::decode(&[0xff]).is_err());
    assert!(yaml::encode(&Document::Bytes(vec![0, 1])).is_err());
}

#[test]
fn text_escaping_and_quoted_special_keys_round_trip() {
    let text = "\0\u{1}\t\r\n\u{7f}\u{85}\u{2028}\u{2029}\u{fffe}\u{ffff}\\\" café 🦀";
    let document = Document::Mapping(BTreeMap::from([
        ("<<".into(), Document::Text(text.into())),
        ("true".into(), Document::Text("false".into())),
        ("".into(), Document::Sequence(vec![])),
    ]));
    let encoded = yaml::encode(&document).unwrap();
    assert_eq!(yaml::decode(&encoded).unwrap(), document);
    assert_eq!(yaml::encode(&document).unwrap(), encoded);
}

#[test]
fn one_explicit_empty_document_is_null() {
    assert_eq!(parse("---\n...\n"), Document::Null);
    assert_eq!(parse("!!map {a: !!seq [1, 2]}"), parse("a: [1, 2]"));
}

#[test]
fn no_adapter_nesting_ceiling_is_imposed() {
    let mut input = String::new();
    for level in 0..300 {
        input.push_str(&format!("{}-\n", " ".repeat(level * 2)));
    }
    input.push_str(&format!("{}0\n", " ".repeat(600)));
    let doc = parse(&input);
    let encoded = yaml::encode(&doc).unwrap();
    assert_eq!(yaml::decode(&encoded).unwrap(), doc);
}

#[test]
fn upstream_flow_nesting_limit_is_a_reported_error_not_a_silent_truncation() {
    let input = format!("{}0{}", "[".repeat(256), "]".repeat(256));
    assert!(yaml::decode(input.as_bytes()).is_err());
}
#[test]
fn duplicate_key_errors_retain_machine_readable_paths() {
    let error = yaml::decode(b"a: 1\na: 2").unwrap_err();
    assert_eq!(error.path(), "/a");
    let error = yaml::decode(b"outer: {a: 1, a: 2}").unwrap_err();
    assert_eq!(error.path(), "/outer/a");
    let error = yaml::decode(b"outer: [0, !!int nope]").unwrap_err();
    assert_eq!(error.path(), "/outer/1");
}
