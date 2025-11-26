# liquid-bot

Solana üzerinde çalışan otomatik lending likidasyon botu. Bu bot, Solana blockchain üzerindeki lending protokollerindeki riskli pozisyonları tespit ederek, kârlı olduğunda otomatik şekilde likidasyon işlemi gerçekleştirir.

## 🏗 Mimari

Proje, event-driven (olay tabanlı) ve loosely-coupled (gevşek bağlı) bir mimari kullanır. Core bileşenler protokol bağımsızdır; protokole özel mantık ayrı tutulur.

```
Data Source (RPC/WS)
       ↓
 Event Bus  ←→  Logger / Metrics
       ↓
   Analyzer
       ↓
  Strategist
       ↓
   Executor
       ↓
 Solana Client → On-chain Transaction
```

## 📁 Proje Yapısı

```
src/
  main.rs              # Giriş noktası - tüm sistemi birleştirir
  config.rs            # Konfigürasyon yönetimi
  domain.rs            # İş modeli (AccountPosition, LiquidationOpportunity)
  event.rs             # Event enum'ları
  event_bus.rs         # Merkezi event bus (tokio::broadcast)
  data_source.rs       # Data source kontrol katmanı
  ws_listener.rs       # WebSocket listener
  rpc_poller.rs        # RPC polling
  analyzer.rs          # Health Factor analizi
  strategist.rs        # İş kuralları değerlendirmesi
  executor.rs          # Transaction gönderimi
  logger.rs            # Loglama ve metrics
  solana_client.rs     # Solana client wrapper
  math.rs              # Finansal hesaplamalar
```

## 🚀 Kurulum

1. Rust yüklü olduğundan emin olun (Rust 1.70+ önerilir)

2. Bağımlılıkları yükleyin:
```bash
cargo build
```

3. Wallet oluşturun (eğer yoksa):
```bash
mkdir -p solanakey
solana-keygen new -o ./solanakey/bot-wallet.json
```

4. `.env` dosyası oluşturun:
```bash
cp .env.example .env
```

4. `.env` dosyasını düzenleyin ve gerekli değerleri ayarlayın:
   - `RPC_HTTP_URL`: Solana RPC HTTP endpoint (Helius, Triton, QuickNode vb.)
   - `RPC_WS_URL`: Solana RPC WebSocket endpoint (opsiyonel)
   - `WALLET_PATH`: Wallet dosyası yolu (örn: `./wallet.json`)
   - `HF_LIQUIDATION_THRESHOLD`: Health Factor eşiği (varsayılan: 1.0)
   - `MIN_PROFIT_USD`: Minimum kâr eşiği (USD, **production için önerilen: 5.0-10.0**, test için: 1.0)
   - `MAX_SLIPPAGE_BPS`: Maksimum slippage (basis points, önerilen: 50-100)
   - `POLL_INTERVAL_MS`: Polling aralığı (milisaniye, önerilen: 2000-5000)
   - `DRY_RUN`: Test modu (true/false, **ilk kullanımda mutlaka true!**)

   Detaylı açıklamalar için `.env.example` dosyasına bakın.

## 🏃 Çalıştırma

```bash
# Development modunda
cargo run

# Release modunda
cargo run --release
```

## ⚙️ Konfigürasyon

Tüm konfigürasyon değerleri environment variable'lar üzerinden yönetilir. Detaylar için `.env.example` dosyasına bakın.

### Önemli Parametreler

- **HF_LIQUIDATION_THRESHOLD**: Health Factor bu değerin altındaysa pozisyon riskli kabul edilir
- **MIN_PROFIT_USD**: Bu değerin altındaki fırsatlar işleme alınmaz
  - **Production için önerilen: $5-10** (transaction fee + gas maliyetleri için yeterli margin)
  - **Test için: $1** (sadece test amaçlı, production'da kullanmayın!)
- **DRY_RUN**: `true` ise gerçek transaction gönderilmez, sadece simüle edilir

## 🔧 Geliştirme Durumu

Bu proje şu anda **Production-Ready** aşamasındadır.

### ✅ Tamamlanan
- ✅ Temel mimari yapı
- ✅ Event-driven sistem
- ✅ Worker pipeline
- ✅ Konfigürasyon yönetimi ve validation
- ✅ Solana RPC entegrasyonu
- ✅ Protokol implementasyonu (Solend - temel yapı)
- ✅ Transaction gönderimi (dry-run ve real-run)
- ✅ **Production özellikleri:**
  - ✅ Graceful shutdown
  - ✅ Health check sistemi
  - ✅ Performance monitoring (latency tracking)
  - ✅ TX-lock mekanizması (double liquidation önleme)
  - ✅ Retry mekanizması (exponential backoff)
  - ✅ Rate limiting
  - ✅ Sermaye kontrolü
  - ✅ Slippage kontrolü
  - ✅ Error recovery

### 🔄 Devam Eden / İyileştirmeler
- 🔄 Solend account parsing (gerçek IDL entegrasyonu)
- 🔄 Solend liquidation instruction (gerçek implementasyon)
- 🔄 WebSocket gerçek implementasyonu (RPC polling çalışıyor)

### 📋 Gelecek
- 📋 Multi-protocol desteği (altyapı hazır)
- 📋 WebSocket reconnection mantığı
- 📋 Metrics dashboard
- 📋 MEV optimizasyonları

## 📚 Referans Doküman

Detaylı business analiz dokümanı için `src/business_version_1.0.0.md` dosyasına bakın.

## ⚠️ Önemli Uyarılar

### Production Kullanımı
- **İlk kullanımda mutlaka `DRY_RUN=true` ile test edin!**
- Production'a geçmeden önce küçük sermaye ile test yapın
- Wallet dosyanızı asla git'e commit etmeyin
- RPC provider'ınızın rate limit'lerini kontrol edin

### Güvenlik
- `.env` dosyasını asla git'e commit etmeyin
- `wallet.json` dosyasını asla paylaşmayın
- Private key'inizi güvenli saklayın
- Production'da premium RPC provider kullanın

## 📝 Lisans

Bu proje eğitim ve geliştirme amaçlıdır.
