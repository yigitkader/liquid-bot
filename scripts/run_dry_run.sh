#!/bin/bash

# Dry-Run Test Script with Logging
# This script runs the bot in dry-run mode and saves logs for analysis

set -e

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Create logs directory
mkdir -p logs

# Generate log filename with timestamp
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
LOG_FILE="logs/dry_run_${TIMESTAMP}.log"
SUMMARY_FILE="logs/dry_run_summary_${TIMESTAMP}.txt"

echo -e "${BLUE}🚀 Starting Dry-Run Test${NC}"
echo -e "${BLUE}Log file: ${LOG_FILE}${NC}"
echo -e "${BLUE}Summary file: ${SUMMARY_FILE}${NC}"
echo ""

# Start the bot with logging
echo -e "${GREEN}Starting bot in dry-run mode...${NC}"
echo -e "${YELLOW}Press Ctrl+C to stop${NC}"
echo ""

# Run the bot and log everything
DRY_RUN=true cargo run --bin liquid-bot 2>&1 | tee "$LOG_FILE"

# Generate summary
echo ""
echo -e "${BLUE}📊 Generating summary...${NC}"

{
    echo "=== DRY-RUN TEST SUMMARY ==="
    echo "Date: $(date)"
    echo "Log File: $LOG_FILE"
    echo ""
    echo "=== KEY METRICS ==="
    
    # Count opportunities detected
    OPPORTUNITIES=$(grep -c "Opportunity detected" "$LOG_FILE" 2>/dev/null || echo "0")
    echo "Opportunities Detected: $OPPORTUNITIES"
    
    # Count successful profit calculations
    PROFIT_CALC=$(grep -c "Estimated profit" "$LOG_FILE" 2>/dev/null || echo "0")
    echo "Profit Calculations: $PROFIT_CALC"
    
    # WebSocket connection status
    if grep -q "✅ WebSocket connected" "$LOG_FILE"; then
        echo "WebSocket: ✅ Connected"
    else
        echo "WebSocket: ❌ Not connected"
    fi
    
    if grep -q "✅ Subscribed to program accounts" "$LOG_FILE"; then
        echo "Subscription: ✅ Active"
    else
        echo "Subscription: ❌ Not active"
    fi
    
    echo ""
    echo "=== ERRORS ==="
    ERROR_COUNT=$(grep -c "ERROR\|error\|Error" "$LOG_FILE" 2>/dev/null || echo "0")
    echo "Total Errors: $ERROR_COUNT"
    if [ "$ERROR_COUNT" -gt 0 ]; then
        echo ""
        echo "Recent Errors:"
        grep -i "error" "$LOG_FILE" | tail -10
    fi
    
    echo ""
    echo "=== WARNINGS ==="
    WARNING_COUNT=$(grep -c "WARN\|warn\|Warning" "$LOG_FILE" 2>/dev/null || echo "0")
    echo "Total Warnings: $WARNING_COUNT"
    
    echo ""
    echo "=== OPPORTUNITY DETAILS ==="
    if [ "$OPPORTUNITIES" -gt 0 ]; then
        echo "Recent Opportunities:"
        grep "Opportunity detected" "$LOG_FILE" | tail -5
    else
        echo "No opportunities detected during this run"
    fi
    
    echo ""
    echo "=== PROFIT CALCULATIONS ==="
    if [ "$PROFIT_CALC" -gt 0 ]; then
        echo "Recent Profit Calculations:"
        grep "Estimated profit" "$LOG_FILE" | tail -5
    else
        echo "No profit calculations during this run"
    fi
    
} > "$SUMMARY_FILE"

echo -e "${GREEN}✅ Summary saved to: ${SUMMARY_FILE}${NC}"
echo ""
echo -e "${BLUE}📋 Quick Analysis:${NC}"
echo "  Opportunities: $OPPORTUNITIES"
echo "  Profit Calculations: $PROFIT_CALC"
echo "  Errors: $ERROR_COUNT"
echo "  Warnings: $WARNING_COUNT"
echo ""
echo -e "${BLUE}💡 To analyze logs in detail, run:${NC}"
echo "  ./scripts/analyze_dry_run_logs.sh $LOG_FILE"

