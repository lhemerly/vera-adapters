# Modular JSON, CBOR and YAML adapters

`rust/vera-adapters` is an optional host-boundary crate. It does not add formats
to the language, change the verifier, replace persistent canonical codecs, or
include any application protocol or benchmark logic. Hosts choose a transport,
perform typed admission, then call their existing VERA workflow. A format can
also be used independently, without VERA execution.

## Dependencies and integration

The default features enable `json`, `cbor` and `yaml`. Each format can be selected
independently; `--no-default-features --features yaml` needs neither the other
format implementations nor Z3. The separate `vera` feature enables the bridge
to `vera-compiler`, whose verifier dependency requires the repository's native
Z3 setup. Adding the adapters does not add format dependencies to the compiler
or generated standalone Rust. The bridge targets the compiler/interpreter's
`Value` and checked type arena, not generated application-specific Rust structs.

```toml
[dependencies]
vera-adapters = { git = "https://github.com/lhemerly/vera-adapters", default-features = false, features = ["json", "yaml"] }
```

Format modules expose `decode(&[u8]) -> Result<Document, Error>` and
`encode(&Document) -> Result<Vec<u8>, Error>`. Their stateless `Json`, `Cbor` and
`Yaml` structs implement the small `Adapter` trait. Hosts can implement that
trait for another transport without changing the compiler.

```rust
use vera_adapters::{json::Json, yaml::Yaml, transcode};
let yaml = transcode(&Json, &Yaml, br#"{"enabled":true,"count":18446744073709551616}"#)?;
# Ok::<(), vera_adapters::Error>(())
```

`transcode` converts through the shared model. An unsupported destination value
is an error, never a lossy fallback. In particular, native CBOR byte strings do
not automatically become JSON or YAML text. An application must choose a byte
representation, or use the typed VERA bridge's portable hexadecimal mapping.

## Exact shared model

`Document` has null, boolean, exact number, text, bytes, sequence and mapping
variants. Mapping keys are unique strings; Map stores typed arbitrary key/value
pairs, and Float stores BinaryFloat width plus raw IEEE bits. Numbers retain a `BigInt` coefficient
and an `i64` base-ten exponent: `coefficient * 10^exponent`. Equivalent spellings
normalize to the same value; `1`, `1.0` and `100e-2` are equal. No binary floating
point conversion or machine-integer narrowing is used. Exponent representability
is explicit; an exponent outside the model produces an error.

Formats preserve semantic values, not original spelling, comments, key order,
whitespace, tag spelling or binary layout. Encoders are deterministic but are
not a signing/canonicalization specification. In particular, CBOR output is not
claimed to be RFC 8949 deterministic/canonical encoding for all profiles.

## Typed admission

With `features = ["vera"]`, `vera::Typed::new(&program, ty)` binds an adapter to
a checked VERA `ValueType`. Its `decode`/`encode` methods convert between external
bytes and `vera_compiler::value::Value`; `from_document`/`to_document` allow a host
to reuse an already parsed document. Records require exactly their declared
fields. Tagged unions require exactly `tag` and `payload`, with a known variant
and a correctly typed payload. Nominal identities, scalar domains, booleans and
bounded-byte capacities are checked in both directions.

Portable wire conventions are:

- Booleans are native booleans, not integers or string spellings.
- Integers are exact integral numbers; the existing scalar text forms remain
  available. Numeric fractions are not silently rounded or truncated.
- Decimal **numbers are integral scaled coefficients**, matching VERA's existing
  host JSON **output** convention. This bridge is stricter than the legacy
  `value_from_json` fractional-number input convenience: a host migrating that
  input must quote human-unit decimals explicitly. Equivalent numeric spellings
  `1`, `1.0` and `1e0` all denote coefficient 1 here. For `dec64` (scale 8), numeric `125000000` represents
  1.25 units. Numeric `1` represents one unit of the coefficient, not 1.0 units.
  Dotted **text** such as `"1.25"` expresses human decimal units and must be
  exactly representable. Fractional numeric `1.25` is rejected for that type.
  Output uses the coefficient representation, preserving exact typed roundtrips.
  `dec128` similarly uses its declared scale of 18.
- Enum output uses declared labels; admission also supports the compiler's
  existing enum scalar conventions. Unknown labels/out-of-domain values fail.
- `sym32` text retains the compiler's symbol-ID hashing convention; output is
  the numeric ID, not an attempted reconstruction of the original string.
- Bounded bytes use lowercase `0x`-prefixed hexadecimal text on output across
  all three formats. CBOR native byte strings are also admitted for `bytes<N>`.
  Odd/malformed hex and oversized values fail. `bytes32` remains its existing
  exact-width scalar hexadecimal convention, not a variable byte sequence.

`decode_params(adapter, program, params, input)` and `encode_params(...)` bind
an entire parameter mapping to a checked function, transition, operation or
effect signature. Unknown and missing names fail. Returned maps use parameter
names; positional runtime callers should order values by the checked signature,
not by map iteration order. Error paths are JSON-pointer-like escaped paths;
format errors may also carry source location information.

The complete runnable example parses records and a union, verifies a pure
identity function, admits JSON, executes the function, and checks typed JSON,
YAML and CBOR roundtrips. It also demonstrates parameter maps and independent
transport conversion:

```sh
cargo run -p vera-adapters --features vera --example typed_pipeline
cargo test -p vera-adapters --all-features
```

