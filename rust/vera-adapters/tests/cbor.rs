#![cfg(feature = "cbor")]
use std::collections::BTreeMap;
use vera_adapters::cbor::{decode, encode};
use vera_adapters::{cbor, Adapter, Document, Number};

fn number(text: &str) -> Document {
    Document::Number(Number::parse(text).unwrap())
}
fn assert_roundtrip(document: Document) {
    let bytes = cbor::encode(&document).unwrap();
    assert_eq!(cbor::decode(&bytes).unwrap(), document);
    assert_eq!(cbor::Cbor.decode(&bytes).unwrap(), document);
    assert_eq!(cbor::Cbor.encode(&document).unwrap(), bytes);
}

#[test]
fn known_wire_vectors_are_interoperable() {
    for (wire, document) in [
        (vec![0x00], number("0")),
        (vec![0x18, 0x18], number("24")),
        (vec![0x20], number("-1")),
        (vec![0x39, 0x03, 0xe7], number("-1000")),
        (vec![0xf4], Document::Bool(false)),
        (vec![0xf5], Document::Bool(true)),
        (vec![0xf6], Document::Null),
        (vec![0x42, 0x00, 0xff], Document::Bytes(vec![0x00, 0xff])),
        (vec![0x63, b'a', b'b', b'c'], Document::Text("abc".into())),
        (vec![0xc4, 0x82, 0x21, 0x19, 0x04, 0xd2], number("12.34")),
        (
            vec![0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            number("-18446744073709551616"),
        ),
    ] {
        assert_eq!(cbor::decode(&wire).unwrap(), document, "{wire:x?}");
        assert_roundtrip(document);
    }
}

#[test]
fn exact_wide_numbers_and_decimal_exponents_roundtrip() {
    for text in [
        "18446744073709551615",
        "18446744073709551616",
        "-18446744073709551617",
        "170141183460469231731687303715884105727",
        "-170141183460469231731687303715884105728",
        "123456789012345678901234567890123456789012345678901234567890123456789",
        "-0.000000000000000001",
        "123456789012345678.123456789012345678",
        "1e9223372036854775807",
        "1e-9223372036854775808",
    ] {
        assert_roundtrip(number(text));
    }
    // The enormous exponent is encoded symbolically, never expanded into zeroes.
    assert!(
        cbor::encode(&number("1e9223372036854775807"))
            .unwrap()
            .len()
            < 16
    );
}

#[test]
fn bignum_tags_use_rfc_negative_complement() {
    assert_eq!(cbor::decode(&[0xc2, 0x40]).unwrap(), number("0"));
    assert_eq!(cbor::decode(&[0xc3, 0x40]).unwrap(), number("-1"));
    assert_eq!(cbor::decode(&[0xc3, 0x41, 0x01]).unwrap(), number("-2"));
    assert_eq!(
        cbor::decode(&[0xc2, 0x42, 0x00, 0x01]).unwrap(),
        number("1")
    );
}

#[test]
fn nested_and_indefinite_values_preserve_meaning() {
    let expected = Document::Mapping(BTreeMap::from([
        (
            "a".into(),
            Document::Sequence(vec![number("1"), Document::Bool(true)]),
        ),
        ("b".into(), Document::Bytes(vec![1, 2, 3])),
    ]));
    assert_roundtrip(expected.clone());
    let wire = [
        0xbf, 0x61, b'a', 0x9f, 1, 0xf5, 0xff, 0x61, b'b', 0x5f, 0x42, 1, 2, 0x41, 3, 0xff, 0xff,
    ];
    assert_eq!(cbor::decode(&wire).unwrap(), expected);
    assert_eq!(
        cbor::decode(&[0x7f, 0x61, b'a', 0x62, b'b', b'c', 0xff]).unwrap(),
        Document::Text("abc".into())
    );
    assert_eq!(
        cbor::decode(&[0xc4, 0x9f, 0x21, 0x18, 123, 0xff]).unwrap(),
        number("1.23")
    );
}

#[test]
fn unsupported_and_malformed_inputs_fail_closed() {
    for wire in [
        vec![],
        vec![1, 2],
        vec![0xff],
        vec![0xf7],
        vec![0xf8, 32],
        vec![0xf8, 20],
        vec![0xf8, 22],
        vec![0xf8, 0],
        vec![0xc0, 0x61, b'x'],
        vec![0xc2, 1],
        vec![0xc3, 0xf6],
        vec![0xc4, 0x81, 0],
        vec![0xc4, 0x83, 0, 0, 0],
        vec![0xc4, 0x82, 0, 0xf6],
        vec![
            0xc4, 0x82, 0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1,
        ],
        vec![0xc4, 0x9f, 0, 1, 2, 0xff],
        vec![0xc4, 0x82, 0xc4, 0x82, 0, 1, 1],
        vec![0xbf, 0x61, b'a', 0xff],
        vec![0x81, 0xff],
        vec![0xa2, 0x61, b'a', 1, 0x61, b'a', 2],
        vec![0x61, 0xff],
        vec![0x62, 0xc2],
        vec![0x62, 0xc2, 0x20],
        vec![0x7f, 0x61, 0xc2, 0x61, 0xa2, 0xff],
        vec![0x5f, 0x5f, 0xff, 0xff],
        vec![0x7f, 0x7f, 0xff, 0xff],
        vec![0x5f, 0x61, b'a', 0xff],
        vec![0x7f, 0x41, 1, 0xff],
        vec![0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        vec![0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        vec![0x1c],
        vec![0x1f],
    ] {
        assert!(cbor::decode(&wire).is_err(), "accepted {wire:x?}");
    }
}

#[test]
fn collections_have_no_library_default_depth_ceiling() {
    let mut wire = vec![0x81; 1024];
    wire.push(0xf6);
    let document = cbor::decode(&wire).unwrap();
    assert_eq!(cbor::encode(&document).unwrap(), wire);
}

#[test]
fn text_chunk_boundaries_validate_utf8_without_size_ceiling() {
    let document = Document::Text("世🙂".repeat(3000));
    assert_roundtrip(document);
}

#[test]
fn normalized_native_integers_remain_wire_integers() {
    assert_eq!(cbor::encode(&number("100")).unwrap(), [0x18, 100]);
    assert_eq!(cbor::encode(&number("-100")).unwrap(), [0x38, 99]);
}

#[test]
fn every_truncated_prefix_is_rejected() {
    let value = Document::Mapping(BTreeMap::from([
        ("data".into(), Document::Bytes(vec![1; 40])),
        (
            "number".into(),
            number("-170141183460469231731687303715884105728"),
        ),
        (
            "records".into(),
            Document::Sequence(vec![Document::Text("世界".into()), number("0.001")]),
        ),
    ]));
    let wire = cbor::encode(&value).unwrap();
    for length in 0..wire.len() {
        assert!(
            cbor::decode(&wire[..length]).is_err(),
            "accepted prefix {length}"
        );
    }
    assert_eq!(cbor::decode(&wire).unwrap(), value);
}

#[test]
fn duplicate_keys_are_detected_across_distinct_wire_encodings() {
    // Same logical "a", one definite and one chunked string encoding.
    assert!(cbor::decode(&[0xa2, 0x61, b'a', 1, 0x7f, 0x61, b'a', 0xff, 2]).is_err());
}

#[test]
fn every_half_float_preserves_raw_bits_and_width() {
    for bits in 0u16..=u16::MAX {
        let wire = [0xf9, (bits >> 8) as u8, bits as u8];
        let value = decode(&wire).unwrap();
        assert_eq!(
            value,
            Document::Float(vera_adapters::BinaryFloat {
                width: 16,
                bits: bits.into()
            })
        );
        assert_eq!(encode(&value).unwrap(), wire);
    }
}
#[test]
fn float_boundaries_and_general_map_keys_roundtrip() {
    for (width, bits) in [
        (32, 0u64),
        (32, 0x80000000),
        (32, 1),
        (32, 0x7f800000),
        (32, 0xff800000),
        (32, 0x7fc12345),
        (32, 0x7f812345),
        (64, 0),
        (64, 0x8000000000000000),
        (64, 1),
        (64, 0x7ff0000000000000),
        (64, 0xfff0000000000000),
        (64, 0x7ff8123456789abc),
        (64, 0x7ff0123456789abc),
    ] {
        let value = Document::Float(vera_adapters::BinaryFloat { width, bits });
        assert_eq!(decode(&encode(&value).unwrap()).unwrap(), value);
        for n in 0..encode(&value).unwrap().len() {
            assert!(decode(&encode(&value).unwrap()[..n]).is_err());
        }
        #[cfg(feature = "json")]
        assert!(vera_adapters::json::encode(&value).is_err());
        #[cfg(feature = "yaml")]
        assert!(vera_adapters::yaml::encode(&value).is_err());
    }
    // Integer, bytes, array, map, boolean, null and floating keys remain distinct typed values.
    for wire in [
        vec![0xa1, 1, 2],
        vec![0xbf, 0x20, 0xf5, 0xff],
        vec![0xa1, 0x41, 0, 0xf6],
        vec![0xa1, 0x81, 1, 2],
        vec![0xa1, 0xa1, 1, 2, 3],
        vec![0xa2, 0xf5, 1, 1, 2],
        vec![0xa1, 0xf6, 0xf5],
        vec![0xa1, 0xf9, 0x3c, 0, 1],
    ] {
        let value = decode(&wire).unwrap();
        assert!(matches!(value, Document::Map(_)));
        assert_eq!(decode(&encode(&value).unwrap()).unwrap(), value);
        #[cfg(feature = "json")]
        assert!(vera_adapters::json::encode(&value).is_err());
        #[cfg(feature = "yaml")]
        assert!(vera_adapters::yaml::encode(&value).is_err());
    }
}
#[test]
fn duplicate_nontext_keys_and_invalid_float_values_fail_without_panicking() {
    for wire in [
        vec![0xa2, 1, 0, 0x18, 1, 1],
        vec![0xa2, 0x81, 1, 0, 0x81, 1, 1],
        vec![0xa2, 0xf9, 0x3c, 0, 0, 0xfa, 0x3f, 0x80, 0, 0, 1],
        vec![0xbf, 1, 0xff],
    ] {
        assert!(decode(&wire).is_err());
    }
    for value in [
        vera_adapters::BinaryFloat { width: 8, bits: 0 },
        vera_adapters::BinaryFloat {
            width: 16,
            bits: 65536,
        },
    ] {
        let key = Document::Float(value);
        assert!(encode(&key).is_err());
        assert!(encode(&Document::Map(vec![
            (key.clone(), Document::Null),
            (key, Document::Null)
        ]))
        .is_err());
    }
}

#[test]
fn rfc8949_key_equivalence_handles_nan_payloads_signed_zero_and_map_order() {
    for wire in [
        vec![0xa2, 0xf9, 0, 0, 1, 0xf9, 0x80, 0, 2],
        vec![0xa2, 0xf9, 0x7e, 1, 1, 0xf9, 0xfe, 1, 2],
        vec![0xa2, 0xf9, 0x7e, 0, 1, 0xfa, 0x7f, 0xc0, 0, 0, 2],
        vec![0xa2, 0xa2, 1, 2, 3, 4, 0, 0xa2, 3, 4, 1, 2, 1],
    ] {
        assert!(decode(&wire).is_err());
    }
    // Different NaN payloads, and signaling versus quiet, are distinct map keys.
    for wire in [
        vec![0xa2, 0xf9, 0x7e, 1, 1, 0xf9, 0x7e, 2, 2],
        vec![0xa2, 0xf9, 0x7c, 1, 1, 0xf9, 0x7e, 1, 2],
    ] {
        assert!(decode(&wire).is_ok());
    }
}
