# 📋 Production Checklist - Solana Liquidation Bot

Bu doküman, production'a geçmeden önce yapılması gereken tüm testleri ve kontrolleri içerir.

## 🎯 Genel Bakış

Production'a geçmeden önce aşağıdaki adımları **mutlaka** tamamlayın:

1. ✅ Struct Validation Test
2. ✅ Obligation Parsing Test
3. ✅ System Integration Test
4. ✅ Dry-Run Test (24 saat)
5. ✅ Small Capital Test
6. ✅ Configuration Checklist

## 🚀 Hızlı Başlangıç

Tüm testleri otomatik olarak çalıştırmak için:

```bash
./scripts/production_checklist.sh
```

Bu script tüm testleri çalıştırır ve sonuçları özetler.

---

## 1️⃣ Struct Validation Test

**Amaç:** Reserve struct'ının gerçek mainnet verileriyle uyumlu olduğunu doğrulamak.

### Test Komutu

```bash
cargo run --bin validate_reserve -- \
  --reserve BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw
```

### Beklenen Çıktı

```
✅ SUCCESS: Reserve account parsed successfully!
✅ Struct structure matches the real Solend IDL!
   You can safely use this struct in production.
```

### Başarısız Olursa

Eğer test başarısız olursa:
1. `src/protocols/solend_reserve.rs` dosyasını kontrol edin
2. Resmi Solend SDK'yı kontrol edin: https://github.com/solendprotocol/solend-program
3. IDL'yi güncelleyin: `./scripts/fetch_solend_idl.sh`

---

## 2️⃣ Obligation Parsing Test

**Amaç:** Obligation struct'ının gerçek mainnet obligation hesaplarını parse edebildiğini doğrulamak.

### Test Komutu

```bash
cargo run --bin find_my_obligation
```

### Beklenen Çıktı

**Seçenek 1:** Aktif obligation varsa:
```
✅ Found 1 active obligation account(s)!
✅ OBLIGATION STRUCT VALIDATION SUCCESSFUL!
   The obligation struct successfully parsed your real mainnet obligation account(s).
```

**Seçenek 2:** Aktif obligation yoksa (normal):
```
⚠️  No active obligation accounts found
   This is normal if you don't have any active positions in Solend
```

### Notlar

- Eğer Solend'de pozisyonunuz yoksa, bu test yine de struct'ın doğru olduğunu doğrular
- Test, obligation PDA derivation'ı da doğrular

---

## 3️⃣ System Integration Test

**Amaç:** Tüm sistem bileşenlerinin birlikte çalıştığını doğrulamak.

### Test Komutu

```bash
cargo run --bin validate_system
```

### Detaylı Çıktı İçin

```bash
cargo run --bin validate_system -- --verbose
```

### Test Edilen Bileşenler

- ✅ Configuration correctness
- ✅ Address validity (program IDs, markets, reserves)
- ✅ Account parsing (reserve, obligation)
- ✅ PDA derivation (lending market authority, obligation)
- ✅ Instruction format correctness
- ✅ Oracle account reading
- ✅ System integration integrity

### Beklenen Çıktı

```
✅ ALL TESTS PASSED! (X/X)
   System is ready for production use.
```

---

## 4️⃣ Dry-Run Test (24 Saat)

**Amaç:** Bot'un gerçek transaction göndermeden 24 saat boyunca çalıştığını ve opportunity detection'ın doğru çalıştığını doğrulamak.

### Test Komutu

```bash
DRY_RUN=true cargo run
```

### İzlenmesi Gerekenler

Log'larda şunları kontrol edin:

#### ✅ WebSocket Bağlantısı

```
✅ WebSocket connected
✅ Subscribed to program accounts (subscription ID: X)
```

**Önemli:** Eğer bu log'ları görmüyorsanız:
- WebSocket bağlantısı başarısız olmuş olabilir
- RPC polling fallback'e düşmüş olabilir
- `RPC_WS_URL` ayarını kontrol edin

#### ✅ Opportunity Detection

```
🔍 Opportunity detected: account=..., health_factor=0.95, profit=$X.XX
```

#### ✅ Profit Calculation

```
💰 Estimated profit: $X.XX
   - Liquidation bonus: $X.XX
   - Transaction fees: $X.XX
   - DEX fees: $X.XX
   - Slippage: $X.XX
```

#### ✅ Fee Breakdown

```
📊 Fee breakdown:
   - Base transaction fee: X lamports
   - Priority fee: X lamports
   - DEX fee: X bps
   - Total fees: $X.XX
```

#### ✅ Slippage Estimation

