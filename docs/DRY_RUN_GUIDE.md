# 🧪 Dry-Run Test Kılavuzu

Bu kılavuz, production'a geçmeden önce dry-run testlerini nasıl çalıştıracağınızı ve logları nasıl analiz edeceğinizi açıklar.

## 📋 Ön Hazırlık

Dry-run testine başlamadan önce:

1. ✅ Production checklist'i tamamlayın:
   ```bash
   ./scripts/production_checklist.sh
   ```

2. ✅ `.env` dosyasını kontrol edin:
   ```bash
   grep -E "(DRY_RUN|MIN_PROFIT_USD|WALLET_PATH)" .env
   ```

3. ✅ `DRY_RUN=true` olduğundan emin olun

## 🚀 Dry-Run'u Başlatma

### Yöntem 1: Otomatik Script (Önerilen)

```bash
./scripts/run_dry_run.sh
```

Bu script:
- Log dosyasını otomatik oluşturur (`logs/dry_run_YYYYMMDD_HHMMSS.log`)
- Tüm çıktıyı hem ekrana hem dosyaya yazar
- Test bittiğinde otomatik özet oluşturur

### Yöntem 2: Manuel

```bash
# Log dosyası ile
DRY_RUN=true cargo run --bin liquid-bot 2>&1 | tee logs/dry_run_$(date +%Y%m%d_%H%M%S).log

# Sadece ekranda görmek için
DRY_RUN=true cargo run --bin liquid-bot
```

## 📊 Log Analizi

### Otomatik Analiz

Test bittikten sonra:

```bash
./scripts/analyze_dry_run_logs.sh logs/dry_run_YYYYMMDD_HHMMSS.log
```

Bu script şunları analiz eder:
- ✅ WebSocket bağlantı durumu
- 🎯 Tespit edilen opportunity'ler
- 💰 Profit hesaplamaları
- 💸 Fee breakdown'ları
- 📉 Slippage tahminleri
- ❌ Hatalar
- ⚠️ Uyarılar
- 💚 Sistem sağlığı
- 📈 Performans metrikleri

### Real-Time Monitoring

Başka bir terminal'de:

```bash
./scripts/monitor_dry_run.sh logs/dry_run_YYYYMMDD_HHMMSS.log
```

Bu script gerçek zamanlı olarak şunları gösterir:
- WebSocket bağlantı durumu
- Tespit edilen opportunity'ler
- Profit hesaplamaları
- Hatalar
- Health check sonuçları

## ✅ Kontrol Edilmesi Gerekenler

### 1. WebSocket Bağlantısı

Log'larda şunları görmelisiniz:

```
✅ WebSocket connected
✅ Subscribed to program accounts (subscription ID: X)
```

**Eğer görmüyorsanız:**
- `RPC_WS_URL` ayarını kontrol edin
- WebSocket endpoint'inin erişilebilir olduğunu doğrulayın
- RPC polling fallback'e düşmüş olabilir (normal)

### 2. Opportunity Detection

Log'larda şunları görmelisiniz:

```
🔍 Opportunity detected: account=..., health_factor=0.95, profit=$X.XX
```

**Eğer görmüyorsanız:**
- Bu normal olabilir (hiç riskli pozisyon yok)
- `MIN_PROFIT_USD` çok yüksek olabilir
- Sistem hala başlatılıyor olabilir

### 3. Profit Calculation

Log'larda şunları görmelisiniz:

```
💰 Estimated profit: $X.XX
   - Liquidation bonus: $X.XX
   - Transaction fees: $X.XX
   - DEX fees: $X.XX
   - Slippage: $X.XX
```

**Kontrol edin:**
- Profit hesaplamaları mantıklı mı?
- Fee'ler makul aralıkta mı?
- Slippage tahminleri gerçekçi mi?

### 4. Slippage Estimation

**Jupiter API kullanıyorsanız:**
```
📡 Jupiter API slippage: X bps
```

**Manuel estimation kullanıyorsanız:**
```
📊 Estimated slippage: X bps (size: $X.XX, multiplier: X.XX)
```

**Kontrol edin:**
- Slippage tahminleri makul mu? (genellikle 10-100 bps)
- Trade size'a göre değişiyor mu?

### 5. Hata Kontrolü

Log'larda hata olmamalı:

```bash
# Hata sayısını kontrol edin
grep -ci "error" logs/dry_run_*.log

# Hataları görüntüleyin
grep -i "error" logs/dry_run_*.log | tail -20
```

## 📈 Örnek Analiz Çıktısı

```
📊 Analyzing Dry-Run Logs
File: logs/dry_run_20241128_042235.log

=== 1. WebSocket Connection Status ===
✅ WebSocket: Connected
   Connection Time: 2024-11-28T04:22:35
✅ Subscription: Active
   Subscription Time: 2024-11-28T04:22:36

=== 2. Opportunity Detection ===
Total Opportunities: 5

Opportunity Details:
   Time: 2024-11-28T04:23:15
   Account: 8PRPsh5Z...Lac24sAV
   Health Factor: 0.95
   Profit: $12.50

=== 3. Profit Calculations ===
Total Profit Calculations: 5
   Average Profit: $15.30

=== 6. Error Analysis ===
Total Errors: 0
✅ No errors found

=== 10. Summary ===
✅ Overall Status: HEALTHY
```

## ⏱️ Test Süresi

**Minimum:** 1 saat (sistemin stabilize olması için)
**Önerilen:** 24 saat (tüm senaryoları görmek için)

## 🎯 Başarı Kriterleri

Dry-run testi başarılı sayılır eğer:

- ✅ WebSocket bağlantısı kuruldu
- ✅ Subscription aktif
- ✅ Opportunity detection çalışıyor (en az 1 opportunity tespit edildi)
- ✅ Profit calculation'lar mantıklı
- ✅ Hata yok veya minimal hata
- ✅ Health check'ler başarılı

## 🚨 Sorun Giderme

### WebSocket Bağlanamıyor

```bash
# RPC_WS_URL'i kontrol edin
echo $RPC_WS_URL

# WebSocket endpoint'ini test edin
curl -i -N -H "Connection: Upgrade" -H "Upgrade: websocket" $RPC_WS_URL
```

### Opportunity Tespit Edilmiyor

1. `MIN_PROFIT_USD` değerini düşürün (test için):
   ```bash
   MIN_PROFIT_USD=1.0 DRY_RUN=true cargo run --bin liquid-bot
   ```

2. `HF_LIQUIDATION_THRESHOLD` değerini kontrol edin

3. Sistemin başlatılmasını bekleyin (ilk 5-10 dakika)

### Çok Fazla Hata

1. RPC endpoint'inizi kontrol edin
2. Rate limit'e takılmadığınızdan emin olun
3. WebSocket kullanıldığından emin olun

## 📝 Sonraki Adımlar

Dry-run testi başarılı olduktan sonra:

1. ✅ Log'ları analiz edin
2. ✅ Profit calculation'ları doğrulayın
3. ✅ Slippage tahminlerini kontrol edin
4. ✅ Small capital test yapın (opsiyonel)
5. ✅ Production'a geçin

## 🔗 İlgili Dokümanlar

- [Production Checklist](PRODUCTION_CHECKLIST.md)
- [Production Quick Reference](PRODUCTION_QUICK_REFERENCE.md)
- [Slippage Calibration](SLIPPAGE_CALIBRATION.md)

---

**İyi testler! 🚀**

