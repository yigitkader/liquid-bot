#!/bin/bash
# Test obligation discriminator calculation

echo "🔍 Testing Obligation Discriminator"
echo ""

# Anchor'da discriminator = sha256("account:Obligation")[:8]
# veya "global:Obligation" gibi bir prefix ile

echo "Expected discriminator: [0x6f, 0x62, 0x6c, 0x69, 0x67, 0x61, 0x74, 0x69] (\"obligati\")"
echo ""

# Python ile test et
python3 << PYTHON
import hashlib

# Anchor'da genellikle "account:Obligation" veya "global:Obligation" formatı kullanılır
test_strings = [
    "account:Obligation",
    "global:Obligation", 
    "Obligation",
    "obligation",
]

for test_str in test_strings:
    hash_obj = hashlib.sha256(test_str.encode())
    discriminator = hash_obj.digest()[:8]
    hex_str = "".join(f"{b:02x}" for b in discriminator)
    print(f"sha256(\"{test_str}\")[:8] = [{', '.join(f'0x{b:02x}' for b in discriminator)}]")
    print(f"  Hex: {hex_str}")
    print(f"  ASCII: {discriminator.decode('ascii', errors='ignore')}")
    print()

# Beklenen: "obligati" = [0x6f, 0x62, 0x6c, 0x69, 0x67, 0x61, 0x74, 0x69]
expected = bytes([0x6f, 0x62, 0x6c, 0x69, 0x67, 0x61, 0x74, 0x69])
print(f"Expected: {expected.decode('ascii')} = [{', '.join(f'0x{b:02x}' for b in expected)}]")
PYTHON

