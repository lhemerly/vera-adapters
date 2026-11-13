//! Exact CBOR boundary profile, implemented on Ciborium's wire primitives.
//!
//! This profile accepts arbitrary map keys, arrays, bytes, text, booleans, null,
//! integers, bignums (tags 2/3), decimal fractions (tag 4), and raw IEEE floats.
//! Undefined, other simple values and unknown tags are rejected, not coerced.
//! Indefinite collections and strings are accepted with the same validation.
//! The encoder is deterministic but does not claim RFC 8949 canonical encoding.
use crate::{BinaryFloat, Document, Error, Number};
use ciborium_ll::{Decoder, Encoder, Header};
use num_bigint::{BigInt, Sign};
use std::collections::BTreeMap;

struct Reader<'a> {
    wire: Decoder<&'a [u8]>,
    input: &'a [u8],
}
impl<'a> Reader<'a> {
    fn from(input: &'a [u8]) -> Self {
        Self {
            wire: Decoder::from(input),
            input,
        }
    }
    fn offset(&mut self) -> usize {
        self.wire.offset()
    }
    fn pull(&mut self) -> Result<Header, ciborium_ll::Error<std::io::Error>> {
        let offset = self.offset();
        // RFC 8949 section 3.3: additional-info 24 may not encode simple 0..31.
        // Ciborium's low-level header parser intentionally permits these values.
        if self.input.get(offset) == Some(&0xf8)
            && self.input.get(offset + 1).is_some_and(|value| *value < 32)
        {
            return Err(ciborium_ll::Error::Syntax(offset));
        }
        self.wire.pull()
    }
}
type Writer<'a> = Encoder<&'a mut Vec<u8>>;

fn wire_error(error: impl std::fmt::Debug) -> Error {
    Error::new(format!("invalid CBOR: {error:?}"))
}

fn bytes(reader: &mut Reader<'_>, length: Option<usize>) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut next = length;
    loop {
        let len = match next {
            Some(len) => len,
            None => match reader.pull().map_err(wire_error)? {
                Header::Bytes(Some(len)) => len,
                Header::Break => return Ok(out),
                _ => {
                    return Err(Error::new(
                        "CBOR byte-string chunks must be definite byte strings",
                    ))
                }
            },
        };
        let mut segments = reader.wire.bytes(Some(len));
        let mut scratch = [0; 4096];
        while let Some(mut segment) = segments.pull().map_err(wire_error)? {
            while let Some(chunk) = segment.pull(&mut scratch).map_err(wire_error)? {
                out.extend_from_slice(chunk);
            }
        }
        if length.is_some() {
            return Ok(out);
        }
        next = None;
    }
}

fn text(reader: &mut Reader<'_>, length: Option<usize>) -> Result<String, Error> {
    let mut out = String::new();
    let mut next = length;
    loop {
        let len = match next {
            Some(len) => len,
            None => match reader.pull().map_err(wire_error)? {
                Header::Text(Some(len)) => len,
                Header::Break => return Ok(out),
                _ => {
                    return Err(Error::new(
                        "CBOR text chunks must be definite UTF-8 text strings",
                    ))
                }
            },
        };
        let mut segments = reader.wire.text(Some(len));
        let mut scratch = [0; 4096];
        while let Some(mut segment) = segments.pull().map_err(wire_error)? {
            while let Some(chunk) = segment.pull(&mut scratch).map_err(wire_error)? {
                out.push_str(chunk);
            }
        }
        if length.is_some() {
            return Ok(out);
        }
        next = None;
    }
}

fn big_integer(reader: &mut Reader<'_>, negative: bool) -> Result<BigInt, Error> {
    let Header::Bytes(length) = reader.pull().map_err(wire_error)? else {
        return Err(Error::new("CBOR bignum requires a byte-string payload"));
    };
    let integer = BigInt::from_bytes_be(Sign::Plus, &bytes(reader, length)?);
    Ok(if negative { -integer - 1u8 } else { integer })
}

