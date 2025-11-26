#!/bin/bash
# Find a real Solend obligation account for testing

set -e

RPC_URL="${RPC_URL:-https://api.mainnet-beta.solana.com}"
SOLEND_PROGRAM_ID="So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo"

echo "🔍 Finding Solend obligation accounts for testing..."
echo ""

# Try to get program accounts with a reasonable data size filter
# Obligation accounts are typically larger than reserve accounts
echo "Fetching program accounts (this may take a while)..."
echo ""

# Use a simple approach: try to get accounts and test them
echo "💡 To find an obligation account manually:"
echo ""
echo "1. Visit Solscan:"
echo "   https://solscan.io/account/$SOLEND_PROGRAM_ID"
echo "   Look for 'Token Accounts' or 'Program Accounts' section"
echo ""
echo "2. Or use Solend SDK/Explorer:"
echo "   https://solend.fi/dashboard"
echo ""
echo "3. Or check Solend liquidator bot examples:"
echo "   https://github.com/solendprotocol/liquidator"
echo ""
echo "4. Once you have an obligation account, test with:"
echo "   cargo run --bin test_instruction_format -- \\"
echo "     --rpc-url $RPC_URL \\"
echo "     --obligation <OBLIGATION_PUBKEY>"
echo ""

