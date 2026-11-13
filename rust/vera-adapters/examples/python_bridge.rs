//! Offline process boundary for Python and other hosts. No compiler or Z3.
//! stdin/stdout contain one document, not a line-delimited or application protocol.
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
};
use vera_adapters::{
    cbor::Cbor, json::Json, yaml::Yaml, Adapter, BinaryFloat, Document, Error, Number,
};

fn adapter(name: &str) -> Result<&'static dyn Adapter, Error> {
    match name {
        "json" => Ok(&Json),
        "cbor" => Ok(&Cbor),
        "yaml" => Ok(&Yaml),
        _ => Err(Error::new("format must be json, cbor or yaml")),
    }
}
fn text(value: impl Into<String>) -> Document {
    Document::Text(value.into())
}
fn node(kind: &str, value: Document) -> Document {
    Document::Mapping(BTreeMap::from([
        ("kind".into(), text(kind)),
        ("value".into(), value),
    ]))
}
// Every value is tagged, so user mapping keys cannot collide with this envelope.
fn pack(value: Document) -> Document {
    match value {
        Document::Null => node("null", Document::Null),
        Document::Bool(v) => node("bool", Document::Bool(v)),
        Document::Number(v) => node("number", text(v.to_string())),
        Document::Float(v) => node(
            "float",
            Document::Sequence(vec![
                text(v.width.to_string()),
                text(format!(
                    "{:0width$x}",
                    v.bits,
                    width = usize::from(v.width) / 4
                )),
            ]),
        ),
        Document::Map(v) => node(
            "map",
            Document::Sequence(
                v.into_iter()
                    .map(|(k, v)| Document::Sequence(vec![pack(k), pack(v)]))
                    .collect(),
            ),
        ),
        Document::Text(v) => node("text", text(v)),
        Document::Bytes(v) => node(
            "bytes",
            text(v.iter().map(|b| format!("{b:02x}")).collect::<String>()),
        ),
        Document::Sequence(v) => node(
            "sequence",
            Document::Sequence(v.into_iter().map(pack).collect()),
        ),
        Document::Mapping(v) => node(
            "mapping",
            Document::Mapping(v.into_iter().map(|(k, v)| (k, pack(v))).collect()),
        ),
    }
}
fn unpack(value: Document) -> Result<Document, Error> {
    let Document::Mapping(mut fields) = value else {
        return Err(Error::new("expected tagged mapping"));
    };
    if fields.len() != 2 {
        return Err(Error::new("expected exactly kind and value"));
    }
    let Some(Document::Text(kind)) = fields.remove("kind") else {
        return Err(Error::new("expected text kind"));
    };
    let value = fields
        .remove("value")
        .ok_or_else(|| Error::new("missing value"))?;
    match (kind.as_str(), value) {
        ("null", Document::Null) => Ok(Document::Null),
        ("bool", Document::Bool(v)) => Ok(Document::Bool(v)),
        ("number", Document::Text(v)) => Ok(Document::Number(Number::parse(&v)?)),
        ("float", Document::Sequence(v)) => {
            let [Document::Text(width), Document::Text(bits)] = v.as_slice() else {
                return Err(Error::new("float requires [width text, hexadecimal bits]"));
            };
            let width = match width.as_str() {
                "16" => 16u8,
                "32" => 32,
                "64" => 64,
                _ => return Err(Error::new("float width must be 16, 32 or 64")),
            };
            if bits.len() != usize::from(width) / 4 || !bits.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(Error::new("float bits must be fixed-width hexadecimal"));
            }
            let bits =
                u64::from_str_radix(bits, 16).map_err(|_| Error::new("invalid float bits"))?;
            Ok(Document::Float(BinaryFloat { width, bits }))
        }
        ("map", Document::Sequence(v)) => Ok(Document::Map(
            v.into_iter()
                .enumerate()
                .map(|(i, pair)| {
                    let Document::Sequence(mut pair) = pair else {
                        return Err(Error::new("map entry must be pair").at(i.to_string()));
                    };
                    if pair.len() != 2 {
                        return Err(Error::new("map entry must have two values").at(i.to_string()));
                    }
                    let value =
                        unpack(pair.pop().unwrap()).map_err(|e| e.at("value").at(i.to_string()))?;
                    let key =
                        unpack(pair.pop().unwrap()).map_err(|e| e.at("key").at(i.to_string()))?;
                    Ok((key, value))
                })
                .collect::<Result<_, Error>>()?,
        )),
        ("text", Document::Text(v)) => Ok(text(v)),
        ("bytes", Document::Text(v)) => {
            if v.len() % 2 != 0 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::new("expected even hexadecimal bytes"));
            }
            Ok(Document::Bytes(
                v.as_bytes()
                    .chunks_exact(2)
                    .map(|pair| {
                        // ASCII and pair length validated above.
                        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
                    })
                    .collect(),
            ))
        }
        ("sequence", Document::Sequence(v)) => Ok(Document::Sequence(
            v.into_iter()
                .enumerate()
                .map(|(i, v)| unpack(v).map_err(|e| e.at(i.to_string())))
                .collect::<Result<_, _>>()?,
        )),
        ("mapping", Document::Mapping(v)) => Ok(Document::Mapping(
            v.into_iter()
                .map(|(k, v)| unpack(v).map(|v| (k.clone(), v)).map_err(|e| e.at(k)))
                .collect::<Result<_, _>>()?,
        )),
        _ => Err(Error::new("unknown kind or mismatched tagged value")),
    }
}
fn execute(args: &[String], input: &[u8]) -> Result<Vec<u8>, Error> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["decode", format] => Json.encode(&pack(adapter(format)?.decode(input)?)),
        ["encode", format] => adapter(format)?.encode(&unpack(Json.decode(input)?)?),
        ["transcode", source, destination] => {
            vera_adapters::transcode(adapter(source)?, adapter(destination)?, input)
        }
        _ => Err(Error::new(
            "usage: python_bridge decode FORMAT | encode FORMAT | transcode SOURCE DESTINATION",
        )),
    }
}
fn main() {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input)?;
        let output = execute(&std::env::args().skip(1).collect::<Vec<_>>(), &input)?;
        // Never emit partial success output for a rejected input or destination.
        io::stdout().write_all(&output)?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tagged_model_is_collision_free_and_exact() {
        let value = Document::Mapping(BTreeMap::from([
            ("kind".into(), text("bytes")),
            ("value".into(), Document::Bytes(vec![0, 255])),
            (
                "number".into(),
                Document::Number(Number::parse("18446744073709551616.125").unwrap()),
            ),
        ]));
        assert_eq!(
            unpack(
                Json.decode(&Json.encode(&pack(value.clone())).unwrap())
                    .unwrap()
            )
            .unwrap(),
            value
        );
    }
    #[test]
    fn float_bits_and_arbitrary_keys_survive_tagged_json() {
        let value = Document::Map(vec![
            (
                Document::Sequence(vec![Document::Bool(true)]),
                Document::Float(BinaryFloat {
                    width: 64,
                    bits: 0x7ff0000000000001,
                }),
            ),
            (
                Document::Float(BinaryFloat {
                    width: 16,
                    bits: 0x8000,
                }),
                Document::Float(BinaryFloat {
                    width: 32,
                    bits: 0xffc01234,
                }),
            ),
        ]);
        let tagged = Json.encode(&pack(value.clone())).unwrap();
        assert_eq!(unpack(Json.decode(&tagged).unwrap()).unwrap(), value);
    }
    #[test]
    fn malformed_tags_and_formats_fail() {
        for input in [
            r#"{"kind":"bytes","value":"0"}"#,
            r#"{"kind":"bytes","value":"zz"}"#,
            r#"{"kind":"bool","value":1}"#,
            r#"{"kind":"null","value":null,"extra":true}"#,
            r#"{"kind":"unknown","value":null}"#,
            r#"{"kind":"float","value":["16","7c0"]}"#,
            r#"{"kind":"float","value":["16","zzzz"]}"#,
            r#"{"kind":"float","value":["8","00"]}"#,
            r#"{"kind":"float","value":[16,"7c00"]}"#,
            r#"{"kind":"map","value":[[]]}"#,
            r#"{"kind":"map","value":[{}]}"#,
            r#"{"kind":"text","kind":"text","value":"x"}"#,
        ] {
            assert!(
                execute(&["encode".into(), "json".into()], input.as_bytes()).is_err(),
                "{input}"
            );
        }
        assert!(execute(&["decode".into(), "xml".into()], b"null").is_err());
        assert!(execute(&[], b"").is_err());
    }
    #[test]
    fn exact_transports_and_unsupported_destination() {
        let input = br#"{"count":18446744073709551616,"active":true}"#;
        for format in ["json", "cbor", "yaml"] {
            let wire = execute(&["transcode".into(), "json".into(), format.into()], input).unwrap();
            assert_eq!(
                adapter(format).unwrap().decode(&wire).unwrap(),
                Json.decode(input).unwrap()
            );
        }
        assert!(execute(
            &["transcode".into(), "cbor".into(), "json".into()],
            &[0x41, 0xff]
        )
        .is_err());
    }
}
