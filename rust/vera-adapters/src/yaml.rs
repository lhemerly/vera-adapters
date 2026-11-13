//! Strict, exact YAML 1.2 data-profile adapter.
//!
//! Accepts one UTF-8 document with string mapping keys, core scalars and
//! sequences. Anchors, aliases, merges, complex keys and non-core tags are
//! rejected, not expanded or silently discarded. Numbers never pass through
//! floating point. Encoding uses deterministic block YAML with quoted strings.
use crate::{Document, Error, Number};
use num_bigint::BigInt;
use saphyr_parser::{Event, Parser, ScalarStyle, Tag};
use std::collections::BTreeMap;

#[derive(Debug)]
enum Frame {
    Sequence(Vec<Document>),
    Mapping(BTreeMap<String, Document>, Option<String>),
}

fn add(value: Document, stack: &mut [Frame], root: &mut Option<Document>) -> Result<(), Error> {
    match stack.last_mut() {
        Some(Frame::Sequence(values)) => values.push(value),
        Some(Frame::Mapping(values, key)) => {
            if let Some(key) = key.take() {
                if values.insert(key.clone(), value).is_some() {
                    return Err(Error::new("duplicate YAML mapping key").at(key));
                }
            } else {
                let Document::Text(name) = value else {
                    return Err(Error::new("YAML mapping keys must be strings"));
                };
                if values.contains_key(&name) {
                    return Err(Error::new("duplicate YAML mapping key").at(name));
                }
                *key = Some(name);
            }
        }
        None => {
            if root.replace(value).is_some() {
                return Err(Error::new("expected exactly one YAML value"));
            }
        }
    }
    Ok(())
}

fn metadata(anchor: usize, tag: Option<&Tag>, expected: &str) -> Result<(), Error> {
    if anchor != 0 {
        return Err(Error::new("YAML anchors are not supported"));
    }
    if let Some(tag) = tag {
        if !tag.is_yaml_core_schema() || tag.suffix != expected {
            return Err(Error::new(format!("unsupported YAML tag {tag}")));
        }
    }
    Ok(())
}

/// Decode one complete YAML document without duplicate-key loss or numeric rounding.
pub fn decode(input: &[u8]) -> Result<Document, Error> {
    let text = std::str::from_utf8(input).map_err(|e| Error::new(e.to_string()))?;
    let mut stack = Vec::new();
    let mut root = None;
    let mut documents = 0usize;
    for event in Parser::new_from_str(text) {
        let (event, span) = event.map_err(|e| Error::new(e.to_string()))?;
        let result = match event {
            Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => Ok(()),
            Event::DocumentStart(_) => {
                documents += 1;
                if documents != 1 {
                    Err(Error::new("expected exactly one YAML document"))
                } else {
                    Ok(())
                }
            }
            Event::Scalar(text, style, anchor, tag) => {
                if anchor != 0 {
                    Err(Error::new("YAML anchors are not supported"))
                } else if text == "<<"
                    && style == ScalarStyle::Plain
                    && tag.is_none()
                    && matches!(stack.last(), Some(Frame::Mapping(_, None)))
                {
                    Err(Error::new("YAML merge keys are not supported"))
                } else {
                    scalar(&text, style, tag.as_deref())
                        .and_then(|value| add(value, &mut stack, &mut root))
                }
            }
            Event::SequenceStart(anchor, tag) => {
                metadata(anchor, tag.as_deref(), "seq").map(|()| {
                    stack.push(Frame::Sequence(Vec::new()));
                })
            }
            Event::MappingStart(anchor, tag) => metadata(anchor, tag.as_deref(), "map").map(|()| {
                stack.push(Frame::Mapping(BTreeMap::new(), None));
            }),
            Event::SequenceEnd => match stack.pop() {
                Some(Frame::Sequence(values)) => {
                    add(Document::Sequence(values), &mut stack, &mut root)
                }
                _ => Err(Error::new("invalid YAML sequence end")),
            },
            Event::MappingEnd => match stack.pop() {
                Some(Frame::Mapping(values, None)) => {
                    add(Document::Mapping(values), &mut stack, &mut root)
                }
                _ => Err(Error::new("invalid YAML mapping end")),
            },
            Event::Alias(_) => Err(Error::new("YAML aliases are not supported")),
            Event::Nothing => Err(Error::new("unexpected YAML parser event")),
        };
        result.map_err(|mut error| {
            for frame in stack.iter().rev() {
                match frame {
                    Frame::Mapping(_, Some(key)) => error = error.at(key),
                    Frame::Sequence(values) => error = error.at(values.len().to_string()),
                    Frame::Mapping(_, None) => {}
                }
            }
            error.message = format!(
                "{} at line {}, column {}",
                error.message,
                span.start.line(),
                span.start.col() + 1
            );
            error
        })?;
    }
    if documents != 1 || !stack.is_empty() {
        return Err(Error::new("expected one complete YAML document"));
    }
    root.ok_or_else(|| Error::new("empty YAML stream"))
}

