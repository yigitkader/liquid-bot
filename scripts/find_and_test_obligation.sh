#!/bin/bash
# Find and test a real Solend obligation account

set -e

RPC_URL="${RPC_URL:-https://api.mainnet-beta.solana.com}"
SOLEND_PROGRAM_ID="So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo"

echo "🔍 Finding Solend obligation accounts..."
echo ""

# RPC'den program account'larını çek (sadece büyük account'lar - muhtemelen obligation'lar)
echo "Fetching program accounts from RPC..."
RESULT=$(curl -s -X POST "$RPC_URL" \
  -H 'Content-Type: application/json' \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"id\": 1,
    \"method\": \"getProgramAccounts\",
    \"params\": [
      \"$SOLEND_PROGRAM_ID\",
      {
        \"encoding\": \"base64\",
        \"filters\": [
          {
            \"dataSize\": 300
          }
        ]
      }
    ]
  }")

# Python ile parse et
python3 << PYTHON
import sys
import json
import base64

try:
    data = json.loads('''$RESULT''')
    
    if 'error' in data:
        print(f"❌ RPC Error: {data['error']}")
        sys.exit(1)
    
    accounts = data.get('result', [])
    print(f"Found {len(accounts)} accounts with dataSize >= 300 bytes")
    
    if len(accounts) == 0:
        print("\n⚠️  No accounts found. Trying without filter...")
        print("\n💡 Manual steps:")
        print("   1. Visit: https://solscan.io/account/$SOLEND_PROGRAM_ID")
        print("   2. Look for 'Obligation' accounts in the 'Token Accounts' section")
        print("   3. Or use Solend SDK to find active obligations")
        print("   4. Test with: cargo run --bin validate_obligation -- --obligation <ADDRESS>")
        sys.exit(0)
    
    # Account'ları size'a göre sırala (büyükten küçüğe)
    accounts_with_size = []
    for acc in accounts:
        pubkey = acc['pubkey']
        data_size = len(base64.b64decode(acc['account']['data'][0]))
        accounts_with_size.append((pubkey, data_size))
    
    accounts_with_size.sort(key=lambda x: x[1], reverse=True)
    
    print("\n📋 Top 5 largest accounts (likely obligations):")
    for i, (pubkey, size) in enumerate(accounts_with_size[:5], 1):
        print(f"   {i}. {pubkey} ({size} bytes)")
    
    # İlk account'u test et
    if len(accounts_with_size) > 0:
        test_account = accounts_with_size[0][0]
        print(f"\n🧪 Testing first account: {test_account}")
        print(f"   Run: cargo run --bin validate_obligation -- --obligation {test_account}")
        
except Exception as e:
    print(f"❌ Error parsing response: {e}")
    sys.exit(1)
PYTHON

