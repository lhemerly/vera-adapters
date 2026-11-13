"""Run after building python_bridge; no solver or third-party Python package."""
from vera_adapters import AdapterClient, ExactNumber

client = AdapterClient()
value = client.decode("json", b'{"count":18446744073709551616,"enabled":true}')
assert value["count"] == ExactNumber("18446744073709551616")
for format in ("json", "cbor", "yaml"):
    assert client.decode(format, client.encode(format, value)) == value
assert client.decode("cbor", client.encode("cbor", b"\x00\xff")) == b"\x00\xff"
print("JSON/CBOR/YAML exact Python boundary roundtrips passed")
