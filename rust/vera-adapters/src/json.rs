//! Exact JSON with duplicate-key rejection and one-document framing.
use crate::{Adapter, Document, Error, Number};
use serde::{
    de::{MapAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::value::RawValue;
use std::{collections::BTreeSet, fmt};

#[derive(Clone, Copy, Debug, Default)]
pub struct Json;
impl Adapter for Json {
    fn decode(&self, input: &[u8]) -> Result<Document, Error> {
        decode(input)
    }
    fn encode(&self, document: &Document) -> Result<Vec<u8>, Error> {
        encode(document)
    }
}

fn read<'de, T: Deserialize<'de>>(input: &'de [u8]) -> Result<T, Error> {
    let mut de = serde_json::Deserializer::from_slice(input);
    de.disable_recursion_limit();
    let value = T::deserialize(serde_stacker::Deserializer::new(&mut de))
        .map_err(|e| Error::new(e.to_string()))?;
    de.end().map_err(|e| Error::new(e.to_string()))?;
    Ok(value)
}

struct Object<'a>(Vec<(String, &'a RawValue)>);
impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object with unique text keys")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Object<'de>, M::Error> {
                let mut keys = BTreeSet::new();
                let mut fields = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate mapping key {key:?}"
                        )));
                    }
                    fields.push((key, map.next_value()?));
                }
                Ok(Object(fields))
            }
        }
        de.deserialize_map(ObjectVisitor)
    }
}

pub fn decode(input: &[u8]) -> Result<Document, Error> {
    enum Task<'a> {
        Parse(&'a RawValue),
        Sequence(usize),
        Mapping(Vec<String>),
    }
    let mut tasks = vec![Task::Parse(read(input)?)];
    let mut values = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Parse(raw) => {
                let bytes = raw.get().as_bytes();
                match bytes.first() {
                    Some(b'[') => {
                        let items: Vec<&RawValue> = read(bytes)?;
                        tasks.push(Task::Sequence(items.len()));
                        tasks.extend(items.into_iter().rev().map(Task::Parse));
                    }
                    Some(b'{') => {
                        let Object(fields) = read(bytes)?;
                        tasks.push(Task::Mapping(
                            fields.iter().map(|(k, _)| k.clone()).collect(),
                        ));
                        tasks.extend(fields.into_iter().rev().map(|(_, v)| Task::Parse(v)));
                    }
                    Some(b'"') => values.push(Document::Text(read(bytes)?)),
                    Some(b'n') => {
                        let _: () = read(bytes)?;
                        values.push(Document::Null);
                    }
                    Some(b't' | b'f') => values.push(Document::Bool(read(bytes)?)),
                    _ => values.push(Document::Number(Number::parse(raw.get())?)),
                }
            }
            Task::Sequence(count) => {
                let children = values.split_off(values.len() - count);
                values.push(Document::Sequence(children));
            }
            Task::Mapping(keys) => {
                let children = values.split_off(values.len() - keys.len());
                values.push(Document::Mapping(keys.into_iter().zip(children).collect()));
            }
        }
    }
    values
        .pop()
        .ok_or_else(|| Error::new("missing JSON document"))
}

pub fn encode(document: &Document) -> Result<Vec<u8>, Error> {
    enum Task<'a> {
        Node(&'a Document),
        Text(&'a str),
        Raw(&'static [u8]),
    }
    let mut tasks = vec![Task::Node(document)];
    let mut out = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Raw(bytes) => out.extend_from_slice(bytes),
            Task::Text(text) => serde_json::to_writer(&mut out, text).map_err(|e| Error::new(e.to_string()))?,
            Task::Node(node) => match node {
                Document::Float(_) | Document::Map(_) => return Err(Error::new("JSON requires explicit conversion of binary floats or non-text maps")),
                Document::Null => out.extend_from_slice(b"null"),
                Document::Bool(true) => out.extend_from_slice(b"true"),
                Document::Bool(false) => out.extend_from_slice(b"false"),
                Document::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
                Document::Text(s) => tasks.push(Task::Text(s)),
                Document::Bytes(_) => return Err(Error::new("JSON has no native byte string; select a typed or explicit text representation")),
                Document::Sequence(items) => {
                    out.push(b'[');
                    tasks.push(Task::Raw(b"]"));
                    for (i, item) in items.iter().enumerate().rev() {
                        tasks.push(Task::Node(item));
                        if i != 0 { tasks.push(Task::Raw(b",")); }
                    }
                }
                Document::Mapping(fields) => {
                    out.push(b'{');
                    tasks.push(Task::Raw(b"}"));
                    for (i,(key,value)) in fields.iter().enumerate().rev() {
                        tasks.push(Task::Node(value));
                        tasks.push(Task::Raw(b":"));
                        tasks.push(Task::Text(key));
                        if i != 0 { tasks.push(Task::Raw(b",")); }
                    }
                }
            }
        }
    }
    Ok(out)
}
