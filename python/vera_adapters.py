"""Exact offline JSON/CBOR/YAML via the Rust python_bridge example.

No Python dependencies, implicit builds, shell commands, model calls or Z3.
The format profiles are deliberately limited; read docs/FORMAT_ADAPTERS.md.
"""
from dataclasses import dataclass
from pathlib import Path
import json
import re
import subprocess
import struct

_FORMATS = frozenset(("json", "cbor", "yaml"))
_I64_MIN, _I64_MAX = -(2**63), 2**63 - 1


@dataclass(frozen=True)
class ExactNumber:
    """coefficient * 10**exponent, without binary floats or eager expansion.

    Coefficients are strings to avoid Python's integer-string digit ceiling.
    """
    coefficient: str
    exponent: int = 0

    def __post_init__(self):
        if not isinstance(self.coefficient, str) or not re.fullmatch(r"-?(0|[1-9][0-9]*)", self.coefficient):
            raise ValueError("coefficient must be a decimal integer string")
        if type(self.exponent) is not int or not _I64_MIN <= self.exponent <= _I64_MAX:
            raise ValueError("exponent must fit i64")
        coefficient = self.coefficient.rstrip("0")
        if coefficient in ("", "-"):
            coefficient, exponent = "0", 0
        else:
            exponent = self.exponent + len(self.coefficient) - len(coefficient)
            if exponent > _I64_MAX:
                raise ValueError("normalized exponent exceeds i64")
        object.__setattr__(self, "coefficient", coefficient)
        object.__setattr__(self, "exponent", exponent)


@dataclass(frozen=True)
class BinaryFloat:
    """IEEE-754 representation, with bitwise equality (not numeric equality).

    Preserves width, signed zero and NaN payload/signaling bits. Converting to a
    native Python float is deliberately not implicit.
    """
    width: int
    bits: int

    def __post_init__(self):
        if type(self.width) is not int or self.width not in (16, 32, 64):
            raise ValueError("float width must be 16, 32 or 64")
        if type(self.bits) is not int or not 0 <= self.bits < (1 << self.width):
            raise ValueError("float bits must fit its unsigned width")


@dataclass
class MapEntries:
    """Ordered key/value pairs, including non-hashable keys. Not a Python dict."""
    entries: list

    def __post_init__(self):
        if not isinstance(self.entries, list) or any(
            not isinstance(pair, (list, tuple)) or len(pair) != 2
            for pair in self.entries
        ):
            raise ValueError("map entries must be a list of key/value pairs")
        self.entries = [(key, value) for key, value in self.entries]


class AdapterError(ValueError):
    """The native adapter rejected an input, format or destination value."""


def _pack(value):
    if value is None:
        kind, data = "null", None
    elif type(value) is bool:
        kind, data = "bool", value
    elif isinstance(value, ExactNumber):
        kind, data = "number", f"{value.coefficient}e{value.exponent}"
    elif isinstance(value, BinaryFloat):
        kind, data = "float", [str(value.width), f"{value.bits:0{value.width // 4}x}"]
    elif type(value) is float:
        return _pack(BinaryFloat(64, int.from_bytes(struct.pack(">d", value), "big")))
    elif isinstance(value, MapEntries):
        kind, data = "map", [[_pack(k), _pack(v)] for k, v in value.entries]
    elif type(value) is int:
        kind, data = "number", str(value)
    elif isinstance(value, str):
        kind, data = "text", value
    elif isinstance(value, bytes):
        kind, data = "bytes", value.hex()
    elif isinstance(value, list):
        kind, data = "sequence", [_pack(v) for v in value]
    elif isinstance(value, dict):
        if not all(isinstance(k, str) for k in value):
            raise TypeError("mapping keys must be strings")
        kind, data = "mapping", {k: _pack(v) for k, v in value.items()}
    else:
        raise TypeError("expected None, bool, int, float, ExactNumber, BinaryFloat, MapEntries, str, bytes, list or str-keyed dict")
    return {"kind": kind, "value": data}


def _unpack(node):
    kind, value = node["kind"], node["value"]
    if kind in ("null", "bool", "text"):
        return value
    if kind == "number":
        coefficient, _, exponent = value.partition("e")
        return ExactNumber(coefficient, int(exponent) if exponent else 0)
    if kind == "float":
        return BinaryFloat(int(value[0]), int(value[1], 16))
    if kind == "map":
        return MapEntries([(_unpack(k), _unpack(v)) for k, v in value])
    if kind == "bytes":
        return bytes.fromhex(value)
    if kind == "sequence":
        return [_unpack(v) for v in value]
    if kind == "mapping":
        return {k: _unpack(v) for k, v in value.items()}
    raise AdapterError("unknown bridge document kind")


class AdapterClient:
    """Caller-owned executable and timeout, one isolated process per document.

    Build once with the documented offline Cargo command. A final application
    must carry its own source/binary, not depend on a development checkout remaining installed.
    Output buffers are not capped here; hostile inputs need host budgets/isolation.
    """
    def __init__(self, binary=None, *, timeout=30):
        self.binary = Path(binary) if binary is not None else Path(__file__).resolve().parents[1] / "target/release/examples/python_bridge"
        if not self.binary.is_absolute():
            raise ValueError("bridge binary path must be absolute")
        self.timeout = timeout

    def _call(self, operation, formats, data):
        if any(format not in _FORMATS for format in formats):
            raise ValueError("format must be json, cbor or yaml")
        if not isinstance(data, bytes):
            raise TypeError("wire input must be bytes")
        result = subprocess.run([str(self.binary), operation, *formats], input=data,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=self.timeout, check=False, shell=False)
        if result.returncode != 0:
            raise AdapterError(result.stderr.decode("utf-8", errors="replace").strip() or f"bridge exited {result.returncode}")
        return result.stdout

    def decode(self, format, data):
        """Decode numbers as ExactNumber or bit-preserving BinaryFloat, never float."""
        return _unpack(json.loads(self._call("decode", [format], data)))

    def encode(self, format, value):
        """Encode Python values; unsupported values/destination profiles fail."""
        tagged = json.dumps(_pack(value), ensure_ascii=True, allow_nan=False).encode("utf-8")
        return self._call("encode", [format], tagged)

    def transcode(self, source, destination, data):
        """Convert semantic values; no spelling, byte layout or canonicality promise."""
        return self._call("transcode", [source, destination], data)
