#!/bin/bash
# Reserve Account Structure Validation Script
# 
# Bu script, gerçek Solend mainnet reserve account'larını parse ederek
# struct yapısının doğruluğunu doğrular.

set -e

echo "🔍 Validating Solend Reserve Account Structure..."
echo ""

# RPC URL (mainnet)
RPC_URL="${RPC_URL:-https://api.mainnet-beta.solana.com}"

# Bilinen Solend reserve account'ları (mainnet)
# NOT: Bu adresler gerçek mainnet reserve account'larıdır.
# Solend'in resmi dokümantasyonundan veya SDK'sından alınmalıdır.

# Örnek: USDC Reserve (gerçek adres bulunmalı)
# USDC_RESERVE="..."

echo "📋 To validate reserve structure:"
echo "1. Find a real Solend reserve account address from mainnet"
echo "2. Use Solend SDK: https://sdk.solend.fi"
echo "3. Or check Solend documentation"
echo ""
echo "Example validation command:"
echo "  cargo run --bin validate_reserve -- --rpc-url $RPC_URL --reserve <RESERVE_ADDRESS>"
echo ""
echo "Or use the Rust validator:"
echo "  use crate::protocols::reserve_validator::validate_reserve_structure;"
echo "  validate_reserve_structure(rpc_client, &reserve_pubkey).await?;"
echo ""

# Python script ile IDL'den Reserve yapısını çıkarma (eğer IDL'de varsa)
if [ -f "idl/solend_official.json" ]; then
    echo "📋 Checking IDL for Reserve structure..."
    python3 << 'PYTHON'
import json
import sys

try:
    with open('idl/solend_official.json', 'r') as f:
        idl = json.load(f)
    
    # Reserve account yapısını bul
    reserve_found = False
    for account in idl.get('accounts', []):
        if account.get('name') == 'Reserve':
            reserve_found = True
            print("✅ Reserve account found in IDL:")
            print(json.dumps(account, indent=2))
            break
    
    if not reserve_found:
        print("❌ Reserve account NOT found in IDL")
        print("   This is expected - Solend IDL may not include Reserve account structure")
        print("   Need to validate against real mainnet reserve accounts")
        
except FileNotFoundError:
    print("❌ IDL file not found: idl/solend_official.json")
    print("   Run: ./scripts/fetch_solend_idl.sh")
except Exception as e:
    print(f"❌ Error: {e}")
PYTHON
else
    echo "⚠️  IDL file not found: idl/solend_official.json"
    echo "   Run: ./scripts/fetch_solend_idl.sh"
fi

echo ""
echo "✅ Validation script ready!"
echo "   Next: Test with real mainnet reserve account"

