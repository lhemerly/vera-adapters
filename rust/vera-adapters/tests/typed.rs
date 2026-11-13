#![cfg(all(feature = "vera", feature = "json", feature = "cbor", feature = "yaml"))]
use num_bigint::BigInt;
use std::collections::BTreeMap;
use vera_adapters::{
    cbor::Cbor,
    json::Json,
    transcode,
    vera::{decode_params, encode_params, Typed},
    yaml::Yaml,
    Adapter, Document, Number,
};
use vera_compiler::{
    analyze,
    ir::{TableId, TypeId, VariantId},
    parse_source,
    runtime::Machine,
    types::{ScalarType, ValueType},
    value::Value,
    verifier::{verify_all, VerifierConfig},
};

fn program() -> vera_compiler::ir::CheckedProgram {
    analyze(parse_source("record Packet {n:u64;price:dec128;data:bytes<4>;digest:bytes32;mode:enum{A,B};flag:bool;}union Message {Put:Packet;Clear:u8;}record Other {n:u64;}effect Seen(message:Message);transition submit(message:Message) reads [] writes [] ensure same:eq(effect_arg(Seen,0,message),message);body {emit Seen(message);commit;}").unwrap()).unwrap()
}
fn json_input() -> Vec<u8> {
    format!(r#"{{"tag":"Put","payload":{{"n":18446744073709551615,"price":170141183460469231731687303715884105727,"data":"0x00ff","digest":"0x{}","mode":"B","flag":true}}}}"#,"a5".repeat(32)).into_bytes()
}
fn adapters() -> [&'static dyn Adapter; 3] {
    [&Json, &Cbor, &Yaml]
}
#[test]
fn typed_structured_boundaries_preserve_values_across_every_format_pair() {
    let p = program();
    let ty = ValueType::Named(p.type_id("Message").unwrap());
    let typed = Typed::new(&p, ty.clone()).unwrap();
    let value = typed.decode(&Json, &json_input()).unwrap();
    for source in adapters() {
        for destination in adapters() {
            let input = typed.encode(source, &value).unwrap();
            let wire = transcode(source, destination, &input).unwrap();
            assert_eq!(typed.decode(destination, &wire).unwrap(), value);
            let canonical = vera_compiler::value::encode_value(&p, &value, &ty).unwrap();
            assert_eq!(
                vera_compiler::value::encode_value(
                    &p,
                    &typed.decode(destination, &wire).unwrap(),
                    &ty
                )
                .unwrap(),
                canonical
            );
        }
    }
}
#[test]
fn verified_runtime_ingress_and_effect_egress_are_format_independent() {
    let p = program();
    for report in verify_all(&p, &VerifierConfig::default()) {
        assert!(report.ok(), "{}", report.summary());
    }
    let transition = p.transition(p.transition_id("submit").unwrap());
    let message = Typed::new(&p, transition.params[0].ty.clone())
        .unwrap()
        .decode(&Json, &json_input())
        .unwrap();
    let values = BTreeMap::from([("message".into(), message)]);
    for adapter in adapters() {
        let wire = encode_params(adapter, &p, &transition.params, &values).unwrap();
        let args = decode_params(adapter, &p, &transition.params, &wire).unwrap();
        let mut machine = Machine::new(&p).unwrap();
        machine.call("submit", &args).unwrap();
        let effect = &machine.state.outbox[0];
        let def = p.effect(effect.effect);
        let effect_values = def
            .params
            .iter()
            .zip(&effect.args)
            .map(|(p, v)| (p.name.clone(), v.clone()))
            .collect();
        let egress = encode_params(adapter, &p, &def.params, &effect_values).unwrap();
        assert_eq!(
            decode_params(adapter, &p, &def.params, &egress).unwrap(),
            args
        );
    }
}
#[test]
fn scalar_bounds_have_no_float_rounding_or_untrusted_exponent_allocation() {
    let p = program();
    for (scalar, valid, invalid) in [
        (ScalarType::U8, "255", "256"),
        (ScalarType::U16, "65535", "65536"),
        (ScalarType::U32, "4294967295", "4294967296"),
        (
            ScalarType::U64,
            "18446744073709551615",
            "18446744073709551616",
        ),
        (
            ScalarType::I64,
            "-9223372036854775808",
            "-9223372036854775809",
        ),
        (
            ScalarType::Dec64,
            "9223372036854775807",
            "9223372036854775808",
        ),
        (
            ScalarType::Dec128,
            "-170141183460469231731687303715884105728",
            "-170141183460469231731687303715884105729",
        ),
        (ScalarType::Sym32, "4294967295", "4294967296"),
    ] {
        let typed = Typed::new(&p, ValueType::Scalar(scalar)).unwrap();
        for adapter in adapters() {
            let good = Document::Number(Number::parse(valid).unwrap());
            let bad = Document::Number(Number::parse(invalid).unwrap());
            assert!(typed
                .decode(adapter, &adapter.encode(&good).unwrap())
                .is_ok());
            assert!(typed
                .decode(adapter, &adapter.encode(&bad).unwrap())
                .is_err());
            for n in ["1e9223372036854775807", "0.1", "-0.0001"] {
                assert!(typed
                    .from_document(&Document::Number(Number::parse(n).unwrap()))
                    .is_err());
            }
        }
    }
}
#[test]
fn decimals_use_integral_coefficients_and_explicit_text_for_human_units() {
    let p = program();
    let typed = Typed::new(&p, ValueType::Scalar(ScalarType::Dec64)).unwrap();
    assert_eq!(typed.decode(&Json, b"1.0").unwrap(), Value::Int(1.into()));
    assert_eq!(
        typed.decode(&Json, br#""1.0""#).unwrap(),
        Value::Int(100_000_000.into())
    );
    assert_eq!(
        typed.decode(&Json, br#""1.00000001""#).unwrap(),
        Value::Int(100_000_001.into())
    );
    assert!(typed.decode(&Json, b"1.25").is_err());
    assert!(typed.decode(&Json, br#""1.000000001""#).is_err());
}
#[test]
fn malformed_unknown_missing_and_nominal_mismatches_fail_before_runtime() {
    let p = program();
    let message = p.type_id("Message").unwrap();
    let typed = Typed::new(&p, ValueType::Named(message)).unwrap();
    for (text, path) in [
        (r#"{"tag":"Unknown","payload":0}"#, "/tag"),
        (r#"{"tag":"Clear","payload":256}"#, "/payload"),
        (r#"{"tag":"Clear"}"#, "/payload"),
        (r#"{"tag":"Clear","payload":1,"extra":2}"#, "/extra"),
    ] {
        let document = Json.decode(text.as_bytes()).unwrap();
        for adapter in adapters() {
            let e = typed
                .decode(adapter, &adapter.encode(&document).unwrap())
                .unwrap_err();
            assert_eq!(e.path(), path);
        }
    }
    assert!(typed
        .to_document(&Value::Variant {
            ty: message,
            tag: VariantId(999),
            payload: Box::new(Value::Int(1.into()))
        })
        .is_err());
    assert!(typed
        .to_document(&Value::Record {
            ty: p.type_id("Other").unwrap(),
            fields: vec![Value::Int(1.into())]
        })
        .is_err());
    assert!(Typed::new(&p, ValueType::Named(TypeId(999))).is_err());
    assert!(Typed::new(&p, ValueType::Scalar(ScalarType::Id64(TableId(999)))).is_err());
    let bool_typed = Typed::new(&p, ValueType::Scalar(ScalarType::Bool)).unwrap();
    assert!(bool_typed.decode(&Json, b"1").is_err());
    let bytes = Typed::new(&p, ValueType::Bytes { capacity: 0 }).unwrap();
    assert_eq!(bytes.decode(&Cbor, &[0x40]).unwrap(), Value::Bytes(vec![]));
    for text in [r#""0x1""#, r#""0x00""#, r#"[]"#, r#""00""#] {
        assert!(bytes.decode(&Json, text.as_bytes()).is_err());
    }
}
#[test]
fn forged_enum_and_parameter_schemas_do_not_silently_collapse() {
    let p = program();
    for tags in [
        vec![],
        vec!["A".into(), "A".into()],
        (0..257).map(|i| format!("V{i}")).collect(),
    ] {
        assert!(Typed::new(&p, ValueType::Scalar(ScalarType::Enum(tags))).is_err());
    }
    let param = p.transitions()[0].params[0].clone();
    let params = [param.clone(), param];
    assert!(decode_params(&Json, &p, &params, b"{}").is_err());
    assert!(encode_params(&Json, &p, &params, &BTreeMap::new()).is_err());
    assert!(decode_params(&Json, &p, &p.transitions()[0].params, b"{\"unknown\":1}").is_err());
}
#[test]
fn native_bytes_and_portable_hex_share_the_declared_logical_sequence() {
    let p = program();
    let typed = Typed::new(&p, ValueType::Bytes { capacity: 4 }).unwrap();
    let value = typed.decode(&Cbor, &[0x42, 0, 255]).unwrap();
    assert_eq!(typed.encode(&Json, &value).unwrap(), br#""0x00ff""#);
    assert_eq!(
        typed
            .decode(&Yaml, &typed.encode(&Yaml, &value).unwrap())
            .unwrap(),
        value
    );
    assert!(typed.to_document(&Value::Bytes(vec![0; 5])).is_err());
    assert!(typed.to_document(&Value::Int(BigInt::from(0))).is_err());
}
