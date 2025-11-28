#!/bin/bash

# Real-time Dry-Run Monitor
# Monitors the bot in real-time and shows key metrics

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

LOG_FILE="${1:-logs/dry_run_latest.log}"

echo -e "${BLUE}📊 Real-Time Dry-Run Monitor${NC}"
echo -e "${BLUE}Monitoring: $LOG_FILE${NC}"
echo -e "${YELLOW}Press Ctrl+C to stop${NC}"
echo ""

# Create log file if it doesn't exist
touch "$LOG_FILE"

# Monitor in real-time
tail -f "$LOG_FILE" | while IFS= read -r line; do
    # WebSocket connection
    if echo "$line" | grep -q "✅ WebSocket connected"; then
        echo -e "${GREEN}[$(date +%H:%M:%S)] ✅ WebSocket Connected${NC}"
    fi
    
    # Subscription
    if echo "$line" | grep -q "✅ Subscribed to program accounts"; then
        echo -e "${GREEN}[$(date +%H:%M:%S)] ✅ Subscribed to Program Accounts${NC}"
    fi
    
    # Opportunity detected
    if echo "$line" | grep -q "Opportunity detected"; then
        ACCOUNT=$(echo "$line" | grep -oE 'account=[A-Za-z0-9]{32,44}' | cut -d= -f2 | cut -c1-8 || echo "N/A")
        HF=$(echo "$line" | grep -oE 'health_factor=[0-9.]+' | cut -d= -f2 || echo "N/A")
        PROFIT=$(echo "$line" | grep -oE 'profit=\$[0-9.]+' | cut -d= -f2 || echo "N/A")
        echo -e "${YELLOW}[$(date +%H:%M:%S)] 🎯 Opportunity: Account=${ACCOUNT}... HF=${HF} Profit=${PROFIT}${NC}"
    fi
    
    # Profit calculation
    if echo "$line" | grep -q "Estimated profit"; then
        PROFIT=$(echo "$line" | grep -oE '\$[0-9]+\.[0-9]+' | head -1 || echo "N/A")
        echo -e "${GREEN}[$(date +%H:%M:%S)] 💰 Profit: ${PROFIT}${NC}"
    fi
    
    # Errors
    if echo "$line" | grep -qi "error"; then
        echo -e "${RED}[$(date +%H:%M:%S)] ❌ Error: ${line:0:100}${NC}"
    fi
    
    # Health check
    if echo "$line" | grep -q "Health check"; then
        if echo "$line" | grep -q "passed"; then
            echo -e "${GREEN}[$(date +%H:%M:%S)] ✅ Health Check: PASSED${NC}"
        elif echo "$line" | grep -q "failed"; then
            echo -e "${RED}[$(date +%H:%M:%S)] ❌ Health Check: FAILED${NC}"
        fi
    fi
done

