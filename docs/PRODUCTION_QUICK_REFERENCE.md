# 🚀 Production Quick Reference

Hızlı komut referansı için bu sayfayı kullanın.

## 📋 Tüm Testleri Çalıştır

```bash
./scripts/production_checklist.sh
```

## 🔍 Tekil Testler

### 1. Reserve Struct Validation
```bash
cargo run --bin validate_reserve -- --reserve BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw
```

### 2. Obligation Parsing Test
```bash
cargo run --bin find_my_obligation
```

### 3. System Integration Test
```bash
cargo run --bin validate_system
# Detaylı çıktı için:
cargo run --bin validate_system -- --verbose
```

## 🧪 Dry-Run Test (24 Saat)

```bash
DRY_RUN=true cargo run
```

**Log'larda kontrol edin:**
- `✅ WebSocket connected`
- `✅ Subscribed to program accounts`
- Opportunity detection
- Profit calculation
- Fee breakdown
- Slippage estimation

## 💰 Small Capital Test

```bash
DRY_RUN=false MIN_PROFIT_USD=1.0 cargo run
```

**⚠️ UYARI:** Bu gerçek transaction'lar gönderir!

İlk 5-10 transaction'ı dikkatle izleyin.

## ⚙️ Production Ayarları

### Minimum Production Settings

```bash
# .env dosyasına ekleyin veya export edin:

# RPC
RPC_HTTP_URL=https://api.mainnet-beta.solana.com
RPC_WS_URL=wss://api.mainnet-beta.solana.com

# Profit
MIN_PROFIT_USD=5.0  # Minimum (önerilen: 5.0-10.0)

# Dry Run
DRY_RUN=false  # Production için false

# Slippage
USE_JUPITER_API=true  # Önerilen

# Polling (fallback için)
POLL_INTERVAL_MS=10000  # Free RPC için minimum

# Wallet
WALLET_PATH=./secret/bot-wallet.json
MIN_RESERVE_LAMPORTS=1000000  # 0.001 SOL
```

### Premium RPC Settings

```bash
# Premium RPC kullanıyorsanız:
RPC_HTTP_URL=https://your-premium-rpc-url.com
RPC_WS_URL=wss://your-premium-rpc-url.com
POLL_INTERVAL_MS=2000  # Premium RPC için OK
```

## 🚀 Production'da Başlatma

```bash
# 1. Ayarları kontrol edin
./scripts/production_checklist.sh

# 2. Production'da başlatın
DRY_RUN=false MIN_PROFIT_USD=5.0 cargo run
```

## 📊 Log Kontrolleri

### WebSocket Bağlantısı
```bash
# Log'larda şunu görmelisiniz:
grep "WebSocket connected" logs/*.log
grep "Subscribed to program accounts" logs/*.log
```

### Opportunity Detection
```bash
grep "Opportunity detected" logs/*.log
```

### Transaction Sonuçları
```bash
grep "Transaction successful" logs/*.log
grep "Transaction failed" logs/*.log
```

## 🔧 Sorun Giderme

### WebSocket Bağlantı Sorunu
```bash
# RPC_WS_URL'i kontrol edin
echo $RPC_WS_URL

# WebSocket endpoint'ini test edin
curl -i -N -H "Connection: Upgrade" -H "Upgrade: websocket" $RPC_WS_URL
```

### Rate Limit Sorunu
```bash
# Free RPC kullanıyorsanız:
POLL_INTERVAL_MS=10000

# Premium RPC kullanın veya WebSocket kullanın (varsayılan)
```

### Transaction Başarısızlığı
```bash
# Wallet balance'ı kontrol edin
solana balance <YOUR_WALLET_ADDRESS>

# Priority fee'yi artırın
PRIORITY_FEE_PER_CU=2000

# Compute units'ı artırın
DEFAULT_COMPUTE_UNITS=300000
```

## 📈 Monitoring

### Health Check
```bash
# Health manager her 30 saniyede bir log yazar
# Log'larda şunu görmelisiniz:
grep "Health check" logs/*.log
```

### Performance Metrics
```bash
# Performance tracker her 10 transaction'da bir log yazar
grep "Metrics:" logs/*.log
```

## 🎯 Checklist

Production'a geçmeden önce:

- [ ] `./scripts/production_checklist.sh` çalıştırıldı ve tüm testler passed
- [ ] 24-hour dry-run test tamamlandı
- [ ] WebSocket bağlantısı doğrulandı
- [ ] MIN_PROFIT_USD >= 5.0
- [ ] Wallet balance yeterli (SOL + debt tokens)
- [ ] Small capital test tamamlandı (5-10 transaction)
- [ ] Transaction success rate > 95%
- [ ] Profit accuracy doğrulandı

## 📚 Detaylı Dokümanlar

- [Production Checklist](PRODUCTION_CHECKLIST.md) - Detaylı checklist
- [Slippage Calibration](SLIPPAGE_CALIBRATION.md) - Slippage kalibrasyonu
- [Transaction Fee Verification](TRANSACTION_FEE_VERIFICATION.md) - Fee doğrulama

---

**İyi şanslar! 🚀**

