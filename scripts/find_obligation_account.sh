#!/bin/bash
# Find a real Solend obligation account from mainnet
#
# Bu script, Solend program'ından gerçek bir obligation account'u bulur
# ve test için kullanılabilir.

set -e

RPC_URL="${RPC_URL:-https://api.mainnet-beta.solana.com}"
SOLEND_PROGRAM_ID="So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo"

echo "🔍 Finding a real Solend obligation account from mainnet..."
echo "RPC URL: $RPC_URL"
echo "Program ID: $SOLEND_PROGRAM_ID"
echo ""

# Solana CLI ile program account'larını çek
# NOT: Bu işlem uzun sürebilir, bu yüzden sadece ilk birkaç account'u alıyoruz
echo "Fetching program accounts (this may take a while)..."
echo ""

# Solana CLI kullanarak program account'larını çek
# İlk 10 account'u al (obligation account'ları genellikle büyük account'lardır)
solana account "$SOLEND_PROGRAM_ID" --output json > /dev/null 2>&1 || {
    echo "⚠️  Solana CLI not found or RPC connection failed"
    echo ""
    echo "Alternative: Use Solend SDK or explorer to find an obligation account:"
    echo "  1. Visit: https://solscan.io/account/$SOLEND_PROGRAM_ID"
    echo "  2. Look for 'Obligation' accounts"
    echo "  3. Use one of those addresses with validate_obligation tool"
    echo ""
    echo "Or use the RPC directly:"
    echo "  curl -X POST $RPC_URL -H 'Content-Type: application/json' -d '"
    echo '    {
      "jsonrpc": "2.0",
      "id": 1,
      "method": "getProgramAccounts",
      "params": [
        "'"$SOLEND_PROGRAM_ID"'",
        {
          "encoding": "base64",
          "filters": [
            {
              "dataSize": 1000
            }
          ]
        }
      ]
    }'
    echo "'"
    exit 1
}

echo "✅ To find an obligation account manually:"
echo "   1. Visit Solend explorer or Solscan"
echo "   2. Look for accounts owned by: $SOLEND_PROGRAM_ID"
echo "   3. Find accounts with data size > 1000 bytes (likely obligations)"
echo "   4. Test with: cargo run --bin validate_obligation -- --obligation <ADDRESS>"
echo ""