fn integer(reader: &mut Reader<'_>) -> Result<BigInt, Error> {
    match reader.pull().map_err(wire_error)? {
        Header::Positive(value) => Ok(BigInt::from(value)),
        Header::Negative(value) => Ok(-BigInt::from(value) - 1u8),
        Header::Tag(2) => big_integer(reader, false),
        Header::Tag(3) => big_integer(reader, true),
        _ => Err(Error::new(
            "CBOR decimal fraction components must be integers",
        )),
    }
}

fn decimal(reader: &mut Reader<'_>) -> Result<Number, Error> {
    let indefinite = match reader.pull().map_err(wire_error)? {
        Header::Array(Some(2)) => false,
        Header::Array(None) => true,
        _ => {
            return Err(Error::new(
                "CBOR decimal fraction requires [exponent, coefficient]",
            ))
        }
    };
    let exponent = i64::try_from(integer(reader)?)
        .map_err(|_| Error::new("CBOR decimal exponent is outside i64 representation"))?;
    let coefficient = integer(reader)?;
    if indefinite && reader.pull().map_err(wire_error)? != Header::Break {
        return Err(Error::new(
            "CBOR decimal fraction requires exactly two components",
        ));
    }
    Number::from_parts(coefficient, exponent)
}

enum Frame {
    Sequence(Option<usize>, Vec<Document>),
    Mapping(Option<usize>, Vec<(Document, Document)>, Option<Document>),
}

impl Frame {
    fn finish(self) -> Result<Document, Error> {
        match self {
            Self::Sequence(None | Some(0), values) => Ok(Document::Sequence(values)),
            Self::Mapping(None | Some(0), values, None) => Ok(
                if values.iter().all(|(k, _)| matches!(k, Document::Text(_))) {
                    Document::Mapping(
                        values
                            .into_iter()
                            .map(|(k, v)| {
                                let Document::Text(k) = k else { unreachable!() };
                                (k, v)
                            })
                            .collect(),
                    )
                } else {
                    Document::Map(values)
                },
            ),
            _ => Err(Error::new("unexpected CBOR break or incomplete collection")),
        }
    }
}

/// Decode exactly one CBOR item. No application size or nesting ceiling is imposed.
/// Parsing uses an explicit collection stack rather than recursive descent.
/// Available memory and the host's address space still bound the operation.
pub fn decode(input: &[u8]) -> Result<Document, Error> {
    let mut reader = Reader::from(input);
    let mut stack = Vec::new();
    loop {
        let start = reader.offset();
        let header = reader.pull().map_err(wire_error)?;
        let mut item = match header {
            Header::Positive(value) => Document::Number(Number::from_parts(value.into(), 0)?),
            Header::Negative(value) => {
                Document::Number(Number::from_parts(-BigInt::from(value) - 1u8, 0)?)
            }
            Header::Simple(20) => Document::Bool(false),
            Header::Simple(21) => Document::Bool(true),
            Header::Simple(22) => Document::Null,
            Header::Simple(_) => return Err(Error::new("unsupported CBOR simple value")),
            Header::Float(_) => {
                let width = match input[start] {
                    0xf9 => 16,
                    0xfa => 32,
                    0xfb => 64,
                    _ => unreachable!(),
                };
                let mut bits = 0u64;
                for byte in &input[start + 1..reader.offset()] {
                    bits = (bits << 8) | u64::from(*byte);
                }
                Document::Float(BinaryFloat { width, bits })
            }
            Header::Bytes(length) => Document::Bytes(bytes(&mut reader, length)?),
            Header::Text(length) => Document::Text(text(&mut reader, length)?),
            Header::Tag(2 | 3) => Document::Number(Number::from_parts(
                big_integer(&mut reader, header == Header::Tag(3))?,
                0,
            )?),
            Header::Tag(4) => Document::Number(decimal(&mut reader)?),
            Header::Tag(tag) => return Err(Error::new(format!("unsupported CBOR tag {tag}"))),
            Header::Array(Some(0)) => Document::Sequence(Vec::new()),
            Header::Map(Some(0)) => Document::Mapping(BTreeMap::new()),
            Header::Array(length) => {
                stack.push(Frame::Sequence(length, Vec::new()));
                continue;
            }
            Header::Map(length) => {
                stack.push(Frame::Mapping(length, Vec::new(), None));
                continue;
            }
            Header::Break => match stack.pop() {
                Some(frame @ (Frame::Sequence(None, _) | Frame::Mapping(None, _, None))) => {
                    frame.finish()?
                }
                _ => return Err(Error::new("unexpected CBOR break")),
            },
        };
        loop {
            let done = match stack.last_mut() {
                None => {
                    if reader.offset() != input.len() {
                        return Err(Error::new("trailing bytes after CBOR document"));
                    }
                    return Ok(item);
                }
                Some(Frame::Sequence(remaining, values)) => {
                    values.push(item);
                    if let Some(remaining) = remaining {
                        *remaining -= 1;
                        *remaining == 0
                    } else {
                        false
                    }
                }
                Some(Frame::Mapping(remaining, values, key)) => {
                    if let Some(key) = key.take() {
                        values.push((key, item));
                        if let Some(remaining) = remaining {
                            *remaining -= 1;
                            *remaining == 0
                        } else {
                            false
                        }
                    } else {
                        if values.iter().any(|(k, _)| key_equal(k, &item)) {
                            return Err(Error::new("duplicate CBOR map key"));
                        }
                        *key = Some(item);
                        false
                    }
                }
            };
            if !done {
                break;
            }
            item = stack.pop().expect("completed frame exists").finish()?;
        }
    }
}