fn scalar(text: &str, style: ScalarStyle, tag: Option<&Tag>) -> Result<Document, Error> {
    let explicit = match tag {
        Some(tag) if tag.is_yaml_core_schema() => Some(tag.suffix.as_str()),
        Some(tag) => return Err(Error::new(format!("unsupported YAML tag {tag}"))),
        None => None,
    };
    if explicit == Some("str") || (explicit.is_none() && style != ScalarStyle::Plain) {
        return Ok(Document::Text(text.to_owned()));
    }
    let null = matches!(text, "" | "~" | "null" | "Null" | "NULL");
    let boolean = match text {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    };
    match explicit {
        Some("null") => {
            return if null {
                Ok(Document::Null)
            } else {
                Err(Error::new("invalid YAML null"))
            }
        }
        Some("bool") => {
            return boolean
                .map(Document::Bool)
                .ok_or_else(|| Error::new("invalid YAML boolean"))
        }
        Some("int" | "float") | None => {}
        Some(other) => return Err(Error::new(format!("unsupported YAML scalar tag {other}"))),
    }
    if explicit.is_none() {
        if null {
            return Ok(Document::Null);
        }
        if let Some(value) = boolean {
            return Ok(Document::Bool(value));
        }
    }
    if matches!(
        text.to_ascii_lowercase().as_str(),
        ".nan" | ".inf" | "+.inf" | "-.inf"
    ) {
        return Err(Error::new("non-finite YAML numbers are not supported"));
    }
    if explicit != Some("float") {
        let (negative, unsigned) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let radix = unsigned
            .strip_prefix("0x")
            .map(|s| (s, 16))
            .or_else(|| unsigned.strip_prefix("0o").map(|s| (s, 8)));
        if let Some((digits, base)) = radix {
            if !digits.is_empty() && digits.chars().all(|c| c.is_digit(base)) {
                let mut integer = BigInt::parse_bytes(digits.as_bytes(), base)
                    .ok_or_else(|| Error::new("invalid YAML integer"))?;
                if negative {
                    integer = -integer;
                }
                return Number::from_parts(integer, 0).map(Document::Number);
            }
        }
    }
    if let Some(normalized) = decimal(text, explicit == Some("int")) {
        return Number::parse(&normalized).map(Document::Number);
    }
    if explicit.is_some() {
        Err(Error::new("invalid YAML numeric scalar"))
    } else {
        Ok(Document::Text(text.to_owned()))
    }
}

// YAML core numbers allow a leading '+', zeroes, '.5', and '1.'. Normalize only
// syntax; coefficient/exponent arithmetic and representability stay in Number.
fn decimal(text: &str, integer_only: bool) -> Option<String> {
    let (sign, body) = if let Some(body) = text.strip_prefix('-') {
        ("-", body)
    } else {
        ("", text.strip_prefix('+').unwrap_or(text))
    };
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(index) if !integer_only => {
            let exponent = &body[index + 1..];
            let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (&body[..index], Some(exponent))
        }
        Some(_) => return None,
        None => (body, None),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) if !integer_only => (whole, Some(fraction)),
        Some(_) => return None,
        None => (mantissa, None),
    };
    if !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.is_some_and(|s| !s.bytes().all(|b| b.is_ascii_digit()))
        || (whole.is_empty() && fraction.is_none_or(str::is_empty))
    {
        return None;
    }
    let whole = whole.trim_start_matches('0');
    let mut out = format!("{sign}{}", if whole.is_empty() { "0" } else { whole });
    if let Some(fraction) = fraction {
        out.push('.');
        out.push_str(if fraction.is_empty() { "0" } else { fraction });
    }
    if let Some(exponent) = exponent {
        out.push('e');
        out.push_str(exponent);
    }
    Some(out)
}

