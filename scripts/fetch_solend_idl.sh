#!/bin/bash
# Solend IDL'ini resmi GitHub repo'sundan al

set -e

echo "🔍 Fetching Solend IDL from official repository..."

# Solend GitHub repo
REPO_URL="https://raw.githubusercontent.com/solendprotocol/solend-program/master/idl/solend.json"

# IDL'i al
echo "Downloading from: $REPO_URL"
curl -s "$REPO_URL" -o idl/solend_official.json

if [ $? -eq 0 ]; then
    echo "✅ IDL downloaded successfully"
    
    # Reserve account yapısını kontrol et
    echo ""
    echo "📋 Checking for Reserve account structure..."
    if grep -q '"name": "Reserve"' idl/solend_official.json; then
        echo "✅ Reserve account found in IDL"
        echo ""
        echo "Reserve account structure:"
        python3 -c "
import json
with open('idl/solend_official.json', 'r') as f:
    idl = json.load(f)
    for account in idl.get('accounts', []):
        if account.get('name') == 'Reserve':
            print(json.dumps(account, indent=2))
            break
    else:
        print('❌ Reserve account not found in IDL')
" 2>/dev/null || echo "Python not available, check manually"
    else
        echo "❌ Reserve account NOT found in IDL"
        echo "   This is a problem - we need the Reserve account structure"
    fi
else
    echo "❌ Failed to download IDL"
    echo "   Trying alternative method..."
    
    # Alternatif: Anchor IDL fetch
    echo ""
    echo "Trying Anchor IDL fetch..."
    anchor idl fetch So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo --provider.cluster mainnet --outfile idl/solend_anchor.json 2>/dev/null || {
        echo "❌ Anchor CLI not available"
        echo ""
        echo "Manual steps:"
        echo "1. Install Anchor: https://www.anchor-lang.com/docs/installation"
        echo "2. Run: anchor idl fetch So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo --provider.cluster mainnet"
        echo "3. Or check Solend GitHub: https://github.com/solendprotocol/solend-program"
    }
fi

echo ""
echo "📝 Next steps:"
echo "1. Compare idl/solend_official.json with idl/solend.json"
echo "2. Update src/protocols/solend_reserve.rs with correct structure"
echo "3. Test parsing with real reserve account"