// Compare semantic float keys across IEEE widths without converting stored values.
fn float_value(v: &BinaryFloat) -> f64 {
    let mut wire = vec![match v.width {
        16 => 0xf9,
        32 => 0xfa,
        _ => 0xfb,
    }];
    wire.extend_from_slice(&v.bits.to_be_bytes()[8 - usize::from(v.width) / 8..]);
    match Decoder::from(wire.as_slice()).pull().unwrap() {
        Header::Float(v) => v,
        _ => unreachable!(),
    }
}
fn key_equal(a: &Document, b: &Document) -> bool {
    match (a, b) {
        (Document::Float(a), Document::Float(b)) => {
            if a.validate().is_err() || b.validate().is_err() {
                return false;
            }
            fn nan_significand(v: &BinaryFloat) -> Option<u64> {
                let (fraction, exponent) = match v.width {
                    16 => (10, 5),
                    32 => (23, 8),
                    _ => (52, 11),
                };
                let mask = (1u64 << fraction) - 1;
                let f = v.bits & mask;
                (((v.bits >> fraction) & ((1u64 << exponent) - 1)) == ((1u64 << exponent) - 1)
                    && f != 0)
                    .then_some(f << (64 - fraction))
            }
            match (nan_significand(a), nan_significand(b)) {
                (Some(a), Some(b)) => a == b,
                (Some(_), _) | (_, Some(_)) => false,
                _ => float_value(a) == float_value(b),
            }
        }
        (Document::Sequence(a), Document::Sequence(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| key_equal(a, b))
        }
        (Document::Mapping(a), Document::Mapping(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|w| key_equal(v, w)))
        }
        (Document::Map(a), Document::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.iter().any(|(l, w)| key_equal(k, l) && key_equal(v, w)))
        }
        (Document::Map(a), Document::Mapping(b)) | (Document::Mapping(b), Document::Map(a)) => {
            a.len() == b.len()
                && a.iter().all(|(k, v)| match k {
                    Document::Text(k) => b.get(k).is_some_and(|w| key_equal(v, w)),
                    _ => false,
                })
        }
        _ => a == b,
    }
}

fn emit_integer(writer: &mut Writer<'_>, value: &BigInt) -> Result<(), Error> {
    let (negative, magnitude) = if value.sign() == Sign::Minus {
        (true, -value - 1u8)
    } else {
        (false, value.clone())
    };
    if let Ok(small) = u64::try_from(&magnitude) {
        writer
            .push(if negative {
                Header::Negative(small)
            } else {
                Header::Positive(small)
            })
            .map_err(wire_error)
    } else {
        writer
            .push(Header::Tag(if negative { 3 } else { 2 }))
            .map_err(wire_error)?;
        writer
            .bytes(&magnitude.to_bytes_be().1, None)
            .map_err(wire_error)
    }
}

