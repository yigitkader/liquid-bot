#!/bin/bash
# Test instruction discriminator calculation

echo "🔍 Testing Solend Instruction Discriminator"
echo ""

# Test different discriminator formats
python3 << PYTHON
import hashlib

# Test strings for Anchor discriminator
test_strings = [
    "global:liquidateObligation",
    "liquidateObligation",
    "liquidate_obligation",
]

print("Testing Anchor-style discriminators:")
print("=" * 60)
for test_str in test_strings:
    hash_obj = hashlib.sha256(test_str.encode())
    discriminator = hash_obj.digest()[:8]
    hex_str = "".join(f"{b:02x}" for b in discriminator)
    print(f"sha256(\"{test_str}\")[:8]")
    print(f"  Hex: {hex_str}")
    print(f"  Bytes: [{', '.join(f'0x{b:02x}' for b in discriminator)}]")
    print()

# Current implementation
current = hashlib.sha256(b"global:liquidateObligation").digest()[:8]
print("Current implementation (global:liquidateObligation):")
print(f"  Hex: {''.join(f'{b:02x}' for b in current)}")
print(f"  Bytes: [{', '.join(f'0x{b:02x}' for b in current)}]")
print()

# Check if Solend is Anchor-based
print("⚠️  NOTE: Solend might not be an Anchor program!")
print("   If Solend uses a different instruction format, discriminator might be:")
print("   - u8 instruction index (0, 1, 2, ...)")
print("   - Custom format")
print("   - No discriminator (direct instruction index)")
print()
print("💡 To verify:")
print("   1. Check Solend program source code")
print("   2. Check Solend SDK for actual discriminator")
print("   3. Test with a real transaction")
PYTHON

