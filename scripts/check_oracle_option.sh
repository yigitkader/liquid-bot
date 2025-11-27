#!/bin/bash
# Gerçek Solend reserve account'tan oracle_option değerini kontrol et

set -e

RESERVE="BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw"
RPC_URL="${1:-https://api.mainnet-beta.solana.com}"

echo "🔍 Checking oracle_option from real Solend reserve account..."
echo "Reserve: $RESERVE"
echo "RPC: $RPC_URL"
echo ""

# RPC ile account data'yı al
ACCOUNT_DATA=$(curl -s -X POST "$RPC_URL" \
  -H "Content-Type: application/json" \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"id\": 1,
    \"method\": \"getAccountInfo\",
    \"params\": [
      \"$RESERVE\",
      {
        \"encoding\": \"base64\"
      }
    ]
  }" | jq -r '.result.value.data[0]')

if [ "$ACCOUNT_DATA" = "null" ] || [ -z "$ACCOUNT_DATA" ]; then
  echo "❌ Failed to fetch account data"
  exit 1
fi

# Base64'ü decode et ve hex'e çevir
HEX_DATA=$(echo "$ACCOUNT_DATA" | base64 -d | xxd -p -c 1000)

# Offset 107-110: oracle_option (u32, 4 bytes)
# Little-endian olarak oku
OFFSET=107
ORACLE_OPTION_HEX="${HEX_DATA:$((OFFSET*2)):8}"

# Little-endian'ı big-endian'a çevir ve decimal'e çevir
ORACLE_OPTION=$((0x$(echo "$ORACLE_OPTION_HEX" | sed 's/\(..\)\(..\)\(..\)\(..\)/\4\3\2\1/')))

echo "📊 Oracle Option Analysis:"
echo "  Offset: 107-110 (4 bytes, u32)"
echo "  Hex: $ORACLE_OPTION_HEX"
echo "  Decimal: $ORACLE_OPTION"
echo ""

case $ORACLE_OPTION in
  0)
    echo "  ✅ oracle_option = 0 (None - no oracle)"
    ;;
  1)
    echo "  ✅ oracle_option = 1 (Pyth - active)"
    ;;
  2)
    echo "  ✅ oracle_option = 2 (Switchboard - active)"
    ;;
  *)
    echo "  ⚠️  oracle_option = $ORACLE_OPTION (UNEXPECTED - not 0, 1, or 2!)"
    ;;
esac

echo ""
echo "📋 Oracle Addresses:"
# Pyth oracle (offset 111-142, 32 bytes)
PYTH_HEX="${HEX_DATA:$((111*2)):64}"
echo "  Pyth Oracle (offset 111-142):"
echo "    Hex: $PYTH_HEX"

# Switchboard oracle (offset 143-174, 32 bytes)
SWITCHBOARD_HEX="${HEX_DATA:$((143*2)):64}"
echo "  Switchboard Oracle (offset 143-174):"
echo "    Hex: $SWITCHBOARD_HEX"

