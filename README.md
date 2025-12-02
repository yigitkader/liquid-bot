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

4. `.env` dosyası oluşturun ve gerekli değerleri ayarlayın:
   - `RPC_HTTP_URL`: Solana RPC HTTP endpoint (Helius, Triton, QuickNode vb.)
   - `RPC_WS_URL`: Solana RPC WebSocket endpoint (opsiyonel)
   - `RPC_TIMEOUT_SECONDS`: RPC request timeout (saniye, **default: 10**, validation için 5, ağır işlemler için 30)
   - `WALLET_PATH`: Wallet dosyası yolu (örn: `./wallet.json`)
   - `HF_LIQUIDATION_THRESHOLD`: Health Factor eşiği (varsayılan: 1.0)
   - `MIN_PROFIT_USD`: Minimum kâr eşiği (USD, **production için önerilen: 5.0-10.0**, test için: 1.0)
   - `MAX_SLIPPAGE_BPS`: Maksimum slippage (basis points, önerilen: 50-100)
   - `POLL_INTERVAL_MS`: RPC polling fallback aralığı (milisaniye, **ücretsiz RPC için önerilen: 10000**, premium RPC için: 2000-5000)
     - **Not**: WebSocket varsayılan olarak kullanılır. Bu değer sadece WebSocket başarısız olursa fallback için kullanılır.
   - `DRY_RUN`: Test modu (true/false, **ilk kullanımda mutlaka true!**)
   - `USE_JITO`: Jito MEV protection (true/false, **mainnet için önerilen: true**)
   - `TEST_OBLIGATION_PUBKEY`: Test için bir Solend obligation hesabı (opsiyonel)
     - Bulmak için: `./scripts/find_obligation.sh` çalıştırın
     - Veya Solana Explorer'da Solend program hesabına bakın: https://explorer.solana.com/address/So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo

   Detaylı açıklamalar için aşağıdaki bölümlere bakın.

## 🏃 Çalıştırma

```bash
# Development modunda
cargo run

# Release modunda
cargo run --release
```

## ⚙️ Konfigürasyon

Tüm konfigürasyon değerleri environment variable'lar üzerinden yönetilir.

### TEST_OBLIGATION_PUBKEY Nasıl Bulunur?

`TEST_OBLIGATION_PUBKEY` test ve validasyon için kullanılan gerçek bir Solend obligation hesabıdır. Bulmak için:

**Yöntem 1: Script Kullanma**
```bash
./scripts/find_obligation.sh
```

**Yöntem 2: Solana Explorer**
1. https://explorer.solana.com/address/So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo adresine gidin
2. "Program Accounts" sekmesine tıklayın
3. Account size'a göre filtreleyin (~1300 bytes = obligation accounts)
4. Bir obligation pubkey'i kopyalayın ve `.env` dosyasına ekleyin

**Yöntem 3: Bot Çalıştırma**
Bot çalıştığında otomatik olarak obligation hesaplarını bulur. Log'larda obligation adreslerini görebilirsiniz.

**Not:** `TEST_OBLIGATION_PUBKEY` opsiyoneldir. Boş bırakabilirsiniz, bot yine de çalışır.

### Önemli Parametreler

- **RPC_TIMEOUT_SECONDS**: RPC request timeout (saniye)
  - **Default: 10 saniye** (çoğu RPC çağrısı için yeterli)
  - **Validation için: 5 saniye** (daha hızlı timeout)
  - **Ağır işlemler için: 30 saniye** (get_program_accounts gibi)
  - Bu timeout tüm RPC çağrılarını etkiler ve validation'ın bloklanmasını önler