**Jupiter API kullanılıyorsa:**
```
📡 Jupiter API slippage: X bps
```

**Manuel estimation kullanılıyorsa:**
```
📊 Estimated slippage: X bps (size: $X.XX, multiplier: X.XX)
```

### Süre

**Minimum 24 saat** çalıştırın ve:
- Opportunity detection'ın düzenli çalıştığını doğrulayın
- Profit calculation'ların mantıklı olduğunu kontrol edin
- Slippage estimation'ların makul aralıkta olduğunu doğrulayın
- Hata log'ları olmadığını kontrol edin

---

## 5️⃣ Small Capital Test

**Amaç:** Küçük sermaye ile gerçek transaction'ları test etmek.

### Test Komutu

```bash
DRY_RUN=false MIN_PROFIT_USD=1.0 cargo run
```

### ⚠️ UYARI

Bu komut **GERÇEK transaction'lar** gönderir! Küçük sermaye ile test edin (ör. $100).

### İzlenmesi Gerekenler

İlk 5-10 transaction'ı dikkatle izleyin:

1. **Transaction başarı oranı:** %100'e yakın olmalı
2. **Profit accuracy:** Gerçek profit, estimated profit'e yakın olmalı
3. **Slippage accuracy:** Gerçek slippage, estimated slippage'e yakın olmalı
4. **Fee accuracy:** Gerçek fee'ler, estimated fee'lere yakın olmalı

### Transaction Sonrası Kontrol

Her transaction'dan sonra:

1. **Solscan'de transaction'ı kontrol edin:**
   ```bash
   # Transaction signature'ı log'lardan alın ve Solscan'de kontrol edin
   https://solscan.io/tx/<SIGNATURE>
   ```

2. **Gerçek profit'i hesaplayın:**
   - Liquidation bonus
   - Transaction fees (gerçek)
   - DEX fees (gerçek)
   - Slippage (gerçek)

3. **Estimated profit ile karşılaştırın:**
   - Fark %10'dan az olmalı
   - Eğer fark büyükse, slippage multiplier'ları kalibre edin

### Slippage Calibration

İlk 10-20 liquidation'dan sonra:

1. Gerçek slippage'i ölçün (Solscan'den)
2. Estimated slippage ile karşılaştırın
3. `SLIPPAGE_MULTIPLIER_SMALL`, `SLIPPAGE_MULTIPLIER_MEDIUM`, `SLIPPAGE_MULTIPLIER_LARGE` değerlerini ayarlayın

Detaylı bilgi için: `docs/SLIPPAGE_CALIBRATION.md`

---

## 6️⃣ Configuration Checklist

### WebSocket Bağlantısı

**Kontrol:** Log'larda şunu görmelisiniz:
```
✅ WebSocket connected
✅ Subscribed to program accounts
```

**Eğer görmüyorsanız:**
- `RPC_WS_URL` ayarını kontrol edin
- WebSocket endpoint'inin erişilebilir olduğunu doğrulayın
- Firewall/proxy ayarlarını kontrol edin

### RPC Endpoint

**Free RPC (api.mainnet-beta.solana.com):**
- `POLL_INTERVAL_MS=10000` (minimum) - RPC polling fallback için
- WebSocket varsayılan olarak kullanılır (önerilen)

**Premium RPC (Helius, Triton, QuickNode, vb.):**
- `POLL_INTERVAL_MS=2000-5000` OK - RPC polling fallback için
- WebSocket varsayılan olarak kullanılır (önerilen)

### MIN_PROFIT_USD

**Test için:**
```bash
MIN_PROFIT_USD=1.0
```

**Production için:**
```bash
MIN_PROFIT_USD=5.0  # Minimum (önerilen)
MIN_PROFIT_USD=10.0 # Güvenli (daha az transaction, daha yüksek profit)
```

**Neden?**
- Transaction fees: ~$0.1-0.5
- Gas fees: ~$0.01-0.1
- Slippage: değişken
- Minimum $5 profit, transaction cost'ları karşılamak için yeterli

### Slippage Multipliers

**Jupiter API kullanıyorsanız:**
```bash
USE_JUPITER_API=true
```
Bu durumda slippage multiplier'lar otomatik olarak kullanılır.

**Manuel calibration ise:**
İlk 10-20 liquidation'dan sonra:
1. Gerçek slippage'i ölçün
2. Estimated slippage ile karşılaştırın
3. Multiplier'ları ayarlayın:
   ```bash
   SLIPPAGE_MULTIPLIER_SMALL=0.5    # < $10k trades
   SLIPPAGE_MULTIPLIER_MEDIUM=0.6   # $10k - $100k trades
   SLIPPAGE_MULTIPLIER_LARGE=0.8    # > $100k trades
   ```

