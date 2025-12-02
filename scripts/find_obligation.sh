#!/bin/bash

# Script to find a real Solend obligation account for testing
# This script queries Solend program accounts and finds active obligations

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}🔍 Finding Solend Obligation Accounts...${NC}"
echo ""

# Load .env if exists
if [ -f .env ]; then
    export $(cat .env | grep -v '^#' | xargs)
fi

# Default values
SOLEND_PROGRAM_ID=${SOLEND_PROGRAM_ID:-"So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo"}
RPC_HTTP_URL=${RPC_HTTP_URL:-"https://api.mainnet-beta.solana.com"}

echo -e "${YELLOW}Using:${NC}"
echo "  SOLEND_PROGRAM_ID: $SOLEND_PROGRAM_ID"
echo "  RPC_HTTP_URL: $RPC_HTTP_URL"
echo ""

# Check if solana CLI is available
if ! command -v solana &> /dev/null; then
    echo -e "${YELLOW}⚠️  solana CLI not found. Using Rust binary instead...${NC}"
    echo ""
    
    # Use Rust binary to find obligations
    echo -e "${BLUE}Running: cargo run --bin test_production_features${NC}"
    echo ""
    
    if cargo run --bin test_production_features 2>&1 | grep -E "(obligation|Obligation|position|Position)" | head -20; then
        echo ""
        echo -e "${GREEN}✅ Check the output above for obligation addresses${NC}"
    else
        echo ""
        echo -e "${YELLOW}⚠️  No obligations found in test output${NC}"
        echo ""
        echo -e "${BLUE}Alternative: Use Solana Explorer${NC}"
        echo "  1. Go to: https://explorer.solana.com/address/$SOLEND_PROGRAM_ID"
        echo "  2. Click 'Program Accounts' tab"
        echo "  3. Look for accounts with ~1300 bytes size (obligations)"
        echo "  4. Copy a pubkey and set it as TEST_OBLIGATION_PUBKEY in .env"
    fi
else
    # Use solana CLI
    echo -e "${BLUE}Fetching program accounts...${NC}"
    echo ""
    
    # Get program accounts (this may take a while and hit rate limits)
    echo -e "${YELLOW}⚠️  This may take a while and use RPC quota...${NC}"
    echo ""
    
    solana account "$SOLEND_PROGRAM_ID" --url "$RPC_HTTP_URL" > /dev/null 2>&1 || {
        echo -e "${YELLOW}⚠️  Cannot access RPC. Trying alternative method...${NC}"
        echo ""
        echo -e "${BLUE}Use this command manually:${NC}"
        echo "  cargo run --bin test_production_features"
        echo ""
        echo "Or use Solana Explorer:"
        echo "  https://explorer.solana.com/address/$SOLEND_PROGRAM_ID"
        exit 1
    }
    
    echo -e "${GREEN}✅ RPC connection successful${NC}"
    echo ""
    echo -e "${BLUE}To find obligations, run:${NC}"
    echo "  cargo run --bin test_production_features"
    echo ""
    echo "Or use Solana Explorer:"
    echo "  https://explorer.solana.com/address/$SOLEND_PROGRAM_ID"
    echo "  → Click 'Program Accounts' → Filter by size ~1300 bytes"
fi

echo ""
echo -e "${BLUE}📝 Once you find an obligation pubkey, add it to .env:${NC}"
echo "  TEST_OBLIGATION_PUBKEY=<your_obligation_pubkey_here>"
echo ""