See [`typed_pipeline.rs`](../rust/vera-adapters/examples/typed_pipeline.rs).
The example explicitly checks every `verify_all` result before execution;
constructing `Typed` alone does not verify a program.

## Deliberate format profiles

**JSON:** exactly one JSON value; duplicate mapping keys, trailing content and
invalid numeric syntax fail. Arbitrary-precision numeric parsing preserves
integer and decimal precision. JSON has no native byte-string representation,
so generic `Document::Bytes` encoding fails rather than guessing one.

**CBOR:** exactly one item; arbitrary typed map keys, arrays, bytes, UTF-8 text, boolean,
null, integers, bignum tags 2/3, exact decimal-fraction tag 4 and IEEE binary16/32/64 floats are supported.
Definite and indefinite collections/strings are supported with the same
validation. Duplicate keys, invalid UTF-8/chunks, malformed
lengths, truncation and trailing items fail. Unknown tags,
undefined and unsupported simple values are rejected. Decimal fractions use
`[exponent, coefficient]`; the exponent must fit the shared model. Acceptance is
not a promise of compatibility with every CBOR protocol or schema.

**YAML:** one UTF-8 YAML 1.2 core-style document, with block/flow collections,
unique text keys and exact core numeric scalars. Quoting preserves strings;
`yes` and date-like text do not acquire YAML 1.1 boolean/timestamp meanings.
Core scalar/collection tags are checked. Anchors, aliases, unquoted merge keys,
complex/nonstring keys, nonfinite numbers, unsupported tags (including binary),
and multiple documents fail. A quoted literal `"<<"` key is ordinary text.
The encoder emits deterministic quoted-key block YAML, without aliases. Native
`Document::Bytes` needs an explicit host text mapping.

The upstream `saphyr-parser` 0.3.4 scanner represents flow nesting with `u8` and
rejects more than 255 flow levels. This is a reported parser implementation
restriction, not evidence that a VERA type is invalid. Block encoding avoids
that flow restriction; tests exercise 300 block levels. No adapter-wide depth,
byte-capacity, field-count or proof ceiling is imposed.

## Trust and resource boundaries

External parser libraries, adapter code, compiler/runtime implementation and
host integration remain part of the trusted implementation. SMT verification
covers the checked VERA program's obligations under its stated assumptions;
it does **not** prove these JSON/CBOR/YAML parsers, establish protocol
interoperability, or authenticate external input. Roundtrip and malformed-input
tests provide regression evidence, not universal parser proofs.

Typed admission establishes representability in the selected type, not a valid
initial state, current handle liveness, authorization, business preconditions,
checksums, signatures, replay protection or persistence compatibility. Continue
using the runtime's state validation, transaction preconditions and guards.
Unknown, failed or timed-out proofs must not be treated as success.

The absence of an adapter policy ceiling is not unlimited resources. Input and
output buffers, `BigInt`s and collection stacks require memory; allocation and
host address-space limits still apply. The shared recursive `Document` and VERA
values can also consume stack during cloning, equality, destruction and typed
conversion, even where format parsing/encoding uses explicit stacks. Hosts
handling hostile inputs should enforce their own transport/resource budgets and
isolation rather than interpreting larger accepted inputs as a safety promise.
No benchmark was run, and no throughput or constant-cost guarantee is made.

## Python and other process hosts

The reusable `python/vera_adapters.py` interface exposes `AdapterClient.decode`,
`encode` and `transcode` via the `python_bridge` Rust example, without the `vera`
feature or Z3. Numbers use exact coefficient/exponent objects; bytes are tagged
only in the internal process envelope, never silently changed on the wire.
See [python/README.md](../python/README.md) for build and offline preparation instructions, a concrete example, supported
Python values, error handling, independent tests and standalone packaging.
Build with `cargo build --locked --release -p vera-adapters --example
python_bridge`, then run `python python/example.py`.

Check the required wire profile before choosing this or the Rust API. Reuse a
compatible adapter at the real host boundary, or state the concrete mismatch and
keep the required parser. Format names alone do not establish compatibility.

## Binary floats and general CBOR keys

`Document::Float(BinaryFloat { width, bits })` preserves the IEEE width and raw
bit pattern, including signed zero, infinities, signaling NaNs and NaN payloads.
It is not silently converted into the exact decimal Number. `Document::Map`
stores typed entry pairs, avoiding host dictionary collisions and permitting
array/map keys. Text-only maps still decode to Mapping; explicit text-only Map
values normalize to Mapping on decode. Empty maps likewise become Mapping.
Float tokens retain byte identity, but entire CBOR documents do not: integer
widths, decimal tags, collection lengths and text map order may normalize.
Output is not RFC deterministic/canonical CBOR.

Key comparison follows RFC 8949 section 5.6.1 for supported types: integers and
floats remain distinct; float signed zeros compare equal; NaNs compare by
right-zero-extended significand regardless of sign; map order is irrelevant.
The existing exact-number profile normalizes bignums/decimal fractions into
Number, so applications needing tag identity must retain another parser.
Duplicate keys are rejected. No new size/depth/proof budget limits are added.
JSON/YAML and VERA typed admission do not silently accept these new variants:
choose an explicit application conversion or keep the host boundary unchanged.
Python uses BinaryFloat and MapEntries wrappers; see python/README.md.