### Wallet Balance

**Kontrol edin:**
- SOL balance: Transaction fees için yeterli olmalı (minimum 0.1 SOL)
- Debt token balances: USDC, USDT, vb. (liquidation için gerekli)
- `MIN_RESERVE_LAMPORTS=1000000` (0.001 SOL) - transaction fees için rezerve

**Kontrol komutu:**
```bash
solana balance <YOUR_WALLET_ADDRESS>
```

---

## 📊 Production Settings Özeti

### Önerilen Production Ayarları

```bash
# RPC Configuration
RPC_HTTP_URL=https://api.mainnet-beta.solana.com  # veya premium RPC
RPC_WS_URL=wss://api.mainnet-beta.solana.com      # veya premium RPC

# Profit Threshold
MIN_PROFIT_USD=5.0  # Minimum (önerilen: 5.0-10.0)

# Dry Run
DRY_RUN=false  # Production için false

# Slippage
USE_JUPITER_API=true  # Önerilen (real-time slippage)
# veya
USE_JUPITER_API=false  # Manuel calibration gerekli

# Polling (fallback için)
POLL_INTERVAL_MS=10000  # Free RPC için minimum
# veya
POLL_INTERVAL_MS=2000   # Premium RPC için OK

# Wallet
WALLET_PATH=./secret/bot-wallet.json
MIN_RESERVE_LAMPORTS=1000000  # 0.001 SOL
```

---

## ✅ Final Checklist

Production'a geçmeden önce:

- [ ] Struct validation test passed
- [ ] Obligation parsing test passed
- [ ] System integration test passed
- [ ] 24-hour dry-run test completed
- [ ] WebSocket connection verified in logs
- [ ] RPC endpoint configured correctly
- [ ] MIN_PROFIT_USD set to production-safe value (>= 5.0)
- [ ] Slippage multipliers calibrated (if not using Jupiter API)
- [ ] Wallet has sufficient balance (SOL + debt tokens)
- [ ] Small capital test completed (5-10 transactions)
- [ ] Transaction success rate > 95%
- [ ] Profit accuracy verified (estimated vs actual)
- [ ] Slippage accuracy verified (estimated vs actual)

---

## 🆘 Sorun Giderme

### WebSocket Bağlantı Sorunu

**Sorun:** "WebSocket connected" log'u görünmüyor

**Çözüm:**
1. `RPC_WS_URL` ayarını kontrol edin
2. WebSocket endpoint'inin erişilebilir olduğunu doğrulayın
3. Firewall/proxy ayarlarını kontrol edin
4. RPC polling fallback otomatik olarak devreye girer

### Rate Limit Sorunu

**Sorun:** "Rate limit error" log'ları görünüyor

**Çözüm:**
1. WebSocket kullanın (varsayılan, önerilen)
2. Eğer RPC polling kullanıyorsanız:
   - Free RPC: `POLL_INTERVAL_MS=10000` (minimum)
   - Premium RPC: `POLL_INTERVAL_MS=2000-5000` OK

### Transaction Başarısızlığı

**Sorun:** Transaction'lar başarısız oluyor

**Çözüm:**
1. Wallet balance'ı kontrol edin (SOL + debt tokens)
2. Priority fee'yi artırın: `PRIORITY_FEE_PER_CU=2000`
3. Compute units'ı artırın: `DEFAULT_COMPUTE_UNITS=300000`
4. Transaction log'larını kontrol edin

### Profit Accuracy Sorunu

**Sorun:** Gerçek profit, estimated profit'ten çok farklı

**Çözüm:**
1. Slippage multiplier'ları kalibre edin
2. Jupiter API kullanın: `USE_JUPITER_API=true`
3. Fee estimation'ları kontrol edin
4. Oracle confidence interval'ları kontrol edin

---

## 📚 İlgili Dokümanlar

- [Slippage Calibration Guide](SLIPPAGE_CALIBRATION.md)
- [Transaction Fee Verification](TRANSACTION_FEE_VERIFICATION.md)
- [Balance Reservation](BALANCE_RESERVATION.md)
- [Code Flow](CODE_FLOW.md)

---

## 🎉 Production'a Hazır!

Tüm checklist'i tamamladıysanız, production'a geçmeye hazırsınız!

```bash
DRY_RUN=false MIN_PROFIT_USD=5.0 cargo run
```

**İyi şanslar! 🚀**

