#!/bin/bash
# Find Solend Obligation Accounts on Mainnet
#
# Bu script, Solend mainnet'te obligation account'larını bulur.

set -e

RPC_URL="${RPC_URL:-https://api.mainnet-beta.solana.com}"
SOLEND_PROGRAM_ID="So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo"

echo "🔍 Finding Solend Obligation Accounts on Mainnet..."
echo "RPC URL: $RPC_URL"
echo "Solend Program ID: $SOLEND_PROGRAM_ID"
echo ""

# Solend'in main lending market'i
MAIN_LENDING_MARKET="4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY"

echo "📋 Method 1: Using Solana CLI to find program accounts..."
echo "--------------------------------------------------------"
echo ""
echo "Finding obligation accounts owned by Solend program..."
echo ""

# Solana CLI ile program account'larını bul
# NOT: Bu işlem zaman alabilir ve rate limit'e takılabilir
if command -v solana &> /dev/null; then
    echo "Using solana CLI..."
    solana account --url "$RPC_URL" "$SOLEND_PROGRAM_ID" 2>/dev/null || echo "Program account check..."
    
    echo ""
    echo "To find obligation accounts, you can use:"
    echo "  solana account <OBLIGATION_PUBKEY> --url $RPC_URL"
    echo ""
else
    echo "⚠️  solana CLI not found. Install it from: https://docs.solana.com/cli/install-solana-cli-tools"
    echo ""
fi

echo "📋 Method 2: Using known obligation accounts..."
echo "--------------------------------------------------------"
echo ""
echo "You can find obligation accounts from:"
echo "  1. Solend Dashboard: https://solend.fi/dashboard"
echo "  2. Solend API: https://api.solend.fi"
echo "  3. Solana Explorer: https://explorer.solana.com"
echo ""
echo "Example: Look for accounts owned by $SOLEND_PROGRAM_ID"
echo ""

echo "📋 Method 3: Using RPC getProgramAccounts..."
echo "--------------------------------------------------------"
echo ""
echo "You can use RPC getProgramAccounts to find all obligation accounts:"
echo ""
echo "curl -X POST $RPC_URL \\"
echo "  -H 'Content-Type: application/json' \\"
echo "  -d '{"
echo "    \"jsonrpc\": \"2.0\","
echo "    \"id\": 1,"
echo "    \"method\": \"getProgramAccounts\","
echo "    \"params\": ["
echo "      \"$SOLEND_PROGRAM_ID\","
echo "      {"
echo "        \"filters\": ["
echo "          {"
echo "            \"dataSize\": 1300  // Obligation account size (approximate)"
echo "          }"
echo "        ]"
echo "      }"
echo "    ]"
echo "  }'"
echo ""

echo "💡 TIP: Once you have an obligation account address, test it with:"
echo ""
echo "cargo run --bin validate_obligation -- \\"
echo "  --rpc-url $RPC_URL \\"
echo "  --obligation <OBLIGATION_PUBKEY>"
echo ""