/// Encode the exact document without float conversion. Map keys are emitted in
/// Rust string order, not RFC 8949 length-first canonical order.
pub fn encode(document: &Document) -> Result<Vec<u8>, Error> {
    enum Pending<'a> {
        Value(&'a Document),
        Key(&'a str),
    }
    let mut out = Vec::new();
    let mut pending = vec![Pending::Value(document)];
    while let Some(item) = pending.pop() {
        let mut writer = Writer::from(&mut out);
        let document = match item {
            Pending::Value(value) => value,
            Pending::Key(key) => {
                writer.text(key, None).map_err(wire_error)?;
                continue;
            }
        };
        match document {
            Document::Float(value) => {
                value.validate()?;
                drop(writer);
                out.push(match value.width {
                    16 => 0xf9,
                    32 => 0xfa,
                    _ => 0xfb,
                });
                out.extend_from_slice(
                    &value.bits.to_be_bytes()[8 - usize::from(value.width) / 8..],
                );
            }
            Document::Map(values) => {
                for (i, (key, _)) in values.iter().enumerate() {
                    if values[..i].iter().any(|(k, _)| key_equal(k, key)) {
                        return Err(Error::new("duplicate CBOR map key"));
                    }
                }
                writer
                    .push(Header::Map(Some(values.len())))
                    .map_err(wire_error)?;
                for (key, value) in values.iter().rev() {
                    pending.push(Pending::Value(value));
                    pending.push(Pending::Value(key));
                }
            }
            Document::Null => writer.push(Header::Simple(22)).map_err(wire_error)?,
            Document::Bool(value) => writer
                .push(Header::Simple(if *value { 21 } else { 20 }))
                .map_err(wire_error)?,
            Document::Text(value) => writer.text(value, None).map_err(wire_error)?,
            Document::Bytes(value) => writer.bytes(value, None).map_err(wire_error)?,
            Document::Number(value) => {
                // Normalization may turn 100 into 1e2. Preserve native integer
                // interoperability whenever the value fits CBOR's integer range.
                // 19 is derived from u64's decimal range, not an admission cap.
                if (1..=19).contains(&value.exponent()) {
                    let whole =
                        value.coefficient() * BigInt::from(10u8).pow(value.exponent() as u32);
                    let magnitude = if whole.sign() == Sign::Minus {
                        -&whole - 1u8
                    } else {
                        whole.clone()
                    };
                    if u64::try_from(&magnitude).is_ok() {
                        emit_integer(&mut writer, &whole)?;
                        continue;
                    }
                }
                if value.exponent() != 0 {
                    writer.push(Header::Tag(4)).map_err(wire_error)?;
                    writer.push(Header::Array(Some(2))).map_err(wire_error)?;
                    emit_integer(&mut writer, &BigInt::from(value.exponent()))?;
                }
                emit_integer(&mut writer, value.coefficient())?;
            }
            Document::Sequence(values) => {
                writer
                    .push(Header::Array(Some(values.len())))
                    .map_err(wire_error)?;
                pending.extend(values.iter().rev().map(Pending::Value));
            }
            Document::Mapping(values) => {
                writer
                    .push(Header::Map(Some(values.len())))
                    .map_err(wire_error)?;
                for (key, value) in values.iter().rev() {
                    pending.push(Pending::Value(value));
                    pending.push(Pending::Key(key));
                }
            }
        }
    }
    Ok(out)
}

/// Stateless, reusable CBOR adapter for generic host workflows.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cbor;
impl crate::Adapter for Cbor {
    fn decode(&self, input: &[u8]) -> Result<Document, Error> {
        decode(input)
    }
    fn encode(&self, document: &Document) -> Result<Vec<u8>, Error> {
        encode(document)
    }
}
