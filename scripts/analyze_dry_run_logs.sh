#!/bin/bash

# Dry-Run Log Analysis Script
# Analyzes dry-run logs and provides detailed insights

set -e

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

if [ $# -eq 0 ]; then
    echo -e "${RED}Usage: $0 <log_file>${NC}"
    echo -e "${YELLOW}Example: $0 logs/dry_run_20241128_031500.log${NC}"
    exit 1
fi

LOG_FILE="$1"

if [ ! -f "$LOG_FILE" ]; then
    echo -e "${RED}Error: Log file not found: $LOG_FILE${NC}"
    exit 1
fi

echo -e "${BLUE}📊 Analyzing Dry-Run Logs${NC}"
echo -e "${BLUE}File: $LOG_FILE${NC}"
echo ""

# 1. WebSocket Connection Status
echo -e "${BLUE}=== 1. WebSocket Connection Status ===${NC}"
if grep -q "✅ WebSocket connected" "$LOG_FILE"; then
    echo -e "${GREEN}✅ WebSocket: Connected${NC}"
    WS_CONNECT_TIME=$(grep "✅ WebSocket connected" "$LOG_FILE" | head -1 | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}' | head -1 || echo "N/A")
    echo "   Connection Time: $WS_CONNECT_TIME"
else
    echo -e "${RED}❌ WebSocket: Not connected${NC}"
fi

if grep -q "✅ Subscribed to program accounts" "$LOG_FILE"; then
    echo -e "${GREEN}✅ Subscription: Active${NC}"
    SUB_TIME=$(grep "✅ Subscribed to program accounts" "$LOG_FILE" | head -1 | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}' | head -1 || echo "N/A")
    echo "   Subscription Time: $SUB_TIME"
else
    echo -e "${RED}❌ Subscription: Not active${NC}"
fi

# Check for WebSocket errors
WS_ERRORS=$(grep -c "WebSocket error\|WebSocket failed" "$LOG_FILE" 2>/dev/null || echo "0")
if [ "$WS_ERRORS" -gt 0 ]; then
    echo -e "${YELLOW}⚠️  WebSocket Errors: $WS_ERRORS${NC}"
    echo "   Recent WebSocket Errors:"
    grep "WebSocket error\|WebSocket failed" "$LOG_FILE" | tail -3 | sed 's/^/   /'
fi
echo ""

# 2. Opportunity Detection
echo -e "${BLUE}=== 2. Opportunity Detection ===${NC}"
OPPORTUNITIES=$(grep -c "Opportunity detected\|opportunity detected" "$LOG_FILE" 2>/dev/null || echo "0")
echo "Total Opportunities: $OPPORTUNITIES"

if [ "$OPPORTUNITIES" -gt 0 ]; then
    echo ""
    echo "Opportunity Details:"
    grep "Opportunity detected" "$LOG_FILE" | while IFS= read -r line; do
        # Extract key information
        ACCOUNT=$(echo "$line" | grep -oE 'account=[A-Za-z0-9]{32,44}' | cut -d= -f2 || echo "N/A")
        HF=$(echo "$line" | grep -oE 'health_factor=[0-9.]+' | cut -d= -f2 || echo "N/A")
        PROFIT=$(echo "$line" | grep -oE 'profit=\$[0-9.]+' | cut -d= -f2 || echo "N/A")
        TIME=$(echo "$line" | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}' | head -1 || echo "N/A")
        
        echo "   Time: $TIME"
        echo "   Account: ${ACCOUNT:0:8}...${ACCOUNT: -8}"
        echo "   Health Factor: $HF"
        echo "   Profit: $PROFIT"
        echo ""
    done | head -20
else
    echo -e "${YELLOW}⚠️  No opportunities detected${NC}"
    echo "   This is normal if:"
    echo "   - No accounts are below liquidation threshold"
    echo "   - All opportunities are below MIN_PROFIT_USD"
    echo "   - System is still initializing"
fi
echo ""

# 3. Profit Calculations
echo -e "${BLUE}=== 3. Profit Calculations ===${NC}"
PROFIT_CALC=$(grep -c "Estimated profit\|estimated profit" "$LOG_FILE" 2>/dev/null || echo "0")
echo "Total Profit Calculations: $PROFIT_CALC"

if [ "$PROFIT_CALC" -gt 0 ]; then
    echo ""
    echo "Profit Calculation Details:"
    grep "Estimated profit" "$LOG_FILE" | tail -5 | while IFS= read -r line; do
        PROFIT=$(echo "$line" | grep -oE '\$[0-9]+\.[0-9]+' | head -1 || echo "N/A")
        TIME=$(echo "$line" | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}' | head -1 || echo "N/A")
        echo "   $TIME: $PROFIT"
    done
    
    # Calculate average profit
    AVG_PROFIT=$(grep "Estimated profit" "$LOG_FILE" | grep -oE '\$[0-9]+\.[0-9]+' | sed 's/\$//' | awk '{sum+=$1; count++} END {if(count>0) printf "%.2f", sum/count; else print "0"}' || echo "0")
    if [ "$AVG_PROFIT" != "0" ]; then
        echo ""
        echo "   Average Profit: \$$AVG_PROFIT"
    fi
else
    echo -e "${YELLOW}⚠️  No profit calculations${NC}"
fi
echo ""

# 4. Fee Breakdown Analysis
echo -e "${BLUE}=== 4. Fee Breakdown Analysis ===${NC}"
FEE_BREAKDOWN=$(grep -c "Fee breakdown\|fee breakdown" "$LOG_FILE" 2>/dev/null || echo "0")
echo "Fee Breakdowns: $FEE_BREAKDOWN"

if [ "$FEE_BREAKDOWN" -gt 0 ]; then
    echo ""
    echo "Recent Fee Breakdowns:"
    grep -A 5 "Fee breakdown" "$LOG_FILE" | tail -15 | sed 's/^/   /'
else
    echo -e "${YELLOW}⚠️  No fee breakdowns found${NC}"
fi
echo ""

# 5. Slippage Estimation
echo -e "${BLUE}=== 5. Slippage Estimation ===${NC}"
SLIPPAGE_EST=$(grep -c "slippage\|Slippage" "$LOG_FILE" 2>/dev/null || echo "0")
echo "Slippage Mentions: $SLIPPAGE_EST"

if [ "$SLIPPAGE_EST" -gt 0 ]; then
    echo ""
    echo "Recent Slippage Estimates:"
    grep -i "slippage" "$LOG_FILE" | tail -5 | sed 's/^/   /'
else
    echo -e "${YELLOW}⚠️  No slippage estimates found${NC}"
fi
echo ""

# 6. Error Analysis
echo -e "${BLUE}=== 6. Error Analysis ===${NC}"
ERROR_COUNT=$(grep -ci "error" "$LOG_FILE" 2>/dev/null || echo "0")
echo "Total Errors: $ERROR_COUNT"

if [ "$ERROR_COUNT" -gt 0 ]; then
    echo ""
    echo "Error Types:"
    grep -i "error" "$LOG_FILE" | grep -oE 'error[^:]*' | sort | uniq -c | sort -rn | head -10 | sed 's/^/   /'
    
    echo ""
    echo "Recent Errors:"
    grep -i "error" "$LOG_FILE" | tail -10 | sed 's/^/   /'
else
    echo -e "${GREEN}✅ No errors found${NC}"
fi
echo ""

# 7. Warning Analysis
echo -e "${BLUE}=== 7. Warning Analysis ===${NC}"
WARNING_COUNT=$(grep -ci "warn" "$LOG_FILE" 2>/dev/null || echo "0")
echo "Total Warnings: $WARNING_COUNT"

if [ "$WARNING_COUNT" -gt 0 ]; then
    echo ""
    echo "Warning Types:"
    grep -i "warn" "$LOG_FILE" | grep -oE 'warn[^:]*' | sort | uniq -c | sort -rn | head -10 | sed 's/^/   /'
else
    echo -e "${GREEN}✅ No warnings found${NC}"
fi
echo ""

# 8. System Health
echo -e "${BLUE}=== 8. System Health ===${NC}"
HEALTH_CHECKS=$(grep -c "Health check\|health check" "$LOG_FILE" 2>/dev/null || echo "0")
echo "Health Checks: $HEALTH_CHECKS"

if [ "$HEALTH_CHECKS" -gt 0 ]; then
    HEALTHY=$(grep -c "Health check passed\|health check passed" "$LOG_FILE" 2>/dev/null || echo "0")
    UNHEALTHY=$(grep -c "Health check failed\|health check failed" "$LOG_FILE" 2>/dev/null || echo "0")
    echo "   Healthy: $HEALTHY"
    echo "   Unhealthy: $UNHEALTHY"
else
    echo -e "${YELLOW}⚠️  No health check data${NC}"
fi
echo ""

# 9. Performance Metrics
echo -e "${BLUE}=== 9. Performance Metrics ===${NC}"
METRICS=$(grep -c "Metrics:\|metrics:" "$LOG_FILE" 2>/dev/null || echo "0")
echo "Metrics Reports: $METRICS"

if [ "$METRICS" -gt 0 ]; then
    echo ""
    echo "Recent Metrics:"
    grep "Metrics:" "$LOG_FILE" | tail -5 | sed 's/^/   /'
else
    echo -e "${YELLOW}⚠️  No metrics data${NC}"
fi
echo ""

# 10. Summary
echo -e "${BLUE}=== 10. Summary ===${NC}"
echo "Opportunities Detected: $OPPORTUNITIES"
echo "Profit Calculations: $PROFIT_CALC"
echo "Fee Breakdowns: $FEE_BREAKDOWN"
echo "Slippage Estimates: $SLIPPAGE_EST"
echo "Errors: $ERROR_COUNT"
echo "Warnings: $WARNING_COUNT"
echo "Health Checks: $HEALTH_CHECKS"
echo ""

# Overall Status
if [ "$ERROR_COUNT" -eq 0 ] && [ "$OPPORTUNITIES" -ge 0 ]; then
    echo -e "${GREEN}✅ Overall Status: HEALTHY${NC}"
    if [ "$OPPORTUNITIES" -eq 0 ]; then
        echo -e "${YELLOW}   Note: No opportunities detected (this may be normal)${NC}"
    fi
else
    echo -e "${YELLOW}⚠️  Overall Status: REVIEW NEEDED${NC}"
    if [ "$ERROR_COUNT" -gt 0 ]; then
        echo -e "${RED}   Action: Review errors above${NC}"
    fi
fi
echo ""

