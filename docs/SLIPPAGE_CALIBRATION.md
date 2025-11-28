# Slippage Calculation Calibration Guide

## ⚠️ CRITICAL: CALIBRATION REQUIRED

The slippage calculation model uses **ESTIMATED multipliers** that **MUST be calibrated** against real mainnet liquidation data in production.

**Risk:** Using uncalibrated multipliers can lead to:
- Unprofitable liquidations (actual slippage > estimated)
- Missed opportunities (overly conservative estimates)
- Model uncertainty errors

**Solution:** 
1. **Recommended:** Enable Jupiter API for real-time slippage (`USE_JUPITER_API=true`)
2. **Alternative:** Calibrate multipliers after first 10-20 liquidations (see below)

## Current Implementation

### Size-Based Multipliers

The current model uses trade size-based multipliers to estimate DEX slippage:

- **Small trades (<$10k)**: 0.5x multiplier (lower slippage, better liquidity)
- **Medium trades ($10k-$100k)**: 0.6x multiplier (moderate slippage)
- **Large trades (>$100k)**: 0.8x multiplier (higher slippage, lower liquidity)

These multipliers are applied to `MAX_SLIPPAGE_BPS` to get estimated slippage.

### Formula

```rust
// Estimated DEX slippage
estimated_dex_slippage_bps = MAX_SLIPPAGE_BPS * size_multiplier

// Oracle confidence (Pyth ±1σ)
oracle_confidence_bps = (confidence / price) * 10_000

// Total slippage
total_slippage_bps = estimated_dex_slippage_bps + oracle_confidence_bps
final_slippage_bps = total_slippage_bps * SLIPPAGE_FINAL_MULTIPLIER
```

## Problems with Current Model

1. **Estimated Multipliers**: Size multipliers are conservative estimates, not based on real market data
2. **Market Variability**: Real slippage on Jupiter/Raydium may differ significantly from estimates
3. **Liquidity Changes**: DEX liquidity depth changes over time, affecting actual slippage

## Solutions

### Option 1: Jupiter API Integration (Recommended)

Use real-time slippage estimation from Jupiter API:

```bash
export USE_JUPITER_API=true
```

**Benefits:**
- Real-time market data
- Accurate price impact calculation
- Automatic adaptation to market conditions

**How it works:**
- When `USE_JUPITER_API=true` and a swap is needed, the bot queries Jupiter's quote API
- Gets actual price impact percentage for the swap
- Falls back to estimated slippage if API fails

**API Reference:**
- Jupiter Quote API: https://quote-api.jup.ag/v6/quote
- Documentation: https://docs.jup.ag/docs/apis/quote-api

### Option 2: Calibrate Multipliers from Real Data

**⚠️ IMPORTANT:** If Jupiter API is disabled, you **MUST** calibrate multipliers after the first 10-20 liquidations in production.

Measure actual slippage from successful liquidations and adjust multipliers:

1. **Collect Data (After 10-20 Liquidations):**
   ```bash
   # After successful liquidations, check transaction signatures from logs
   # Example log output:
   # "Liquidation transaction sent: 5j7s8K9L0M1N2O3P4Q5R6S7T8U9V0W1X2Y3Z4A5B6C7D8E9F0G1H2"
   
   # On Solscan, check each transaction:
   # https://solscan.io/tx/<signature>
   
   # For each transaction, record:
   # - Trade size (USD)
   # - Oracle price (from Pyth/Switchboard at time of liquidation)
   # - Execution price (from swap transaction)
   # - Price impact percentage (if available)
   ```

2. **Calculate Actual Slippage:**
   ```
   actual_slippage_bps = ((oracle_price - execution_price) / oracle_price) * 10_000
   ```

3. **Compare with Estimated:**
   ```
   estimated_slippage_bps = MAX_SLIPPAGE_BPS * size_multiplier
   difference = actual_slippage_bps - estimated_slippage_bps
   ```

4. **Adjust Multipliers:**
   ```bash
   # If actual slippage is consistently higher:
   export SLIPPAGE_MULTIPLIER_SMALL=0.6  # Increase from 0.5
   export SLIPPAGE_MULTIPLIER_MEDIUM=0.7  # Increase from 0.6
   export SLIPPAGE_MULTIPLIER_LARGE=0.9   # Increase from 0.8
   
   # If actual slippage is consistently lower:
   export SLIPPAGE_MULTIPLIER_SMALL=0.4  # Decrease from 0.5
   export SLIPPAGE_MULTIPLIER_MEDIUM=0.5  # Decrease from 0.6
   export SLIPPAGE_MULTIPLIER_LARGE=0.7  # Decrease from 0.8
   ```

## Pyth Confidence (Oracle Uncertainty)

**Important:** Pyth confidence is already ±1σ (68% confidence interval). 

- ✅ **Correct:** Use confidence directly: `confidence_bps = (confidence / price) * 10_000`
- ❌ **Incorrect:** Multiplying by Z-score (1.96) is statistically wrong

The current implementation correctly uses confidence directly without Z-score multiplication.

## Verification Steps

### 1. Enable Jupiter API (Recommended)

```bash
export USE_JUPITER_API=true
cargo run
```

Check logs for:
```
✅ Using Jupiter API slippage: 45 bps (estimated: 50 bps)
```

### 2. Measure Real Slippage

After executing a liquidation:

1. Get transaction signature from logs
2. Check on Solana Explorer: https://solscan.io/tx/<signature>
3. Compare:
   - Oracle price (from Pyth/Switchboard)
   - Execution price (from swap transaction)
   - Calculate actual slippage

### 3. Calibrate Multipliers

If not using Jupiter API, collect data from 10-20 successful liquidations:

- Group by trade size (small/medium/large)
- Calculate average actual slippage for each group
- Adjust multipliers to match actual slippage

### 4. Monitor and Adjust

Regularly review:
- Actual vs estimated slippage
- Profit margins (should be positive after all costs)
- Rejection rates (if too high, multipliers may be too conservative)

## Configuration

### Environment Variables

```bash
# Enable Jupiter API (recommended)
USE_JUPITER_API=true

# Size thresholds
SLIPPAGE_SIZE_SMALL_THRESHOLD_USD=10000
SLIPPAGE_SIZE_LARGE_THRESHOLD_USD=100000

# Size multipliers (calibrate from real data)
SLIPPAGE_MULTIPLIER_SMALL=0.5
SLIPPAGE_MULTIPLIER_MEDIUM=0.6
SLIPPAGE_MULTIPLIER_LARGE=0.8

# Safety margin
SLIPPAGE_FINAL_MULTIPLIER=1.1  # 10% safety margin

# Max slippage
MAX_SLIPPAGE_BPS=100  # 1% max slippage
```

## Expected Slippage Ranges

Based on typical DEX liquidity:

- **Small trades (<$10k)**: 0.1-0.3% (10-30 bps)
- **Medium trades ($10k-$100k)**: 0.3-0.6% (30-60 bps)
- **Large trades (>$100k)**: 0.6-1.0% (60-100 bps)

**Note:** These are rough estimates. Actual slippage varies significantly based on:
- Token pair liquidity
- Market volatility
- Time of day
- Network congestion

## References

- Jupiter API: https://docs.jup.ag/docs/apis/quote-api
- Pyth Confidence: https://docs.pyth.network/price-feeds/confidence-intervals
- Solana Explorer: https://solscan.io/

