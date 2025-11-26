#!/bin/bash
# Test Obligation with Known Account
#
# Bu script, bilinen bir obligation account ile test yapar.
# Eğer obligation account adresi verilmezse, kullanıcıya nasıl bulacağını gösterir.

set -e

RPC_URL="${RPC_URL:-https://api.mainnet-beta.solana.com}"

echo "🔍 Testing Solend Obligation Account"
echo "===================================="
echo ""

# Eğer obligation account adresi verilmişse, direkt test et
if [ -n "$1" ]; then
    OBLIGATION_PUBKEY="$1"
    echo "Testing obligation account: $OBLIGATION_PUBKEY"
    echo ""
    
    cargo run --bin find_and_test_obligation -- \
        --rpc-url "$RPC_URL" \
        --obligation "$OBLIGATION_PUBKEY"
    
    exit $?
fi

# Aksi halde, kullanıcıya nasıl bulacağını göster
echo "⚠️  No obligation account address provided."
echo ""
echo "To test an obligation account, you need to provide an address."
echo ""
echo "📋 How to find a Solend obligation account:"
echo ""
echo "Method 1: Use Solend Dashboard"
echo "  1. Visit: https://solend.fi/dashboard"
echo "  2. Connect a wallet that has a position"
echo "  3. The obligation account address will be visible"
echo ""
echo "Method 2: Use Solana Explorer"
echo "  1. Visit: https://explorer.solana.com/address/So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo"
echo "  2. Look for 'Program Accounts' section"
echo "  3. Find accounts with data size > 1000 bytes"
echo ""
echo "Method 3: Use RPC (may hit rate limits)"
echo "  curl -X POST $RPC_URL \\"
echo "    -H 'Content-Type: application/json' \\"
echo "    -d '{"
echo "      \"jsonrpc\": \"2.0\","
echo "      \"id\": 1,"
echo "      \"method\": \"getProgramAccounts\","
echo "      \"params\": ["
echo "        \"So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo\","
echo "        {"
echo "          \"filters\": [{ \"dataSize\": 1000 }]"
echo "        }"
echo "      ]"
echo "    }'"
echo ""
echo "Once you have an obligation account address, run:"
echo ""
echo "  ./scripts/test_obligation_with_known_account.sh <OBLIGATION_PUBKEY>"
echo ""
echo "Or use the Rust tool directly:"
echo ""
echo "  cargo run --bin find_and_test_obligation -- \\"
echo "    --rpc-url $RPC_URL \\"
echo "    --obligation <OBLIGATION_PUBKEY>"
echo ""

