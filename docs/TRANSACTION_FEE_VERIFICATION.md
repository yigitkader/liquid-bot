### Transaction Fee Verification Guide

**Amaç:**  
`math.rs::calculate_transaction_fee_usd` fonksiyonunun ürettiği tahmini ücretin, gerçek mainnet işlem ücretleriyle uyumlu olduğunu doğrulamak (±10% tolerans).

---

### 1. Hazırlık

- **Mod:** Önce `DRY_RUN=true` ile botu çalıştırın ve fee loglarını gözlemleyin.
- **RPC:** Mainnet RPC endpoint'i kullanın (mümkünse premium sağlayıcı).
- **Config:** Aşağıdaki alanların doğru olduğundan emin olun:
  - `liquidation_compute_units`
  - `priority_fee_per_cu`
  - `base_transaction_fee_lamports`

---

### 2. İlk Gerçek Likidasyon ile Doğrulama Adımları

1. **Dry-run ile fee tahminini kontrol edin**
   - Komut:
     - `DRY_RUN=true cargo run`
   - Loglarda şu satırları bulun:
     - `💰 Transaction Fee Breakdown: ...`
   - Buradaki `total=... lamports (... SOL = $... USD)` kısmı sizin **tahmini ücretinizdir**.

2. **Gerçek bir liquidation işlemi gönderin**
   - Komut:
     - `DRY_RUN=false MIN_PROFIT_USD=1.0 cargo run`
   - Executor loglarında transaction imzasını göreceksiniz:
     - `Liquidation transaction sent: <SIGNATURE>`

3. **Solscan'de gerçek fee'yi kontrol edin**
   - `https://solscan.io/tx/<SIGNATURE>` adresine gidin.
   - `Fee` alanını not alın (SOL veya lamports olarak).

4. **Karşılaştırma**
   - Hesaplama:
     - `diff = |fee_actual - fee_estimated|`
     - `ratio = diff / fee_actual`
   - Kriter:
     - `ratio <= 0.10` (yani fark ≤ %10) ise **kabul edilebilir**.
     - Daha büyük fark varsa:
       - `liquidation_compute_units` veya `priority_fee_per_cu` değerleriniz gerçeklerden sapmış olabilir.

---

### 3. Sapma Durumunda Ayarlama

- **Gerçek fee > Tahmin**:
  - `liquidation_compute_units` veya `priority_fee_per_cu` çok düşük olabilir.
  - Adım adım artırın:
    - Önce gerçek işlemlerde görülen `compute units` tüketimini inceleyin.
    - Config’teki `liquidation_compute_units` değerini buna yaklaştırın (+ bir güvenlik payı).
    - Gerekirse `priority_fee_per_cu` değerini güncelleyin.

- **Gerçek fee < Tahmin**:
  - Tahmin çok konservatif olabilir (bu genelde problem değildir).
  - İsterseniz `liquidation_compute_units` veya `priority_fee_per_cu` biraz azaltarak daha gerçekçi bir tahmin elde edebilirsiniz.

---

### 4. Formül Referansı

`calculate_transaction_fee_usd` içinde kullanılan formül:

- **Priority fee (lamports):**
  - `priority_fee_lamports = (compute_units * priority_fee_per_cu) / 1_000_000`
  - `priority_fee_per_cu` mikro-lamports cinsindendir (μlamports/CU).

- **Toplam fee (lamports):**
  - `total_fee_lamports = base_fee_lamports + priority_fee_lamports`

- **USD karşılığı:**
  - `total_fee_sol = total_fee_lamports / 1_000_000_000`
  - `total_fee_usd = total_fee_sol * sol_price_usd`

**Notlar:**
- Oracle account read'leri için **ayrı bir ücret yoktur**; hepsi base transaction fee’ye dahildir.
- Bu nedenle `_oracle_read_fee_lamports` ve `_oracle_accounts_read` parametreleri **deprecated** bırakılmıştır, hesapta kullanılmaz.

---

### 5. Kabul Kriteri (Production)

- En az 1–3 gerçek liquidation işlemi için:
  - Tahmini ücret ile gerçek ücret arasındaki fark **%10'dan küçük veya eşit** olmalıdır.
- Bu sağlandığında:
  - Config değerleriniz ve `calculate_transaction_fee_usd` formülü **production için güvenli** kabul edilebilir.

# Transaction Fee Verification Guide

## ⚠️ CRITICAL: VERIFICATION REQUIRED

Transaction fee calculation **MUST be validated** against real mainnet transactions after the first liquidation.

**Risk:** Unverified fee calculations can lead to:
- Incorrect profit estimates
- Transaction failures (insufficient compute units)
- Missed opportunities (overestimated fees)

**Action Required:** After first real liquidation, verify fees on Solscan and adjust config if needed.

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

### 2. Mainnet Transaction Verification (REQUIRED After First Liquidation)

**⚠️ IMPORTANT:** After your first real liquidation transaction, you **MUST** verify the fee calculation.

1. **Get Transaction Signature**
   - Transaction signature is logged: `✅ Liquidation transaction sent: <signature>`
   - Or check logs for: `Liquidation transaction sent: <signature>`
   - Example: `5j7s8K9L0M1N2O3P4Q5R6S7T8U9V0W1X2Y3Z4A5B6C7D8E9F0G1H2`

2. **Check Transaction on Solana Explorer**
   - Visit: https://solscan.io/tx/<signature>
   - Or: https://explorer.solana.com/tx/<signature>
   - Look for "Transaction Fee" section
   - Record: Actual fee (lamports), Compute Units Consumed

3. **Compare with Estimated Fee**
   - **Expected tolerance:** ±10% difference is acceptable
   - **If difference > 10%:** Adjust config values immediately:
     ```bash
     # If actual CU > configured:
     export LIQUIDATION_COMPUTE_UNITS=250000  # Increase from 200000
     
     # If actual priority fee > estimated:
     export PRIORITY_FEE_PER_CU=2000  # Increase from 1000 (high congestion)
     
     # Base fee should always be 5,000 (do not change)
     ```
   - **If difference < 10%:** Fee calculation is accurate ✅

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

