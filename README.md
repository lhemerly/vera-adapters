# VERA adapters

Reusable Rust adapters for JSON, CBOR and YAML, with a standard-library Python interface and optional typed admission into [VERA](https://github.com/lhemerly/vera).

The format adapters do **not** require VERA, Z3 or a model. Parsing is not formally verified. VERA proofs apply only to the declared program properties, not the external parser or application intent.

## Rust

```toml
[dependencies]
vera-adapters = { git = "https://github.com/lhemerly/vera-adapters", default-features = false, features = ["json", "cbor", "yaml"] }
```

Choose formats independently. No crates.io publication is implied.

```rust
use vera_adapters::{Adapter, json::Json, cbor::Cbor};
let value = Json.decode(br#"{"count":18446744073709551616}"#)?;
let wire = Cbor.encode(&value)?;
assert_eq!(Cbor.decode(&wire)?, value);
# Ok::<(), vera_adapters::Error>(())
```

`Document::Number` preserves exact decimal coefficients and exponents. `Document::Mapping` is the text-keyed shared subset. CBOR also supports `Document::Map` for arbitrary typed keys and `Document::Float(BinaryFloat { width, bits })` for exact IEEE16/32/64 representations, including signed zero, infinities and NaN payloads.

## Python

```sh
cargo build --locked --release -p vera-adapters --example python_bridge
PYTHONPATH=python python python/example.py
PYTHONPATH=python python -m unittest discover -s python -v
```

```python
from vera_adapters import AdapterClient, BinaryFloat, MapEntries
client = AdapterClient()  # finds this checkout's built bridge
wire = client.encode("cbor", MapEntries([(1, BinaryFloat(16, 0x8000))]))
value = client.decode("cbor", wire)
```

Read [the Python interface](python/README.md) for exact-number wrappers, arbitrary map keys, process boundaries and distribution.

## Profiles and limits

Read [FORMAT_ADAPTERS.md](docs/FORMAT_ADAPTERS.md) before replacing a parser. These are deliberate transport profiles, not full implementations of every extension:

- JSON rejects duplicate keys and preserves exact numbers.
- CBOR supports arbitrary typed map keys and raw IEEE floats. Unknown tags and unsupported simple values remain explicit errors. Existing bignum/decimal tags normalize into exact numbers rather than preserving tag identity.
- YAML supports the documented core-style profile, not aliases, arbitrary tags or complex keys.
- Conversions that would discard type or precision fail rather than silently coercing values. JSON/YAML require explicit conversion of CBOR-only values.
- No application size/depth budgets are imposed. Host memory, stack and parser implementation limits still apply. General map-key comparisons can be quadratic and nested comparisons recursive.
- Floating tokens preserve width and bits; whole-document byte layout and canonical CBOR are not promised.

## Optional VERA integration

The `vera` feature adds typed admission through a pinned VERA compiler dependency. It pins public VERA revision `e62e66e51332a3af53700f74e7799ea68b003e1d` and requires [native Z3 setup from that revision](https://github.com/lhemerly/vera/blob/e62e66e51332a3af53700f74e7799ea68b003e1d/tools/setup_z3.sh). The default format-only build does not compile the compiler or need Z3. `typed_pipeline` demonstrates verified-core integration; admission alone does not prove initial state validity, authorization or business preconditions.

## Development

```sh
cargo test --locked -p vera-adapters
cargo test --locked -p vera-adapters --no-default-features --features cbor
cargo test --locked -p vera-adapters --example python_bridge
cargo fmt --all -- --check
```

Tests include all 65,536 IEEE binary16 patterns, binary32/64 edge cases, RFC 8949 map-key equivalence, malformed/truncated inputs, exact-number round trips and real Python process calls. Add `--offline` once dependencies are cached; alternatively use `cargo vendor` and its generated source-replacement configuration for an offline distribution.

## Provenance and license

Extracted from VERA's modular-format-adapters work with the general CBOR key/float extension. MIT licensed; see [LICENSE](LICENSE). Dependency licenses are recorded in their Cargo metadata. No model, benchmark harness, hidden tests or credentials are included.
