//! Opt-in host adapters, separate from VERA's language, verifier and persistence.
//!
//! Formats share one exact data model. Implement [`Adapter`] for another transport
//! without teaching the compiler about it. The optional `vera` feature bridges
//! this model to checked VERA types; it does not prove the external parser.
use num_bigint::BigInt;
use num_traits::Zero;
use std::{collections::BTreeMap, fmt, str::FromStr};

#[cfg(feature = "cbor")]
pub mod cbor;
#[cfg(feature = "json")]
pub mod json;
#[cfg(feature = "vera")]
pub mod vera;
#[cfg(feature = "yaml")]
pub mod yaml;

/// Exact transport model. Mapping preserves the shared text-keyed subset;
/// Map and Float preserve CBOR values that require explicit cross-format conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Document {
    Float(BinaryFloat),
    Map(Vec<(Document, Document)>),
    Null,
    Bool(bool),
    Number(Number),
    Text(String),
    Bytes(Vec<u8>),
    Sequence(Vec<Document>),
    Mapping(BTreeMap<String, Document>),
}

/// Raw IEEE-754 representation. Width and payload are preserved, including NaNs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinaryFloat {
    pub width: u8,
    pub bits: u64,
}
impl BinaryFloat {
    pub fn validate(&self) -> Result<(), Error> {
        match self.width {
            16 if self.bits <= u16::MAX as u64 => Ok(()),
            32 if self.bits <= u32::MAX as u64 => Ok(()),
            64 => Ok(()),
            _ => Err(Error::new(
                "binary float needs width 16/32/64 and fitting bits",
            )),
        }
    }
}

/// An exact finite base-ten number: coefficient times ten to exponent.
///
/// Exponents must fit i64. No binary floating-point conversion is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Number {
    coefficient: BigInt,
    exponent: i64,
}
impl Number {
    pub fn from_parts(coefficient: BigInt, exponent: i64) -> Result<Self, Error> {
        if coefficient.is_zero() {
            return Ok(Self {
                coefficient,
                exponent: 0,
            });
        }
        let digits = coefficient.to_string();
        let trimmed = digits.trim_end_matches('0');
        let count = digits.len() - trimmed.len();
        let exponent = exponent
            .checked_add(
                i64::try_from(count).map_err(|_| Error::new("number exponent exceeds i64"))?,
            )
            .ok_or_else(|| Error::new("number exponent exceeds i64"))?;
        let coefficient = if count == 0 {
            coefficient
        } else {
            BigInt::from_str(trimmed).map_err(|_| Error::new("invalid coefficient"))?
        };
        Ok(Self {
            coefficient,
            exponent,
        })
    }
    /// Parse the JSON numeric grammar, preserving arbitrary integer precision.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let invalid = || Error::new("expected an exact finite JSON number");
        let (negative, rest) = text.strip_prefix('-').map_or((false, text), |s| (true, s));
        let (mantissa, exponent) = match rest.find(['e', 'E']) {
            Some(i) => {
                let exp = &rest[i + 1..];
                let digits = exp.strip_prefix(['+', '-']).unwrap_or(exp);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(invalid());
                }
                (
                    &rest[..i],
                    exp.parse::<i64>()
                        .map_err(|_| Error::new("number exponent exceeds i64"))?,
                )
            }
            None => (rest, 0),
        };
        let (whole, fraction) = mantissa
            .split_once('.')
            .map_or((mantissa, None), |(a, b)| (a, Some(b)));
        if whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || (whole.len() > 1 && whole.starts_with('0'))
        {
            return Err(invalid());
        }
        let fraction = match fraction {
            Some(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => s,
            Some(_) => return Err(invalid()),
            None => "",
        };
        let exponent = exponent
            .checked_sub(i64::try_from(fraction.len()).map_err(|_| invalid())?)
            .ok_or_else(|| Error::new("number exponent exceeds i64"))?;
        let mut digits =
            String::with_capacity(whole.len() + fraction.len() + usize::from(negative));
        if negative {
            digits.push('-');
        }
        digits.push_str(whole);
        digits.push_str(fraction);
        let trimmed = digits.trim_end_matches('0');
        // Normalize textual trailing zeroes before constructing a large BigInt.
        if trimmed.is_empty() || trimmed == "-" {
            return Self::from_parts(BigInt::zero(), 0);
        }
        let count = digits.len() - trimmed.len();
        let exponent = exponent
            .checked_add(i64::try_from(count).map_err(|_| invalid())?)
            .ok_or_else(|| Error::new("number exponent exceeds i64"))?;
        Self::from_parts(BigInt::from_str(trimmed).map_err(|_| invalid())?, exponent)
    }
    pub fn coefficient(&self) -> &BigInt {
        &self.coefficient
    }
    pub fn exponent(&self) -> i64 {
        self.exponent
    }
}
impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.coefficient)?;
        if self.exponent != 0 {
            write!(f, "e{}", self.exponent)?;
        }
        Ok(())
    }
}
impl FromStr for Number {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A boundary error, with a JSON-pointer-like path from the document root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: String,
    path: String,
}
impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            path: String::new(),
        }
    }
    pub fn at(mut self, segment: impl AsRef<str>) -> Self {
        let segment = segment.as_ref().replace('~', "~0").replace('/', "~1");
        self.path = format!("/{segment}{}", self.path);
        self
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn path(&self) -> &str {
        &self.path
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {}",
            if self.path.is_empty() {
                "/"
            } else {
                &self.path
            },
            self.message
        )
    }
}
impl std::error::Error for Error {}

/// Small transport interface. Implementations are reusable, stateless boundaries.
pub trait Adapter {
    fn decode(&self, input: &[u8]) -> Result<Document, Error>;
    fn encode(&self, document: &Document) -> Result<Vec<u8>, Error>;
}

/// Convert through the shared data model. Unsupported destination values fail;
/// this function never silently drops bytes, tags, precision or mapping keys.
pub fn transcode(
    source: &dyn Adapter,
    destination: &dyn Adapter,
    input: &[u8],
) -> Result<Vec<u8>, Error> {
    destination.encode(&source.decode(input)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbers_are_exact_and_normalized() {
        assert_eq!(
            Number::parse("100.00e-2").unwrap(),
            Number::parse("1").unwrap()
        );
        assert_eq!(Number::parse("-0e999999").unwrap().to_string(), "0");
        assert_eq!(
            Number::parse("123456789012345678901234567890.123456789")
                .unwrap()
                .to_string(),
            "123456789012345678901234567890123456789e-9"
        );
        for s in [
            "",
            "+1",
            "01",
            ".1",
            "1.",
            "1e",
            "1e1e2",
            "NaN",
            "inf",
            "1e9223372036854775808",
        ] {
            assert!(Number::parse(s).is_err(), "{s}");
        }
    }
    #[test]
    fn long_trailing_zeroes_normalize_before_integer_construction() {
        let text = format!("1{}", "0".repeat(20_000));
        assert_eq!(Number::parse(&text).unwrap().to_string(), "1e20000");
    }
    #[test]
    fn error_path_escapes_segments() {
        assert_eq!(
            Error::new("bad").at("~/").at("payload").path(),
            "/payload/~0~1"
        );
    }
}
