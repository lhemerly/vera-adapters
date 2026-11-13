//! Typed VERA bridge. Decimal numbers are raw scaled coefficients, matching
//! VERA's existing host JSON output. Unlike the legacy fractional-number input
//! convenience, all numeric spellings denote coefficients; decimal text containing
//! a dot denotes human units.
use crate::{Adapter, Document, Error, Number};
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};
use std::collections::{BTreeMap, BTreeSet};
use vera_compiler::{
    ir::{CheckedProgram, Param, TypeDefinitionKind, VariantId},
    types::{ScalarType, ValueType},
    value::{self, Value},
};

/// Reusable binding of an external value to an already checked VERA schema.
///
/// Constructing this binding checks type identity, not program proof obligations.
/// Callers remain responsible for verifying their program and validating state.
pub struct Typed<'p> {
    program: &'p CheckedProgram,
    ty: ValueType,
}
impl<'p> Typed<'p> {
    pub fn new(program: &'p CheckedProgram, ty: ValueType) -> Result<Self, Error> {
        check_type(program, &ty)?;
        Ok(Self { program, ty })
    }
    pub fn decode(&self, adapter: &dyn Adapter, input: &[u8]) -> Result<Value, Error> {
        self.from_document(&adapter.decode(input)?)
    }
    pub fn encode(&self, adapter: &dyn Adapter, value: &Value) -> Result<Vec<u8>, Error> {
        adapter.encode(&self.to_document(value)?)
    }
    pub fn from_document(&self, document: &Document) -> Result<Value, Error> {
        from_document(self.program, &self.ty, document)
    }
    pub fn to_document(&self, value: &Value) -> Result<Document, Error> {
        to_document(self.program, &self.ty, value)
    }
}

fn check_type(program: &CheckedProgram, ty: &ValueType) -> Result<(), Error> {
    match ty {
        ValueType::Named(id) if program.types().get(id.0).is_none() => {
            Err(Error::new("unknown nominal type identity"))
        }
        ValueType::Scalar(ScalarType::Id64(id)) if program.tables().get(id.0).is_none() => {
            Err(Error::new("unknown handle table identity"))
        }
        ValueType::Scalar(ScalarType::Enum(tags))
            if tags.is_empty()
                || tags.len() > 256
                || tags.iter().collect::<BTreeSet<_>>().len() != tags.len() =>
        {
            Err(Error::new(
                "enum domain requires 1 through 256 distinct tags (u8 representation)",
            ))
        }
        _ => Ok(()),
    }
}

// Test decimal magnitude before constructing a power. Untrusted exponents may
// be enormous; VERA scalar domains need at most 78 digits, never that allocation.
fn bounded_integer(number: &Number, ty: &ScalarType) -> Result<BigInt, Error> {
    if number.exponent() < 0 {
        return Err(Error::new("an exact integral coefficient is required"));
    }
    let digits = number.coefficient().abs().to_string().len();
    let bound_digits = ty
        .min_value()
        .abs()
        .to_string()
        .len()
        .max(ty.max_value().to_string().len());
    let exponent = usize::try_from(number.exponent())
        .map_err(|_| Error::new(format!("value is outside {ty}")))?;
    if !number.coefficient().is_zero()
        && (exponent > bound_digits || digits > bound_digits - exponent)
    {
        return Err(Error::new(format!("value is outside {ty}")));
    }
    let exponent =
        u32::try_from(exponent).map_err(|_| Error::new(format!("value is outside {ty}")))?;
    let value = number.coefficient() * BigInt::from(10u8).pow(exponent);
    value::coerce(&Value::Int(value.clone()), ty).map_err(Error::new)?;
    Ok(value)
}

