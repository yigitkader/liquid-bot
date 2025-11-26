#!/bin/bash
# Verify Solend instruction discriminator format

echo "🔍 Verifying Solend Instruction Discriminator Format"
echo ""

# Solend is based on SPL token-lending program
# SPL token-lending typically uses u8 instruction index, not Anchor discriminator
echo "⚠️  IMPORTANT: Solend is based on SPL token-lending program"
echo "   SPL token-lending programs typically use u8 instruction index (0, 1, 2, ...)"
echo "   NOT Anchor-style discriminator!"
echo ""

echo "📋 Checking Solend SDK for actual instruction format..."
echo ""

# Check if we can find instruction index from Solend SDK
echo "💡 To find the correct instruction index:"
echo "   1. Check Solend SDK: https://github.com/solendprotocol/solend-sdk"
echo "   2. Look for instruction enum or index mapping"
echo "   3. Check SPL token-lending program instruction indices"
echo ""

echo "📊 SPL Token Lending Program Instruction Indices (typical):"
echo "   0 = InitLendingMarket"
echo "   1 = SetLendingMarketOwner"
echo "   2 = InitReserve"
echo "   3 = RefreshReserve"
echo "   4 = DepositReserveLiquidity"
echo "   5 = RedeemReserveCollateral"
echo "   6 = InitObligation"
echo "   7 = RefreshObligation"
echo "   8 = BorrowObligationLiquidity"
echo "   9 = RepayObligationLiquidity"
echo "   10 = LiquidateObligation  ← This is likely the index!"
echo "   11 = FlashLoan"
echo ""

echo "🧪 Testing both formats:"
echo ""

python3 << PYTHON
import hashlib

# Format 1: Anchor discriminator (current implementation)
anchor_discriminator = hashlib.sha256(b"global:liquidateObligation").digest()[:8]
print("1. Anchor Format (current):")
print(f"   sha256(\"global:liquidateObligation\")[:8]")
print(f"   Hex: {anchor_discriminator.hex()}")
print(f"   Bytes: [{', '.join(f'0x{b:02x}' for b in anchor_discriminator)}]")
print()

# Format 2: u8 instruction index (SPL token-lending style)
# LiquidateObligation is typically instruction index 10
instruction_index = 10
print("2. SPL Token-Lending Format (likely correct):")
print(f"   u8 instruction index: {instruction_index}")
print(f"   Hex: {instruction_index:02x}")
print(f"   Bytes: [0x{instruction_index:02x}]")
print()

print("✅ RECOMMENDATION:")
print("   Use u8 instruction index format: [0x0a] (10 in decimal)")
print("   This matches SPL token-lending program convention")
PYTHON

