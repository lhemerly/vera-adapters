# Python host interface (offline, no Z3)

This standard-library Python module calls the existing Rust format adapters via
one process per operation. It does not reimplement JSON, CBOR or YAML parsing in
Python, add protocol variants, prove parsing correct or run a VERA program.
The tagged process envelope is private interchange, not an application protocol.

## Build once, then use

From the repository root, build the bridge explicitly:

```sh
cargo build --locked --release -p vera-adapters --example python_bridge
PYTHONPATH=python python python/example.py
PYTHONPATH=python python -m unittest discover -s python -v
```

Add `--offline` when dependencies are cached, or vendor them with `cargo vendor`
and use its generated configuration. No Python package installation, implicit
build, credential or solver is needed. The build selects the three format
features, not the optional VERA compiler bridge.

```python
from vera_adapters import AdapterClient, ExactNumber

client = AdapterClient("/absolute/path/to/vera-adapters/target/release/examples/python_bridge")
value = client.decode("json", b'{"count":18446744073709551616}')
assert value == {"count": ExactNumber("18446744073709551616")}
wire = client.encode("cbor", value)
assert client.decode("cbor", wire) == value
text = client.transcode("cbor", "yaml", wire)
```

`decode(format, bytes)` returns `None`, `bool`, `ExactNumber`, `str`, `bytes`,
`list`, a string-keyed `dict`, `BinaryFloat`, or `MapEntries`. Decimal/integer
numbers use `ExactNumber(coefficient, exponent)`, representing `coefficient * 10**exponent` without float conversion or
expansion. The coefficient is a decimal string; the exponent fits signed i64.
Normalization matches Rust. Large coefficient strings avoid Python's default
integer-string digit ceiling. Hosts choose any later narrowing and range checks.

`encode(format, value)` accepts those types and Python `int` and `float`.
A native float is encoded as its binary64 bit representation, not an exact decimal.
CBOR binary floats decode to `BinaryFloat(width, bits)` (16, 32 or 64 bits),
preserving width, signed zero, infinities, and NaN sign/payload/signaling bits.
Equality is bitwise representation equality, not floating-point numeric equality.
Use `MapEntries([(key, value), ...])` for arbitrary CBOR keys, including arrays,
maps, and floats. A Python dict must still have text keys: Python merges boolean
and integer keys and cannot hold unhashable keys. Text-only CBOR maps still decode
to dict. This means a text-only MapEntries may decode to dict after a roundtrip.
JSON/YAML destinations reject binary floats and general maps rather than silently
coercing them. Explicit conversion belongs to the caller. Very large
Python ints may hit Python's integer-string conversion limit; pass an
`ExactNumber` string instead. Native CBOR bytes remain bytes; generic JSON/YAML
cannot encode them without an explicit application mapping. Nothing silently
maps arbitrary bytes to text. `transcode(source, destination, bytes)` avoids
Python value conversion entirely. Errors raise `AdapterError`; missing binaries
raise `FileNotFoundError`; deadlines raise `subprocess.TimeoutExpired`.

## Evaluate compatibility before adopting

Read `../docs/FORMAT_ADAPTERS.md` and independently test representative accepted
and rejected wire samples. The existing profiles reject, for example, unknown
CBOR tags and YAML aliases/merge keys.
Do not force this interface onto a protocol requiring those features. Use a
compatible boundary or keep the required host parser and explain the mismatch.
Semantic roundtrips do not preserve comments, whitespace, original numeric
spelling, CBOR layout or a protocol's canonical/signature bytes. Transport-only
conversion is not typed VERA admission, initial-state validation, authorization,
verified host facts or a connection to generated application structs.

The wrapper uses an explicit absolute executable path, argv without a shell,
binary stdin/stdout and checked exit status. `timeout=30` is caller-configurable
(including `None`); a subprocess deadline is not a proof timeout. It buffers
input/output and has no hard memory/depth ceiling. The bridge's tagged-model
walks are recursive. Hosts must impose budgets/isolation for hostile inputs.
There is process-start overhead; no throughput guarantee is made.

The CLI accepts `decode FORMAT`, `encode FORMAT`, or `transcode SOURCE
DESTINATION`, with exactly one raw input document on stdin. Decode output and
encode input use recursive objects with exactly `kind` and `value`. Kinds are
`null`, `bool`, `number` (exact JSON-number text), `text`, `bytes` (hex without
prefix), `sequence` (tagged array), `mapping` (text keys to tagged values), `float`
(`[width-as-decimal-text, fixed-width-hex-bits]`), and `map` (array of pairs of
tagged keys and tagged values). Float hex has exactly width/4 digits, no prefix.
Failures exit 2 with stderr and no success bytes; successful output is raw bytes.
No line framing or extra logging is written to stdout.

For a standalone application, copy `python/vera_adapters.py`, the bridge source,
the adapter crate and required vendored dependencies; build and ship your own
binary and use its absolute path. Do not depend on this plugin at runtime. Keep
source/license provenance. Tests exercise the real process, exact large numbers,
CBOR known wire vectors, all three roundtrips, bytes, malformed documents and
intentional profile rejection. No model or benchmark jobs are involved.