fn from_document(
    program: &CheckedProgram,
    ty: &ValueType,
    document: &Document,
) -> Result<Value, Error> {
    match ty {
        ValueType::Scalar(scalar) => match document {
            Document::Number(number) if !scalar.is_bool() => {
                Ok(Value::Int(bounded_integer(number, scalar)?))
            }
            Document::Bool(boolean) if scalar.is_bool() => Ok(Value::Bool(*boolean)),
            Document::Text(text) if !scalar.is_bool() => {
                value::from_json(scalar, &serde_json::Value::String(text.clone()))
                    .map_err(Error::new)
            }
            _ => Err(Error::new(format!("invalid external value for {scalar}"))),
        },
        ValueType::Bytes { capacity } => {
            let bytes = match document {
                Document::Bytes(bytes) => bytes.clone(),
                Document::Text(text) => {
                    let hex = text.strip_prefix("0x").ok_or_else(|| {
                        Error::new("bytes require a 0x-prefixed hexadecimal string")
                    })?;
                    if hex.len() % 2 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(Error::new("invalid hexadecimal bytes"));
                    }
                    if (hex.len() / 2) as u128 > u128::from(*capacity) {
                        return Err(Error::new("byte length exceeds capacity"));
                    }
                    hex.as_bytes()
                        .chunks_exact(2)
                        .map(|pair| {
                            // ASCII hex was checked before slicing.
                            u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII hex"), 16)
                                .expect("checked hex")
                        })
                        .collect()
                }
                _ => {
                    return Err(Error::new(
                        "bytes require hexadecimal text or a native byte string",
                    ))
                }
            };
            if bytes.len() as u128 > u128::from(*capacity) {
                return Err(Error::new("byte length exceeds capacity"));
            }
            Ok(Value::Bytes(bytes))
        }
        ValueType::Named(id) => {
            let definition = &program.types()[id.0];
            let Document::Mapping(fields) = document else {
                return Err(Error::new("record or union requires a mapping"));
            };
            match &definition.kind {
                TypeDefinitionKind::Record(defs) => {
                    let known: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
                    exact_fields(fields, &known)?;
                    Ok(Value::Record {
                        ty: *id,
                        fields: defs
                            .iter()
                            .map(|d| {
                                from_document(program, &d.ty, &fields[&d.name])
                                    .map_err(|e| e.at(&d.name))
                            })
                            .collect::<Result<_, _>>()?,
                    })
                }
                TypeDefinitionKind::Union(defs) => {
                    exact_fields(fields, &["tag", "payload"])?;
                    let Document::Text(tag) = &fields["tag"] else {
                        return Err(Error::new("union tag must be text").at("tag"));
                    };
                    let variant = defs
                        .iter()
                        .find(|d| d.name == *tag)
                        .ok_or_else(|| Error::new("unknown union tag").at("tag"))?;
                    Ok(Value::Variant {
                        ty: *id,
                        tag: variant.id,
                        payload: Box::new(
                            from_document(program, &variant.payload, &fields["payload"])
                                .map_err(|e| e.at("payload"))?,
                        ),
                    })
                }
            }
        }
    }
}

fn exact_fields(fields: &BTreeMap<String, Document>, expected: &[&str]) -> Result<(), Error> {
    for field in fields.keys() {
        if !expected.contains(&field.as_str()) {
            return Err(Error::new("unknown field").at(field));
        }
    }
    for field in expected {
        if !fields.contains_key(*field) {
            return Err(Error::new("missing field").at(field));
        }
    }
    Ok(())
}

