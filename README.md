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

3. `.env` dosyası oluşturun:
```bash
cp .env.example .env
```

4. `.env` dosyasını düzenleyin ve gerekli değerleri ayarlayın:
- `RPC_HTTP_URL`: Solana RPC HTTP endpoint
- `RPC_WS_URL`: Solana RPC WebSocket endpoint
- `WALLET_PATH`: Wallet dosyası yolu
- `HF_LIQUIDATION_THRESHOLD`: Health Factor eşiği (varsayılan: 1.0)
- `MIN_PROFIT_USD`: Minimum kâr eşiği (USD)
- `DRY_RUN`: Test modu (true/false)

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
- **DRY_RUN**: `true` ise gerçek transaction gönderilmez, sadece simüle edilir

## 🔧 Geliştirme Durumu

Bu proje şu anda **Faz 2 - PoC (Dry-Run, Tek Protokol)** aşamasındadır.

### Tamamlanan
- ✅ Temel mimari yapı
- ✅ Event-driven sistem
- ✅ Worker pipeline
- ✅ Konfigürasyon yönetimi

### Devam Eden
- 🔄 Solana RPC/WebSocket entegrasyonu
- 🔄 Protokol implementasyonu (Solend)
- 🔄 Gerçek transaction gönderimi

### Gelecek
- 📋 Multi-protocol desteği
- 📋 WebSocket reconnection mantığı
- 📋 Metrics dashboard
- 📋 MEV optimizasyonları

## 📚 Referans Doküman

Detaylı business analiz dokümanı için `src/business_version_1.0.0.md` dosyasına bakın.

## ⚠️ Uyarı

Bu bot henüz production-ready değildir. Test amaçlı kullanım için `DRY_RUN=true` ayarını kullanın.

## 📝 Lisans

Bu proje eğitim ve geliştirme amaçlıdır.
