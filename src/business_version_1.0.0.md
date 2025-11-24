-- Version 1.0.0 Business Analysis Document (BAD) --

Aşağıdaki doküman, **“Solana Üzeri Lending Likidasyon Botu”** projesi için hazırlanmış,
**kurumsal seviyede**, **kod içermeyen**, tamamen **mimari & business analiz** odaklı,
geleceğe referans olacak **resmi bir Business Analysis Document (BAD)** formatındadır.

---

# 📘 **Business Analysis Document (BAD)**

## Solana Lending Liquidation Bot – Multi-Protocol Ready Design

### *Version 1.0 – Single Protocol First*

---

# 1. **Executive Summary**

Bu doküman, Solana üzerinde çalışan otomatik bir **Lending Likidasyon Botu** projesinin iş gereksinimlerini, mimari yaklaşımını, kapsamını ve büyüme stratejisini tanımlamak amacıyla hazırlanmıştır.

Bot, başlangıçta **tek bir lending protokolünü** (ör. Solend veya MarginFi) destekleyecek; ancak mimari, gelecekte birden fazla lending protokolünü entegre edecek şekilde **genişletilebilir** tasarlanmıştır.

Doküman, yazılım geliştirme ekibine, proje sahiplerine ve gelecekte projeyi genişletecek ekiplere yol gösterici **referans tasarım** sağlar.

---

# 2. **Project Purpose & Scope**

## 2.1 Amaç

Projenin amacı, Solana blockchain üzerinde çalışan lending protokollerindeki riskli pozisyonları tespit ederek, kârlı olduğunda otomatik şekilde **likidasyon işlemi** gerçekleştirmektir.

## 2.2 Kapsam

Bu proje:

### ✔ Başlangıçta:

* **Tek bir lending protokolünü** destekler
  (örnek: Solend → V1 implementasyonu)

### ✔ Ancak mimari:

* Birden fazla protokolün aynı çekirdek yapı üzerinden çalışmasına izin verecek biçimde tasarlanır
  (Protocol Trait / Interface Model)

### ✔ Bot’un temel fonksiyonları:

1. Pozisyon verilerini (Account Position) almak (RPC/WS)
2. Health Factor analizine göre riskli pozisyonları belirlemek
3. Kârlılık hesaplaması yapmak
4. Likidasyon fırsatı tespit etmek
5. Protokolün liquidation instruction’ını çağırmak
6. İşlem sonuçlarını raporlamak/loglamak
7. Dry-run ve real-run modlarını desteklemek

## 2.3 Kapsam Dışı (Bu Versiyonda)

* Çoklu protokol entegrasyonu (yalnızca altyapı hazırlanacak)
* On-chain arbitrage / MEV fonksiyonları
* Web arayüzü veya dashboard

---

# 3. **Business Goals & Success Criteria**

## 3.1 İş Hedefleri

* Riskli lending pozisyonlarını **erken** ve **doğru** tespit etmek
* Minimum insan müdahalesi ile **otomatik** likidasyon yürütmek
* İşlem başına kârlılığı garanti etmek
* Sistem kararlılığını artırmak (reconnect, retry, rate limiting)
* Gelecekte yeni protokollerin kolayca entegre edilebilmesini sağlamak

## 3.2 Başarı Kriterleri

* HF < threshold pozisyonlarının %99’dan fazlasını belirleyebilmek
* Tek protokol ile tamamen çalışan bir bot (V1)
* Fırsat tespitinden TX gönderimine kadar latency < 300ms (hedef)
* Minimum profit threshold’un altındaki işlemlerin **asla** yapılmaması
* Bot’un 7/24 çalışması ve hata durumunda kendi kendini toparlaması
* Yeni protokol eklemek için **maksimum 1 yeni dosya + 1 mapping** gerekliliği

---

# 4. **High-Level System Overview**

