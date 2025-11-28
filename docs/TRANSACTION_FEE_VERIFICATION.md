# Transaction Fee Verification Guide

## ⚠️ VERIFICATION REQUIRED

Transaction fee calculation needs to be validated against real mainnet transactions.

## Current Implementation

### Fee Calculation Formula

```rust
// Base fee: 5,000 lamports (Solana standard)
// Priority fee: (compute_units * priority_fee_per_cu) / 1_000_000
// Total fee = base_fee + priority_fee
```

### Default Values

- **Base Fee**: 5,000 lamports (0.000005 SOL)
- **Compute Units**: 200,000 (configurable via `LIQUIDATION_COMPUTE_UNITS`)
- **Priority Fee per CU**: 1,000 micro-lamports (configurable via `PRIORITY_FEE_PER_CU`)
- **Estimated Total**: ~5,200 lamports (0.0000052 SOL = ~$0.00078 USD at $150 SOL)

## Verification Steps

### 1. Dry-Run Mode Testing

Run the bot in dry-run mode to see estimated fees:

```bash
DRY_RUN=true cargo run
```

Check logs for fee breakdown:
```
💰 Transaction Fee Breakdown: 
   base=5000 lamports (0.000005000 SOL), 
   priority=200 lamports (0.000000200 SOL, 200000 CU × 1000 μlamports/CU), 
   total=5200 lamports (0.000005200 SOL = $0.000780 USD)
```

### 2. Mainnet Transaction Verification

After executing a real liquidation transaction:

1. **Get Transaction Signature**
   - Transaction signature is logged: `✅ Liquidation transaction sent: <signature>`
   - Or check logs for: `Liquidation transaction sent: <signature>`

2. **Check Transaction on Solana Explorer**
   - Visit: https://solscan.io/tx/<signature>
   - Or: https://explorer.solana.com/tx/<signature>
   - Look for "Transaction Fee" section

3. **Compare with Estimated Fee**
   - Actual fee should be close to estimated fee (~5,200 lamports)
   - If difference > 10%, adjust config values:
     - `LIQUIDATION_COMPUTE_UNITS` - if actual CU differs
     - `PRIORITY_FEE_PER_CU` - if priority fee differs
     - `BASE_TRANSACTION_FEE_LAMPORTS` - should always be 5,000

### 3. Compute Unit Verification

Check if 200K compute units is sufficient:

1. **Check Transaction Metadata**
   - On Solana Explorer, check "Compute Units Consumed"
   - If > 200K, increase `LIQUIDATION_COMPUTE_UNITS` config
   - If < 200K, you can decrease it (but keep some margin)

2. **Common Compute Unit Ranges**
   - Simple liquidation: ~150K-180K CU
   - Complex liquidation (multiple assets): ~180K-200K CU
   - With swaps: ~200K-250K CU

## Expected Fee Ranges

### Normal Conditions
- **Base Fee**: 5,000 lamports (fixed)
- **Priority Fee**: 200-500 lamports (depending on network congestion)
- **Total**: ~5,200-5,500 lamports (~$0.00078-0.00083 USD)

### High Congestion
- **Base Fee**: 5,000 lamports (fixed)
- **Priority Fee**: 500-2,000 lamports (higher priority needed)
- **Total**: ~5,500-7,000 lamports (~$0.00083-0.00105 USD)

## Config Adjustment

If actual fees differ significantly:

1. **Update `LIQUIDATION_COMPUTE_UNITS`**
   ```bash
   export LIQUIDATION_COMPUTE_UNITS=250000  # Increase if transactions fail
   ```

2. **Update `PRIORITY_FEE_PER_CU`**
   ```bash
   export PRIORITY_FEE_PER_CU=2000  # Increase during high congestion
   ```

3. **Keep `BASE_TRANSACTION_FEE_LAMPORTS` at 5,000**
   - This is Solana's standard base fee
   - Should not be changed

## Verification Checklist

- [ ] Run dry-run mode and verify fee breakdown logs
- [ ] Execute real liquidation transaction
- [ ] Check transaction fee on Solana Explorer
- [ ] Compare actual vs estimated fee (should be within 10%)
- [ ] Verify compute units consumed (should be < configured limit)
- [ ] Adjust config if needed
- [ ] Re-run verification after config changes

## Notes

- **Oracle Read Fee**: ❌ REMOVED - Solana doesn't charge separate fees for account reads
- **Base Fee Covers All**: Base transaction fee covers all account reads, including oracle accounts
- **Priority Fee is Optional**: Can be set to 0, but transaction may be slower during congestion

## References

- Solana Fees: https://docs.solana.com/developing/programming-model/runtime#transaction-fees
- Compute Budget: https://docs.solana.com/developing/programming-model/runtime#compute-budget

