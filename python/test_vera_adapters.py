"""Deterministic real-process tests. Build bridge explicitly before running."""
import os
from pathlib import Path
import subprocess
import unittest
from vera_adapters import AdapterClient, AdapterError, ExactNumber, BinaryFloat, MapEntries


class BoundaryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.client = AdapterClient(os.environ.get("VERA_ADAPTER_BRIDGE"))
        if not cls.client.binary.is_file():
            raise RuntimeError("build python_bridge before running these tests")

    def test_exact_numbers_and_semantic_roundtrips(self):
        value = self.client.decode("json", b'{"wide":18446744073709551616,"fraction":1.2500,"tiny":1e-9223372036854775808,"flag":true,"none":null,"text":"yes"}')
        self.assertEqual(value["wide"], ExactNumber("18446744073709551616"))
        self.assertEqual(value["fraction"], ExactNumber("125", -2))
        for format in ("json", "cbor", "yaml"):
            with self.subTest(format=format):
                self.assertEqual(self.client.decode(format, self.client.encode(format, value)), value)
                self.assertEqual(self.client.decode(format, self.client.transcode("json", format, b'{"n":1.25}')), {"n": ExactNumber("125", -2)})

    def test_bytes_and_collision_free_mapping(self):
        value = {"kind": "bytes", "value": b"\x00\xff", "nested": [False, "text", None]}
        self.assertEqual(self.client.decode("cbor", self.client.encode("cbor", value)), value)
        for format in ("json", "yaml"):
            with self.assertRaises(AdapterError):
                self.client.encode(format, value)
        self.assertEqual(self.client.decode("cbor", bytes.fromhex("4200ff")), b"\x00\xff")
        with self.assertRaises(AdapterError):
            self.client.transcode("cbor", "json", bytes.fromhex("4200ff"))

    def test_independent_known_wire_vectors(self):
        self.assertEqual(self.client.encode("cbor", {"n": 24}), bytes.fromhex("a1616e1818"))
        self.assertEqual(self.client.decode("cbor", bytes.fromhex("c482211904d2")), ExactNumber("1234", -2))
        self.assertEqual(self.client.decode("yaml", b'yes: yes\ndate: 2026-01-02\n'), {"yes": "yes", "date": "2026-01-02"})

    def test_malformed_and_protocol_incompatible_inputs(self):
        cases = [("json", b'{"a":1,"a":2}'), ("json", b'{}{}'), ("json", b'1e'),
                 ("cbor", bytes.fromhex("c001")), # unknown tag
                 ("cbor", bytes.fromhex("0001")), ("cbor", bytes.fromhex("62ff")),
                 ("yaml", b'a: &x 1\nb: *x\n'), ("yaml", b'a: 1\na: 2\n'),
                 ("yaml", b'---\na: 1\n---\nb: 2\n'), ("yaml", b'!!binary AA==')]
        for format, wire in cases:
            with self.subTest(format=format, wire=wire), self.assertRaises(AdapterError):
                self.client.decode(format, wire)

    def test_local_validation_and_exponent_boundaries(self):
        self.assertEqual(ExactNumber("100", -2), ExactNumber("1"))
        self.assertEqual(ExactNumber("-0", 99), ExactNumber("0"))
        for coefficient, exponent in [("01", 0), ("1", 2**63), ("10", 2**63-1), ("1", True)]:
            with self.assertRaises(ValueError): ExactNumber(coefficient, exponent)
        with self.assertRaises(TypeError): self.client.encode("json", {1: "x"})
        for value in (1.25, float("nan")):
            with self.assertRaises(AdapterError): self.client.encode("json", value)
        for width, bits in [(8, 0), (True, 0), (16, -1), (16, 65536), (32, True), (64, 2**64)]:
            with self.assertRaises(ValueError): BinaryFloat(width, bits)
        for entries in ({}, [[1]], [[1, 2, 3]]):
            with self.assertRaises(ValueError): MapEntries(entries)
        with self.assertRaises(ValueError): self.client.decode("xml", b"null")
        with self.assertRaises(TypeError): self.client.decode("json", "null")
        with self.assertRaises(ValueError): AdapterClient(Path("relative"))

    def test_binary_floats_preserve_every_wire_bit(self):
        # All widths: ordinary finite, negative zero, infinities, payload NaNs.
        vectors = {
            16: (0x3c00, 0x8000, 0x7c00, 0xfc00, 0x7e42, 0x7c01, 0xfe01),
            32: (0x3f800000, 0x80000000, 0x7f800000, 0xff800000, 0x7fc01234, 0x7f800001, 0xffc00042),
            64: (0x3ff0000000000000, 0x8000000000000000, 0x7ff0000000000000,
                 0xfff0000000000000, 0x7ff8000000001234, 0x7ff0000000000001, 0xfff8000000000042),
        }
        for width, bit_patterns in vectors.items():
            for bits in bit_patterns:
                with self.subTest(width=width, bits=hex(bits)):
                    wire = bytes([{16: 0xf9, 32: 0xfa, 64: 0xfb}[width]]) + bits.to_bytes(width // 8, "big")
                    value = BinaryFloat(width, bits)
                    self.assertEqual(self.client.decode("cbor", wire), value)
                    self.assertEqual(self.client.encode("cbor", value), wire)
                    self.assertEqual(self.client.transcode("cbor", "cbor", wire), wire)
                    for destination in ("json", "yaml"):
                        with self.assertRaises(AdapterError): self.client.encode(destination, value)
                        with self.assertRaises(AdapterError): self.client.transcode("cbor", destination, wire)
        self.assertEqual(self.client.encode("cbor", -0.0), bytes.fromhex("fb8000000000000000"))
        self.assertEqual(self.client.decode("cbor", self.client.encode("cbor", 1.5)), BinaryFloat(64, 0x3ff8000000000000))

    def test_arbitrary_map_keys_and_nested_keys(self):
        self.assertEqual(self.client.decode("cbor", bytes.fromhex("a10102")),
                         MapEntries([(ExactNumber("1"), ExactNumber("2"))]))
        value = MapEntries([
            (True, "boolean"), (ExactNumber("1"), "integer"),
            ([ExactNumber("2"), "x"], "array key"),
            ({"nested": False}, "map key"), (b"x", "bytes key"),
            (BinaryFloat(16, 0x8000), "negative zero"),
            (BinaryFloat(32, 0x7fc01234), "nan key"),
        ])
        self.assertEqual(self.client.decode("cbor", self.client.encode("cbor", value)), value)
        for destination in ("json", "yaml"):
            with self.assertRaises(AdapterError): self.client.encode(destination, value)
        # Existing text-keyed maps still return dict, not MapEntries.
        self.assertEqual(self.client.decode("cbor", self.client.encode("cbor", {"one": True})), {"one": True})

    def test_large_coefficients_do_not_use_python_integer_parser(self):
        coefficient = "1" * 5000
        value = self.client.decode("json", coefficient.encode())
        self.assertEqual(value, ExactNumber(coefficient))
        self.assertEqual(self.client.decode("cbor", self.client.encode("cbor", value)), value)

    def test_failed_bridge_has_no_partial_stdout(self):
        result = subprocess.run([str(self.client.binary), "decode", "json"], input=b'{"x":', capture_output=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, b"")
        self.assertTrue(result.stderr)


if __name__ == "__main__": unittest.main()