Sistem, event-driven (olay tabanlı) ve loosely-coupled (gevşek bağlı) bir mimari kullanır.
Core bileşenler protokol bağımsızdır; protokole özel mantık ayrı tutulur.

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

Her aşama, **tek bir Event Bus** üzerinden haberleşir.

---

# 5. **Functional Requirements**

## FR-1: Pozisyon Verisinin Alınması

* Sistem, hedef lending protokolündeki **obligation/position** account’larını:

    * RPC polling (batch)
    * WebSocket (accountSubscribe)
      yoluyla okuyabilmeli.
* Raw Solana account verisi, protokol implementasyonu tarafından domain modeline dönüştürülür.

## FR-2: Health Factor Analizi

* HF değeri protokol formüllerine göre hesaplanmalı veya doğrulanmalıdır.
* HF < 1 olan pozisyonlar otomatik olarak riskli görülmelidir.

## FR-3: Liquidation Opportunity Üretme

* HF threshold altında ise:

    * Max liquidatable amount
    * Seizable collateral
    * Liquidation bonus
    * Estimated profit
      hesaplanır.
* Opportunity, Event Bus üzerinden publish edilir.

## FR-4: Kârlılık Stratejisi

* Sistem şu kurallara göre likidasyon kararı verir:

    * Profit ≥ min_profit_usd
    * Slippage ≤ max_slippage_bps
    * Likidasyon için gerekli sermaye mevcut
    * İşlem riskleri tolerans dahilinde

## FR-5: Transaction Oluşturma & Gönderme

* Protokol trait’i liquidation instruction’ı oluşturur.
* Executor:

    * Priority fee ekler
    * Compute budget belirler
    * TX imzalar ve gönderir
    * Sonucu Event Bus’a döner

## FR-6: Monitoring & Logging

* Her event loglanır (INFO, WARN, ERROR)
* Metrics:

    * opportunities_found
    * tx_sent
    * tx_success
    * total_profit_usd

## FR-7: Dry-Run Mode

* TX gönderilmez
* Profit hesaplaması ve event akışı aynen işler
* Test amaçlıdır

---

# 6. **Non-Functional Requirements (NFR)**

## NFR-1: Performans

* WS ile event latencies < 100ms hedeflenir
* RPC polling interval: 1000–2000ms

## NFR-2: Güvenilirlik

* WebSocket bağlantı kopmalarına karşı **otomatik reconnect**
* RPC rate limit durumunda **exponential backoff**
* Executor’da güvenli retry mekanizması

## NFR-3: Güvenlik

* Private key dosyası güvenli saklanmalıdır
* Dry-run ve real-run modlarının karışmaması garanti altına alınmalıdır

## NFR-4: Genişletilebilirlik

* Sistem yeni bir protokol eklemek için:

    * Yeni bir struct (`XProtocol`)
    * Protocol trait implementasyonu
      ile genişletilebilmelidir.

## NFR-5: Test Edilebilirlik

* Her worker bağımsız test edilebilmelidir (unit-test friendly)
* Event-driven mimari integration testlerine elverişli olmalıdır

---

# 7. **Architecture & Design**

## 7.1 Protokol Soyutlaması (*Core Expandability Feature*)

Bot başlangıçta yalnızca 1 protokol destekler:
✔ `SolendProtocol`

Ancak `Protocol` trait yapısı sayesinde:

* HF hesaplama
* Account parsing
* Liquidation instruction oluşturma

fonksiyonları protokol bazlı ayrılır.

**Avantajlar:**

* Core logic → tamamen protokol bağımsız
* Yeni protokol eklemek → mevcut sistemi bozmadan ekleme

### Protocol Trait (Üst Düzey Tanım)

```
Protocol:
  id() → protokol adı
  program_id() → Solana program ID
  parse_account_position() → raw account → domain
  calculate_health_factor()
  params() → borrowing params (LTV, bonus, close factor)
  build_liquidation_tx() → liquidation instruction
```