fn quote(text: &str, out: &mut String) {
    use std::fmt::Write;
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}'
                || ('\u{7f}'..='\u{9f}').contains(&ch)
                || matches!(ch, '\u{2028}' | '\u{2029}' | '\u{fffe}' | '\u{ffff}') =>
            {
                write!(out, "\\u{:04x}", ch as u32).expect("writing a String cannot fail");
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// Encode deterministic block YAML. Binary values require an explicit host mapping
/// (the VERA typed bridge uses hexadecimal text); they are never guessed as text.
/// Block syntax avoids the upstream scanner's 255-level flow-syntax limit.
pub fn encode(document: &Document) -> Result<Vec<u8>, Error> {
    enum Task<'a> {
        Value(&'a Document, usize),
        Key(&'a str, usize),
        Dash(usize),
    }
    let mut tasks = vec![Task::Value(document, 0)];
    let mut out = String::new();
    while let Some(task) = tasks.pop() {
        let depth = match task {
            Task::Value(_, depth) | Task::Key(_, depth) | Task::Dash(depth) => depth,
        };
        match task {
            Task::Key(text, _) => {
                indent(&mut out, depth);
                quote(text, &mut out);
                out.push_str(":\n");
            }
            Task::Dash(_) => {
                indent(&mut out, depth);
                out.push_str("-\n");
            }
            Task::Value(value, _) => match value {
                Document::Sequence(values) if !values.is_empty() => {
                    let child_depth = depth
                        .checked_add(2)
                        .ok_or_else(|| Error::new("YAML indentation exceeds usize"))?;
                    for value in values.iter().rev() {
                        tasks.push(Task::Value(value, child_depth));
                        tasks.push(Task::Dash(depth));
                    }
                }
                Document::Mapping(values) if !values.is_empty() => {
                    let child_depth = depth
                        .checked_add(2)
                        .ok_or_else(|| Error::new("YAML indentation exceeds usize"))?;
                    for (key, value) in values.iter().rev() {
                        tasks.push(Task::Value(value, child_depth));
                        tasks.push(Task::Key(key, depth));
                    }
                }
                Document::Bytes(_) => {
                    return Err(Error::new(
                        "YAML binary values require an explicit text mapping",
                    ))
                }
                value => {
                    indent(&mut out, depth);
                    match value {
                        Document::Float(_) | Document::Map(_) => return Err(Error::new("YAML profile requires explicit conversion of binary floats or non-text maps")),
                        Document::Null => out.push_str("null"),
                        Document::Bool(value) => {
                            out.push_str(if *value { "true" } else { "false" })
                        }
                        Document::Number(value) => out.push_str(&value.to_string()),
                        Document::Text(text) => quote(text, &mut out),
                        Document::Sequence(_) => out.push_str("[]"),
                        Document::Mapping(_) => out.push_str("{}"),
                        Document::Bytes(_) => unreachable!(),
                    }
                    out.push('\n');
                }
            },
        }
    }
    Ok(out.into_bytes())
}
fn indent(out: &mut String, depth: usize) {
    out.extend(std::iter::repeat_n(' ', depth));
}

/// Stateless YAML adapter for format-independent host integration.
#[derive(Clone, Copy, Debug, Default)]
pub struct Yaml;
impl crate::Adapter for Yaml {
    fn decode(&self, input: &[u8]) -> Result<Document, Error> {
        decode(input)
    }
    fn encode(&self, document: &Document) -> Result<Vec<u8>, Error> {
        encode(document)
    }
}