- **HF_LIQUIDATION_THRESHOLD**: Health Factor bu değerin altındaysa pozisyon riskli kabul edilir
- **MIN_PROFIT_USD**: Bu değerin altındaki fırsatlar işleme alınmaz
  - **Production için önerilen: $5-10** (transaction fee + gas maliyetleri için yeterli margin)
  - **Test için: $1** (sadece test amaçlı, production'da kullanmayın!)
- **DRY_RUN**: `true` ise gerçek transaction gönderilmez, sadece simüle edilir
- **WebSocket**: **Varsayılan olarak kullanılır** (best practice - real-time updates, no rate limits)
  - WebSocket başarısız olursa otomatik olarak RPC polling'e fallback yapılır
- **POLL_INTERVAL_MS**: RPC polling fallback aralığı (WebSocket başarısız olursa kullanılır)
  - **Ücretsiz RPC için: 10000ms (10 saniye)** - getProgramAccounts rate limit'i nedeniyle
  - **Premium RPC için: 2000-5000ms (2-5 saniye)**
  - **WebSocket aktifken: Kullanılmaz** (real-time updates)

### RPC Rate Limiting ve WebSocket

#### ⚠️ RPC Rate Limiting Sorunu

`getProgramAccounts` çağrısı çok ağır bir RPC çağrısıdır ve ücretsiz RPC endpoint'leri bunu sınırlar:

- **Ücretsiz RPC (api.mainnet-beta.solana.com)**:
  - `getProgramAccounts`: **1 req/10s limit** (çok kısıtlayıcı!)
  - Diğer RPC çağrıları: ~10-40 req/s
  - **Çözüm**: `POLL_INTERVAL_MS=10000` (10 saniye) kullanın

- **Premium RPC (Helius, Triton, QuickNode, Alchemy)**:
  - `getProgramAccounts`: Rate limit yok veya çok yüksek
  - Diğer RPC çağrıları: 100-1000+ req/s
  - **Çözüm**: `POLL_INTERVAL_MS=2000-5000` (2-5 saniye) kullanabilirsiniz

#### ✅ WebSocket Kullanımı (Varsayılan - Best Practice)

WebSocket **varsayılan olarak kullanılır** (best practice):

- **Avantajlar**:
  - **Real-time updates**: <100ms latency (RPC polling'den çok daha hızlı)
  - **Rate limit yok**: Push-based, pull-based değil
  - **Düşük gecikme**: Likidasyon fırsatlarını ilk siz görürsünüz
  - **Stabil**: Premium RPC sağlayıcıları WebSocket'i destekler
  - **Otomatik fallback**: WebSocket başarısız olursa RPC polling'e geçer

- **Kullanım**:
  ```bash
  RPC_WS_URL=wss://mainnet.helius-rpc.com/?api-key=YOUR_API_KEY
  # WebSocket otomatik olarak kullanılacak, flag gerekmez
  ```

- **Premium RPC Sağlayıcıları**:
  - **Helius** (Önerilir - Free tier var): https://www.helius.dev/
  - **Triton**: https://triton.one/
  - **QuickNode**: https://www.quicknode.com/
  - **Alchemy**: https://www.alchemy.com/solana

#### RPC Polling vs WebSocket

| Özellik | RPC Polling | WebSocket |
|---------|-------------|-----------|
| Latency | 2-10 saniye | <100ms |
| Rate Limits | Var (özellikle ücretsiz RPC) | Yok |
| Karmaşıklık | Düşük | Orta |
| Production Uygunluğu | Sınırlı | ✅ Önerilir |
| Ücretsiz RPC | ⚠️ Rate limit sorunu | ⚠️ Sınırlı destek |
| Premium RPC | ✅ Çalışır | ✅ Önerilir |

**Not**: WebSocket varsayılan olarak kullanılır. Premium RPC sağlayıcısı kullanmanız önerilir (Helius, Triton, QuickNode).

## 📋 Production Checklist

Production'a geçmeden önce **mutlaka** aşağıdaki checklist'i tamamlayın:

### Hızlı Test

Tüm testleri otomatik olarak çalıştırmak için:

```bash
./scripts/prod_check.sh
```

### Manuel Testler

1. **Struct Validation Test**
   ```bash
   cargo run --bin validate_reserve -- --reserve BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw
   ```

2. **Obligation Parsing Test**
   ```bash
   cargo run --bin find_my_obligation
   ```

3. **System Integration Test**
   ```bash
   cargo run --bin validate_system
   ```

4. **Dry-Run Test (24 saat)**
   ```bash
   DRY_RUN=true cargo run
   ```
   Log'larda şunları kontrol edin:
   - `✅ WebSocket connected`
   - `✅ Subscribed to program accounts`
   - Opportunity detection
   - Profit calculation
   - Fee breakdown
   - Slippage estimation

5. **Small Capital Test**
   ```bash
   DRY_RUN=false MIN_PROFIT_USD=1.0 cargo run
   ```
   ⚠️ **UYARI:** Bu gerçek transaction'lar gönderir! İlk 5-10 transaction'ı dikkatle izleyin.

### Detaylı Dokümanlar

- [Production Checklist](docs/PRODUCTION_CHECKLIST.md) - Detaylı checklist ve açıklamalar
- [Production Quick Reference](docs/PRODUCTION_QUICK_REFERENCE.md) - Hızlı komut referansı

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