---

## 7.2 Merkezî Event Bus Tasarımı

* `tokio::broadcast` yapısı kullanılır
* Tüm bileşenler yalnızca Event Bus ile konuşur:

    * Data Source → publish AccountUpdated
    * Analyzer → publish PotentiallyLiquidatable
    * Strategist → publish ExecuteLiquidation
    * Executor → publish TxResult
    * Logger → subscribe tüm event’lere

Bu sayede:

* Bileşenler loosely-coupled
* Yeni worker eklemek (ör. “Notifier Worker”) çok kolay
* Test etmek kolay

---

## 7.3 Worker Pipeline İş Akışı

### 1) Data Source

* Ham Solana hesaplarını okur
* `Protocol::parse_account_position()` çağırır
* `AccountUpdated` event’i üretir

### 2) Analyzer

* HF < 1 ise opportunity üretir
* Protokol parametrelerine göre hesaplama

### 3) Strategist

* Profit, slippage, sermaye gibi business kurallarını değerlendirir
* Onaylarsa `ExecuteLiquidation` event’i oluşturur

### 4) Executor

* Protokolün liquidation instruction’ını üretir
* TX oluşturur, priority fee ekler
* TX yayınlar

### 5) Logger & Metrics

* Tüm event’leri kaydeder
* Monitoring sağlar

---

# 8. **Technical Risks & Mitigation**

| Risk                    | Açıklama                         | Çözüm                                   |
| ----------------------- | -------------------------------- | --------------------------------------- |
| RPC/WS limitleri        | Account verisi çok olabilir      | Rate limiting, batch scanning           |
| WS kopması              | Bot durabilir                    | Auto reconnect + backoff                |
| TX yarış (MEV)          | Hızlı gönderme gerek             | Priority fee, compute budget            |
| Protokol değişiklikleri | API değişebilir                  | Trait soyutlaması ile minimum etkilenme |
| Double liquidation      | Aynı pozisyon tekrar işlenebilir | Executor’da TX-lock mekanizması         |
| Hatalı profit modeli    | Kârsız işlem olabilir            | Strict threshold + dry-run testi        |

---

# 9. **Roadmap (Strategy-Level)**

## Faz 1 – *Analysis & Architecture* (Şu An Tamamlanan)

* Business gereksinimleri
* Protocol trait tasarımı
* Event-driven mimari tasarımı
* Worker pipeline iş akışı

## Faz 2 – *PoC (Dry-Run, Tek Protokol)*

* SolendProtocol v1 (parse + HF + params)
* Data Source → Analyzer → Strategist → Logger
* Profit hesaplama simülasyonları

## Faz 3 – *Real Transaction Phase*

* Likidasyon instruction implementasyonu
* Executor + priority fee
* Gerçek mainnet üzerinde düşük sermaye testleri

## Faz 4 – *Stabilization & Optimizations*

* Reconnect + retry + error handling
* Latency optimizasyonu
* Metrics paneli (opsiyonel)

## Faz 5 – *Multi-Protocol Expansion (Future)*

* İkinci protokol ekleme (MarginFi / Port)
* ProtocolRegistry yapısı
* Shared data source ile multi-stream WS

---

# 10. **Conclusion**

Bu doküman, Solana üzerinde çalışan lending likidasyon botu için hazırlanan **temel referans analizidir**.
Sistem, kısa vadede tek protokole odaklanarak hızla MVP üretmeyi; uzun vadede ise **esnek, genişletilebilir ve robust** bir altyapı kurmayı hedeflemektedir.

Bu tasarım sayesinde:

* Akıllı event-driven yapı
* Protokol bağımsız çekirdek
* Trait tabanlı ölçeklenebilirlik
* Gerçek zamanlı risk analizi
* Geleceğe dönük optimizasyon & MEV çalışmaları

gibi güçlü özellikler elde edilecektir.

---