fn to_document(program: &CheckedProgram, ty: &ValueType, value: &Value) -> Result<Document, Error> {
    match (ty, value) {
        (ValueType::Scalar(scalar), _) => {
            value::coerce(value, scalar).map_err(Error::new)?;
            if let Value::Bool(b) = value {
                return Ok(Document::Bool(*b));
            }
            let number = value.int().map_err(Error::new)?;
            match scalar {
                ScalarType::Bytes32 => {
                    let (_, mut bytes) = number.to_bytes_le();
                    bytes.resize(32, 0);
                    Ok(Document::Text(hex_text(&bytes)))
                }
                ScalarType::Enum(tags) => Ok(Document::Text(
                    tags[number
                        .to_usize()
                        .ok_or_else(|| Error::new("invalid enum tag"))?]
                    .clone(),
                )),
                _ => Ok(Document::Number(Number::from_parts(number.clone(), 0)?)),
            }
        }
        (ValueType::Bytes { capacity }, Value::Bytes(bytes))
            if bytes.len() as u128 <= u128::from(*capacity) =>
        {
            Ok(Document::Text(hex_text(bytes)))
        }
        (ValueType::Named(id), Value::Record { ty: actual, fields }) if id == actual => {
            let TypeDefinitionKind::Record(defs) = &program.types()[id.0].kind else {
                return Err(Error::new("value is not this nominal record"));
            };
            if defs.len() != fields.len() {
                return Err(Error::new("record field count differs from declaration"));
            }
            Ok(Document::Mapping(
                defs.iter()
                    .zip(fields)
                    .map(|(d, v)| {
                        Ok((
                            d.name.clone(),
                            to_document(program, &d.ty, v).map_err(|e| e.at(&d.name))?,
                        ))
                    })
                    .collect::<Result<_, Error>>()?,
            ))
        }
        (
            ValueType::Named(id),
            Value::Variant {
                ty: actual,
                tag: VariantId(tag),
                payload,
            },
        ) if id == actual => {
            let TypeDefinitionKind::Union(defs) = &program.types()[id.0].kind else {
                return Err(Error::new("value is not this nominal union"));
            };
            let def = defs
                .get(*tag)
                .ok_or_else(|| Error::new("unknown union tag").at("tag"))?;
            Ok(Document::Mapping(BTreeMap::from([
                ("tag".into(), Document::Text(def.name.clone())),
                (
                    "payload".into(),
                    to_document(program, &def.payload, payload).map_err(|e| e.at("payload"))?,
                ),
            ])))
        }
        _ => Err(Error::new("value does not match its declared VERA type")),
    }
}
fn hex_text(bytes: &[u8]) -> String {
    format!(
        "0x{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// Decode a transition, operation, function or effect's whole parameter mapping.
/// Unknown/missing parameters fail before the caller can execute anything.
pub fn decode_params(
    adapter: &dyn Adapter,
    program: &CheckedProgram,
    params: &[Param],
    input: &[u8],
) -> Result<BTreeMap<String, Value>, Error> {
    check_params(params)?;
    let document = adapter.decode(input)?;
    let Document::Mapping(fields) = document else {
        return Err(Error::new("parameters require a mapping"));
    };
    let expected: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    exact_fields(&fields, &expected)?;
    params
        .iter()
        .map(|p| {
            Ok((
                p.name.clone(),
                Typed::new(program, p.ty.clone())?
                    .from_document(&fields[&p.name])
                    .map_err(|e| e.at(&p.name))?,
            ))
        })
        .collect()
}

/// Encode parameter values using the same portable typed representation.
pub fn encode_params(
    adapter: &dyn Adapter,
    program: &CheckedProgram,
    params: &[Param],
    values: &BTreeMap<String, Value>,
) -> Result<Vec<u8>, Error> {
    check_params(params)?;
    for name in values.keys() {
        if !params.iter().any(|p| p.name == *name) {
            return Err(Error::new("unknown parameter").at(name));
        }
    }
    let fields = params
        .iter()
        .map(|p| {
            let value = values
                .get(&p.name)
                .ok_or_else(|| Error::new("missing parameter").at(&p.name))?;
            Ok((
                p.name.clone(),
                Typed::new(program, p.ty.clone())?
                    .to_document(value)
                    .map_err(|e| e.at(&p.name))?,
            ))
        })
        .collect::<Result<_, Error>>()?;
    adapter.encode(&Document::Mapping(fields))
}

fn check_params(params: &[Param]) -> Result<(), Error> {
    let mut names = BTreeSet::new();
    for param in params {
        if !names.insert(&param.name) {
            return Err(Error::new("duplicate parameter schema name").at(&param.name));
        }
    }
    Ok(())
}