Haklısın, fazla ileri koştuk. Şimdi **ilk versiyondan** devam edelim ve sadece **projenin dosya yapısını** netleştirelim.
Bu; kod yazarken aynen kopyalayıp kullanabileceğin, sade ama temiz bir **project structure** olacak.

---

## 🗂 Proje Kök Dizini

```text
solana-liquidation-bot/
├─ Cargo.toml
├─ .env.example
├─ README.md
└─ src/
```

**Açıklamalar:**

* `Cargo.toml` → crate bağımlılıkları (tokio, solana-client vs. sonra gelir)
* `.env.example` → RPC URL, cüzdan path vb. için örnek env dosyası
* `README.md` → proje açıklaması / çalıştırma notları
* Tüm asıl iş `src/` altında, **alt klasör yok**.

---

## 📁 `src/` Altındaki Dosyalar

Tam liste:

```text
src/
  main.rs

  config.rs
  domain.rs

  event.rs
  event_bus.rs

  data_source.rs
  ws_listener.rs
  rpc_poller.rs

  analyzer.rs
  strategist.rs
  executor.rs
  logger.rs

  solana_client.rs
  math.rs
```

Şimdi tek tek ne işe yaradıklarını yazıyorum:

---

### 1. Giriş / Bootstrap

#### `main.rs`

* Uygulamanın giriş noktası.
* Şunları yapar (ileride):

    * `Config` yükler
    * `EventBus` oluşturur
    * `SolanaClient` oluşturur
    * `Data Source` (WS veya RPC) seçer ve task olarak başlatır
    * `analyzer`, `strategist`, `executor`, `logger` worker’larını `tokio::spawn` ile ayağa kaldırır
* Yani: **tüm sistemi kablolayan yer**.

---

### 2. Konfigürasyon & Domain

#### `config.rs`

* Proje config’leri burada tutulur:

    * `rpc_http_url`
    * `rpc_ws_url`
    * `wallet_path`
    * `hf_liquidation_threshold`
    * `min_profit_usd`
    * `poll_interval_ms`
* `Config::from_env()` gibi bir fonksiyonla `.env` / env var’lardan yüklenir.

#### `domain.rs`

* İş modelin (business objeler) burada:

    * `AccountPosition` (kullanıcının borç/teminat durumu)
    * `LiquidationOpportunity` (likide edilebilir fırsat)
* Bu struct’lar, sistemin içinde dolaşan **ana veri modelleri**.

---

### 3. Event Sistemi (Event-Driven Kalp)

#### `event.rs`

* Tüm sistemin konuştuğu ortak enum burada:

    * `Event::AccountUpdated(AccountPosition)`
    * `Event::PotentiallyLiquidatable(LiquidationOpportunity)`
    * `Event::ExecuteLiquidation(LiquidationOpportunity)`
    * `Event::TxResult { ... }`
* Ayrıca event payload’ları için yardımcı struct’lar da burada olabilir.

#### `event_bus.rs`

* `tokio::sync::broadcast` tabanlı **event bus** burada.
* Sorumluluğu:

    * `EventBus::new(buffer_size)` → sender + receiver oluşturur
    * `EventBus::publish(Event)` → event yayar
    * `EventBus::subscribe()` → her worker kendi receiver’ını alır
* Tüm worker’lar sadece `EventBus` ile konuşur, birbirleriyle direkt konuşmaz.

---

### 4. Veri Kaynağı (RPC / WebSocket)

İlk versiyonda hem RPC hem WebSocket desteği tasarlanmış olacak; hangisini kullanacağın config’ten seçilir.

#### `data_source.rs`

* Ortak arayüz / kontrol katmanı:

    * "WS mi kullanıyoruz, RPC mi?" seçimi burada yapılır.
    * Gerekirse ileride başka kaynaklar da (örneğin cache) buradan yönetilir.
* İçeride:

    * `run_data_source(bus, cfg)` gibi bir fonksiyon olur;

        * config’e göre `ws_listener` veya `rpc_poller` çağrılır.

#### `ws_listener.rs`

* Solana WebSocket (PubSub) üzerinden account değişikliklerini dinleyecek kısım:

    * `accountSubscribe` veya `logsSubscribe`
* Gelen raw account verilerini **şimdilik** `AccountPosition`’a map’leyip:

    * `Event::AccountUpdated` olarak `EventBus`’a gönderir.
* WebSocket reconnection mantığı da ileride burada olacak.

#### `rpc_poller.rs`

* Belirli aralıklarla RPC üzerinden account’ları tarayan kısım:

    * `getProgramAccounts` ile ilgili lending protokol account’larını çeker
* Her poll’da:

    * Güncel `AccountPosition` listesi üretilir
    * Her biri için `Event::AccountUpdated` yayınlanır.
* Poll interval → `Config.poll_interval_ms`.

> **Not:** Başlangıçta istersen sadece **WS** ile ya da sadece **RPC** ile başlarsın; ama yapı her ikisini de taşımaya hazır.

---

### 5. Worker’lar (Business Pipeline)

Bunlar senin **iş akışını** yöneten küçük servisler:

#### `analyzer.rs`

* Input: `Event::AccountUpdated`
* Görev:

    * HF kontrolü
    * HF threshold altındaysa:

        * `math.rs` yardımıyla kârlı bir fırsat (LiquidationOpportunity) hesaplamaya çalışır
        * Kârlıysa: `Event::PotentiallyLiquidatable` yayınlar

#### `strategist.rs`

* Input: `Event::PotentiallyLiquidatable`
* Görev:

    * `Config.min_profit_usd`, ileride belki sermaye vb. kurallara bakar
    * İş fırsatı **iş kurallarına uygunsa**:

        * `Event::ExecuteLiquidation` yayınlar
    * Değilse event’i discarda eder (loglayarak).

#### `executor.rs`

* Input: `Event::ExecuteLiquidation`
* Görev:

    * `solana_client` ile liquidation transaction hazırlatmak ve göndermek
    * Sonucu:

        * `Event::TxResult` olarak event bus’a yayınlamak

#### `logger.rs`

* Input: **tüm event’ler**
* Görev:

    * Sade ama detaylı loglama
    * İleride metrics ile birleştirilebilir
* İlk versiyonda bile en azından:

    * fırsat bulunduğunda
    * tx gönderildiğinde
    * tx başarılı / başarısız olduğunda
      log yazacak.

---

### 6. Altyapı: Solana & Math

#### `solana_client.rs`

* Solana ile konuşmak için tek yer:

    * RPC client
    * WebSocket client (istersen burada da olabilir, istersen `ws_listener` doğrudan kullanır)
    * Transaction oluşturma ve gönderme (ileri versiyon)
* Executor buraya delegasyon yapacak:

    * “Şu opportunity için liquidation tx hazırla + gönder”.

#### `math.rs`

* Finansal ve risk ile ilgili tüm hesaplar:

    * Health Factor (eğer protokolden direkt almıyorsan)
    * Max likidasyon miktarı
    * Liquidation bonus’a göre alınacak teminat
    * Tahmini profit
* Bu dosya, “botun beynindeki matematik” gibi düşünebilirsin.

---

## 🎯 Özet

İlk versiyon için proje yapısı:

* **Flat** (`src/` içinde tek tek dosyalar)
* **Event-driven** (event + event_bus)
* **Kaynak katmanı ayrılmış** (WS vs RPC)
* **Business pipeline net ayrılmış** (analyzer → strategist → executor → logger)
* **Solana & math altyapısı** ayrı dosyalarda

Bu noktada:

* Yapı sadece “hangi dosyada ne var” seviyesinde.
* Henüz implementation detayına girmedik (doğru yaptık).
* Bunu birebir “iskelet” olarak kullanabilirsin.
