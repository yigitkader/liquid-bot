# 🏗️ Solana Liquidation Bot - Mimari ve Sistem Dokümantasyonu

## 📋 İçindekiler

1. [Proje Özeti](#proje-özeti)
2. [Hızlı Başlangıç](#hızlı-başlangıç)
3. [Sistem Mimarisi](#sistem-mimarisi)
4. [Konfigürasyon](#konfigürasyon)
5. [Teknoloji Stack](#teknoloji-stack)
6. [Oracle Entegrasyonları](#oracle-entegrasyonları)
7. [Liquidation Algoritması](#liquidation-algoritması)
8. [Solend Account Parsing Sistemi](#solend-account-parsing-sistemi)
9. [Güvenlik Mekanizmaları](#güvenlik-mekanizmaları)
10. [Yapılan İyileştirmeler](#yapılan-iyileştirmeler)
11. [Kritik Kararlar ve Tasarım Seçimleri](#kritik-kararlar-ve-tasarım-seçimleri)

---

## 🎯 Proje Özeti

**Solana Liquidation Bot**, Solend protokolünde sağlık faktörü (Health Factor) 1.0'ın altına düşen pozisyonları otomatik olarak liquidate eden bir DeFi bot'udur. Bot, kârlı liquidation fırsatlarını tespit eder, risk yönetimi yapar ve Jito bundle kullanarak güvenli bir şekilde liquidation işlemlerini gerçekleştirir.

### Temel Özellikler

- ✅ **Otomatik Tespit**: Health Factor < 1.0 olan pozisyonları otomatik bulur
- ✅ **Oracle Doğrulama**: Pyth ve Switchboard oracle'ları ile çift doğrulama
- ✅ **Kârlılık Analizi**: Jupiter DEX ile swap kârlılığını hesaplar
- ✅ **Risk Yönetimi**: Wallet bazlı risk limitleri ve cumulative risk tracking
- ✅ **MEV Koruması**: Jito bundle ile transaction'ları güvenli şekilde gönderir
- ✅ **Dinamik Slippage**: Pozisyon büyüklüğüne göre otomatik slippage ayarlama

---

## 🚀 Hızlı Başlangıç

### Gereksinimler

- Rust 1.70+ (Solana 2.0 uyumlu)
- Solana CLI (wallet yönetimi için)
- Mainnet RPC endpoint (premium RPC önerilir)
- Jito API erişimi

### Kurulum

```bash
# Projeyi klonlayın
git clone <repo-url>
cd liqid-bot

# Bağımlılıkları yükleyin
cargo build --release

# .env dosyasını oluşturun (aşağıdaki şablonu kullanın)
cp .env.example .env
# .env dosyasını düzenleyin

# Wallet keypair'ı hazırlayın
# secret/main.json dosyasına wallet keypair'ınızı kaydedin
```

### .env Dosyası Konfigürasyonu

**ZORUNLU** environment variable'lar:

```bash
# RPC ve Jito
RPC_URL=https://api.mainnet-beta.solana.com  # Premium RPC önerilir
JITO_URL=https://mainnet.block-engine.jito.wtf
JITO_TIP_ACCOUNT=96gYZGLnJYVFmbjzopPSU6QiEV5fGqZ6N6VBY6FuDgU3
JITO_TIP_AMOUNT_LAMPORTS=10000000  # 0.01 SOL (opsiyonel, default: 10000000)

# Solend Program
SOLEND_PROGRAM_ID=So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo  # Legacy Solend (USDC destekli)

# Token Mints
USDC_MINT=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v

# Oracle Program IDs
PYTH_PROGRAM_ID=FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH
SWITCHBOARD_PROGRAM_ID=SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f
SWITCHBOARD_PROGRAM_ID_V3=SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f

# Risk Yönetimi
MIN_PROFIT_USDC=5.0  # Minimum kâr (USD)
MAX_POSITION_PCT=0.05  # Wallet'ın %5'i max risk

# Mod
DRY_RUN=true  # true = test modu, false = canlı liquidation
```

**OPSİYONEL** environment variable'lar:

```bash
# Retry ve Timeout Ayarları
MAX_RETRIES=5
INITIAL_RETRY_DELAY_MS=1000
POLL_INTERVAL_MS=200

# Oracle Ayarları
MAX_ORACLE_AGE_SECONDS=60
MAX_ORACLE_DEVIATION_PCT=2.0
HF_LIQUIDATION_THRESHOLD=1.0

# Slippage Ayarları
MIN_PROFIT_MARGIN_BPS=50
DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS=20
MAX_SLIPPAGE_BPS=150

# Transaction Fee Ayarları
BASE_TRANSACTION_FEE_LAMPORTS=5000
LIQUIDATION_COMPUTE_UNITS=200000
DEFAULT_PRIORITY_FEE_PER_CU=1000

# Solend Override'ları (opsiyonel)
LIQUIDATION_BONUS=0.05  # %5 (default: Reserve'den okunur)
CLOSE_FACTOR=0.5  # %50 (default: Reserve'den okunur)

# Save Protocol (opsiyonel, USDC yerine SUSD kullanıyorsa)
SUSD_MINT_CANDIDATES=...  # Comma-separated list
SOLEND_PROGRAM_ID_SAVE=SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh
SOLEND_PROGRAM_ID_LEGACY=So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo
```

### Çalıştırma

```bash
# Test modu (DRY_RUN=true)
cargo run --release

# Canlı mod (DRY_RUN=false) - DİKKAT: Gerçek işlemler yapılır!
DRY_RUN=false cargo run --release
```

### Log Dosyaları

Bot, her çalıştırmada `logs/liquidation_YYYY-MM-DD_HH-MM-SS.log` dosyası oluşturur. Loglar hem dosyaya hem de konsola yazılır.

---

## 🏛️ Sistem Mimarisi

### Proje Yapısı

```
liqid-bot/
├── src/
│   ├── main.rs          # Entry point, config loading, validation
│   ├── pipeline.rs      # Ana liquidation loop ve algoritma
│   ├── solend.rs        # Solend account layout'ları ve helper'lar
│   ├── jup.rs           # Jupiter DEX entegrasyonu
│   └── utils.rs         # Jito client, wallet utilities
├── build.rs             # Solend layout code generation
├── idl/                 # Solend account layout JSON'ları
├── secret/              # Wallet keypair (gitignore)
└── tools/               # Yardımcı scriptler
```

### Modül Sorumlulukları

#### 1. `main.rs` - Entry Point
- Environment variable'ları yükler
- Wallet keypair'ı yükler
- Runtime layout validation yapar
- Wallet balance kontrolü yapar
- Liquidation loop'u başlatır

#### 2. `pipeline.rs` - Ana Algoritma
- Obligation tarama ve filtreleme
- Oracle validation (Pyth + Switchboard)
- Liquidation quote hesaplama
- Risk limit kontrolü
- Transaction building ve gönderim

#### 3. `solend.rs` - Solend Protokol Entegrasyonu
- Account layout parsing (Borsh deserialization)
- Health Factor hesaplama
- Reserve ve Obligation helper'ları
- PDA derivation fonksiyonları

#### 4. `jup.rs` - Jupiter DEX Entegrasyonu
- Quote API entegrasyonu
- Retry mekanizması
- Price impact hesaplama
- Slippage yönetimi

#### 5. `utils.rs` - Yardımcı Fonksiyonlar
- Jito bundle client
- Wallet utilities
- Logging helpers

---

## ⚙️ Konfigürasyon

### Environment Variable Yönetimi

Bot, **tüm konfigürasyonu environment variable'lardan** okur. Hardcoded değer yoktur. Bu yaklaşım:

- ✅ **Güvenlik**: Sensitive bilgiler kodda saklanmaz
- ✅ **Esneklik**: Farklı ortamlar için farklı config'ler
- ✅ **Maintainability**: Kod değişikliği olmadan config güncellemesi

### Konfigürasyon Kategorileri

#### 1. RPC ve Infrastructure
- `RPC_URL`: Solana mainnet RPC endpoint (premium RPC önerilir)
- `JITO_URL`: Jito block engine endpoint
- `JITO_TIP_ACCOUNT`: Jito tip account (MEV koruması için)
- `JITO_TIP_AMOUNT_LAMPORTS`: Jito tip miktarı (default: 0.01 SOL)

#### 2. Solend Protokol
- `SOLEND_PROGRAM_ID`: Solend program ID (Legacy Solend önerilir - USDC destekli)
- `USDC_MINT`: USDC token mint address
- `SUSD_MINT_CANDIDATES`: Save Protocol için SUSD mint'leri (opsiyonel)

#### 3. Oracle Program IDs
- `PYTH_PROGRAM_ID`: Pyth Network program ID
- `SWITCHBOARD_PROGRAM_ID`: Switchboard program ID
- `SWITCHBOARD_PROGRAM_ID_V3`: Switchboard v3 program ID

#### 4. Risk Yönetimi
- `MIN_PROFIT_USDC`: Minimum kâr threshold (USD)
- `MAX_POSITION_PCT`: Wallet'ın maksimum yüzdesi (0.05 = %5)
- `HF_LIQUIDATION_THRESHOLD`: Health Factor threshold (default: 1.0)

#### 5. Oracle Ayarları
- `MAX_ORACLE_AGE_SECONDS`: Oracle'ın maksimum yaşı (saniye)
- `MAX_ORACLE_DEVIATION_PCT`: İki oracle arası maksimum sapma (%)

#### 6. Slippage ve Fee Ayarları
- `MIN_PROFIT_MARGIN_BPS`: Minimum kâr marjı (basis points)
- `DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS`: Oracle confidence için slippage
- `MAX_SLIPPAGE_BPS`: Maksimum slippage tolerance

### Runtime Validation

Bot başlangıçta şu kontrolleri yapar:

1. **Mainnet Connection**: Devnet/testnet URL'leri reddedilir
2. **Solend Layout Validation**: Account layout'ları runtime'da doğrulanır
3. **Wallet Balance**: Minimum SOL balance kontrolü
4. **ATA Existence**: Gerekli Associated Token Account'lar oluşturulur
5. **Program ID Validation**: Solend program ID geçerliliği

---

## 🔧 Teknoloji Stack

### Core Dependencies

| Kütüphane | Versiyon | Kullanım Amacı |
|-----------|----------|----------------|
| `solana-client` | 2.0 | RPC client, Solana 2.0 uyumlu |
| `solana-sdk` | 2.0 | Core Solana SDK |
| `solana-program` | 2.0 | Program ID'leri ve utilities |
| `tokio` | 1.0 | Async runtime |
| `borsh` | 1.0 | Solend account deserialization |
| `anyhow` | 1.0 | Error handling |

### Oracle Dependencies

| Kütüphane | Versiyon | Kullanım Amacı |
|-----------|----------|----------------|
| `switchboard-on-demand` | git/main | Switchboard On-Demand SDK (Solana 2.0) |
| `bytemuck` | 1.24.0 | Pod trait için Switchboard parsing |
| `rust_decimal` | 1.0 | Decimal price handling |

### DEX ve Infrastructure

| Kütüphane | Versiyon | Kullanım Amacı |
|-----------|----------|----------------|
| `reqwest` | 0.11 | HTTP client (Jupiter API) |
| `spl-token` | 6.0 | SPL Token program entegrasyonu |
| `spl-associated-token-account` | 4.0 | ATA derivation |

### Build Dependencies

| Kütüphane | Versiyon | Kullanım Amacı |
|-----------|----------|----------------|
| `serde` | 1.0 | JSON parsing (layout generation) |
| `serde_json` | 1.0 | JSON handling |

---

## 🔮 Oracle Entegrasyonları

### 1. Pyth Network Oracle

Pyth Network, Solana ekosisteminde en yaygın kullanılan oracle protokolüdür. Bot, Pyth v2 price feed'lerini kullanarak token fiyatlarını doğrular.

#### Pyth Entegrasyonu Detayları

**Lokasyon**: `src/pipeline.rs::validate_pyth_oracle()`

**Doğrulama Adımları**:

1. **Program ID Kontrolü**
   ```rust
   const PYTH_PROGRAM_ID: &str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH";
   ```
    - Oracle account'un Pyth program'ına ait olduğunu doğrular

2. **Magic Number ve Version Kontrolü**
    - Pyth v2 magic: `[0xa1, 0xb2, 0xc3, 0xd4]`
    - Version: `2`

3. **Price Status Kontrolü**
    - Sadece `Trading` status (2) kabul edilir
    - `Unknown`, `Halted`, `Auction` status'leri reddedilir

4. **Staleness Kontrolü**
   ```rust
   const MAX_SLOT_DIFFERENCE: u64 = 150; // ~1 dakika
   ```
    - `valid_slot`: Price'ın geçerli olduğu son slot
    - `last_slot`: Price'ın son güncellendiği slot
    - Her iki kontrol de yapılır

5. **Confidence Interval Kontrolü**
   ```rust
   const MAX_CONFIDENCE_PCT: f64 = 5.0; // Switchboard varsa %5
   const MAX_CONFIDENCE_PCT_PYTH_ONLY: f64 = 2.0; // Sadece Pyth varsa %2
   ```
    - Confidence interval, price'ın yüzdesi olarak hesaplanır
    - Switchboard yoksa daha sıkı threshold kullanılır

6. **Price Parsing**
   ```rust
   let price = price_raw as f64 * 10_f64.powi(exponent);
   ```
    - Pyth price'ları `i64` formatında, exponent ile normalize edilir
    - Örnek: `price_raw=150000000, exponent=-8 → 1.5 USD`

#### Pyth Özellikleri

- ✅ **Yüksek Güvenilirlik**: Binance, Coinbase gibi major exchange'lerden veri
- ✅ **Düşük Latency**: ~400ms slot time'da güncellenir
- ✅ **Çoklu Publisher**: Birden fazla data source'dan aggregate edilir
- ⚠️ **Manipülasyon Riski**: Tek oracle source olduğunda risk artar (bu yüzden Switchboard cross-validation kullanılır)

### 2. Switchboard On-Demand Oracle

Switchboard On-Demand, Solana 2.0 ile uyumlu yeni nesil oracle protokolüdür. Bot, Switchboard'ı Pyth ile cross-validation için kullanır.

#### Switchboard Entegrasyonu Detayları

**Lokasyon**: `src/pipeline.rs::validate_switchboard_oracle_if_available()`

**SDK Kullanımı**:
```rust
use switchboard_on_demand::on_demand::accounts::pull_feed::PullFeedAccountData;
use bytemuck::Pod;
```

**Parse Yöntemi**:
```rust
// PullFeedAccountData Pod trait'i implement eder
let feed = bytemuck::try_from_bytes::<PullFeedAccountData>(&oracle_account.data)?;
```

**Neden `bytemuck`?**
- SDK'nın `parse()` metodu `Ref<'_, &mut [u8]>` bekler (Anchor context için)
- Off-chain client'larda bu tip oluşturulamaz
- `Pod` trait ile direkt deserialize edilir

**Price Extraction**:
```rust
let price_decimal = feed.value(current_slot)?; // Decimal döner
let price = price_decimal.to_string().parse::<f64>()?;
```

**Staleness Kontrolü**:
- `feed.value(current_slot)` built-in staleness check yapar
- Slot bazlı validation otomatik yapılır

#### Switchboard Özellikleri

- ✅ **On-Demand Model**: Sadece gerektiğinde update edilir (network congestion azaltır)
- ✅ **Multi-Source Aggregation**: Birden fazla oracle'dan veri toplar
- ✅ **Solana 2.0 Uyumlu**: v0 transaction, LUT desteği
- ✅ **Lower-Bound Median**: Güvenli fiyat hesaplama algoritması

#### Cross-Validation Stratejisi

```rust
const MAX_ORACLE_DEVIATION_PCT: f64 = 2.0; // %2 max sapma
```

1. **Her iki oracle'dan price alınır**
2. **Deviation hesaplanır**: `|pyth_price - switchboard_price| / pyth_price * 100`
3. **Eğer deviation > %2**: Oracle validation başarısız
4. **Eğer Switchboard yoksa**: Pyth-only mode, daha sıkı confidence threshold (%2)

**Neden Önemli?**
- Oracle manipülasyon riskini azaltır
- Çift doğrulama ile güvenilirlik artar
- Tek oracle source'a bağımlılığı azaltır

---

## 📦 Solend Account Parsing Sistemi

### Size-Based Discriminator Detection

**KRİTİK DÜZELTME (2025-12-07)**: Solend Legacy hesapları için discriminator tespiti boyut bazlı yapılır.

#### Problem

Eski sistem, ilk 8 byte'ın sıfır olup olmadığına bakarak discriminator varlığını tespit ediyordu. Ancak Solend Legacy hesapları:
- Tam olarak **1300 byte** boyutundadır
- **Anchor discriminator kullanmaz** (veri doğrudan başlar)
- İlk byte **version byte**'dır (0 veya 1), discriminator değil

Eski kod, version byte'ı 1 olan bir hesabı görünce "discriminator var" sanıp 8 byte atlıyor ve 1292 byte ile 1300 byte'lık yapıyı okumaya çalışıyordu → **Hata!**

#### Çözüm: Size-Based Detection

```rust
const EXPECTED_STRUCT_SIZE: usize = 1300; // Legacy Obligation/Reserve boyutu
const DISCRIMINATOR_SIZE: usize = 8;      // Anchor discriminator boyutu

// Boyut bazlı tespit
let has_discriminator = if data.len() == EXPECTED_STRUCT_SIZE + DISCRIMINATOR_SIZE {
    // 1308 byte = Anchor account (discriminator var)
    true
} else if data.len() == EXPECTED_STRUCT_SIZE {
    // 1300 byte = Legacy account (discriminator yok)
    false
} else {
    // Edge case: Fallback to old logic
    // ...
};
```

#### Solend Account Formatları

**Legacy Format (Native Solend)**:
- Boyut: **1300 byte** (exact)
- Discriminator: **Yok**
- Version byte: İlk byte (0 veya 1)
- Kullanım: Mainnet'te aktif olan format

**Anchor Format (Save Protocol - gelecekte)**:
- Boyut: **1308 byte** (1300 + 8 byte discriminator)
- Discriminator: **Var** (ilk 8 byte)
- Version byte: 9. byte (discriminator'dan sonra)
- Kullanım: Save Protocol (2024 rebrand) için hazırlık

#### Fonksiyonlar

**`identify_solend_account_type()`**:
- Boyut bazlı discriminator tespiti
- Version byte kontrolü
- Account type tahmini (Obligation/Reserve/LendingMarket)

**`Obligation::from_account_data()`**:
- Size-based discriminator detection
- 1300 byte → Legacy format (discriminator yok)
- 1308 byte → Anchor format (discriminator var)
- Borsh deserialization

**`Reserve::from_account_data()`**:
- Aynı size-based detection mantığı
- Version byte validation (Reserve için version = 1 zorunlu)

### Account Layout Validation

Bot, başlangıçta tüm Solend account'larını tarayarak layout doğrulaması yapar:

```rust
validate_solend_layouts(&rpc).await?;
```

Bu validation:
- ✅ Account boyutlarını kontrol eder
- ✅ Version byte'larını doğrular
- ✅ Borsh deserialization test eder
- ✅ Layout değişikliklerini erken tespit eder

Eğer layout uyumsuzluğu tespit edilirse, bot hata verir ve IDL JSON'larının güncellenmesi gerektiğini bildirir.

---

## ⚙️ Liquidation Algoritması

### Ana Algoritma Akışı

```
┌─────────────────────────────────────────────────────────┐
│ 1. Obligation Tarama                                    │
│    - get_program_accounts(SOLEND_PROGRAM_ID)            │
│    - Tüm obligation account'larını çek                  │
└─────────────────┬───────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────┐
│ 2. Health Factor Filtreleme                             │
│    - HF = allowedBorrowValue / borrowedValue            │
│    - HF < 1.0 olanları candidates listesine ekle         │
└─────────────────┬───────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────┐
│ 3. Her Candidate İçin:                                  │
│    ┌──────────────────────────────────────────────┐    │
│    │ 3a. Oracle Validation                        │    │
│    │     - Pyth price + confidence check          │    │
│    │     - Switchboard price (varsa)               │    │
│    │     - Cross-validation (deviation < %2)      │    │
│    └─────────────────┬────────────────────────────┘    │
│                      │                                   │
│                      ▼                                   │
│    ┌──────────────────────────────────────────────┐    │
│    │ 3b. Debt Calculation                         │    │
│    │     - Actual debt = borrowed * cumulative_rate│    │
│    │     - Debt to repay = actual_debt * 0.5      │    │
│    │     - Collateral to seize = debt * (1+bonus)│    │
│    └─────────────────┬────────────────────────────┘    │
│                      │                                   │
│                      ▼                                   │
│    ┌──────────────────────────────────────────────┐    │
│    │ 3c. Jupiter Quote                            │    │
│    │     - Dynamic slippage (position size bazlı)  │    │
│    │     - Retry mechanism (3 deneme)             │    │
│    │     - Price impact hesaplama                 │    │
│    └─────────────────┬────────────────────────────┘    │
│                      │                                   │
│                      ▼                                   │
│    ┌──────────────────────────────────────────────┐    │
│    │ 3d. Profit Calculation                      │    │
│    │     - Profit = collateral_value - debt_value│    │
│    │     - Fees: swap + jito + tx                │    │
│    │     - Min profit check                      │    │
│    └─────────────────┬────────────────────────────┘    │
│                      │                                   │
│                      ▼                                   │
│    ┌──────────────────────────────────────────────┐    │
│    │ 3e. Risk Limit Check                         │    │
│    │     - Wallet balance refresh                 │    │
│    │     - Per-liquidation limit                  │    │
│    │     - Cumulative risk tracking              │    │
│    └─────────────────┬────────────────────────────┘    │
│                      │                                   │
│                      ▼                                   │
│    ┌──────────────────────────────────────────────┐    │
│    │ 3f. Transaction Building & Sending          │    │
│    │     - Fresh blockhash (her liquidation için) │    │
│    │     - Solend liquidation instruction        │    │
│    │     - Jito bundle gönderimi                  │    │
│    └──────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────┐
│ 4. Cycle Sonu                                            │
│    - Metrics logging                                     │
│    - 500ms sleep                                        │
│    - Loop devam eder                                    │
└─────────────────────────────────────────────────────────┘
```

### Detaylı Algoritma Adımları

#### Adım 1: Obligation Tarama

```rust
let accounts = rpc.get_program_accounts(&SOLEND_PROGRAM_ID)?;
```

- Solend program'ına ait tüm account'ları çeker
- Her account'u `Obligation::from_account_data()` ile parse eder
- Borsh deserialization kullanılır

#### Adım 2: Health Factor Hesaplama

```rust
pub fn health_factor(&self) -> f64 {
    let borrowed = self.borrowedValue.to_f64();
    if borrowed == 0.0 {
        return f64::INFINITY;
    }
    let weighted_collateral = self.allowedBorrowValue.to_f64();
    weighted_collateral / borrowed
}
```

- `HF = allowedBorrowValue / borrowedValue`
- `HF < 1.0` → Liquidation edilebilir
- `HF >= 1.0` → Güvenli pozisyon

#### Adım 3a: Oracle Validation

**Pyth Validation**:
1. Program ID kontrolü
2. Magic number ve version kontrolü
3. Price status kontrolü (sadece Trading)
4. Staleness kontrolü (valid_slot, last_slot)
5. Confidence interval kontrolü
6. Price parsing ve validation

**Switchboard Validation** (varsa):
1. Feed account parsing (bytemuck ile)
2. `feed.value(current_slot)` ile price extraction
3. Staleness check (built-in)
4. Cross-validation (Pyth ile deviation kontrolü)

#### Adım 3b: Debt Calculation

**KRİTİK DÜZELTME**: Accrued interest hesaba katılmalı!

```rust
// YANLIŞ (eski kod):
let debt_to_repay = borrowedAmountWad * CLOSE_FACTOR / WAD;

// DOĞRU (yeni kod):
let actual_debt_wad = (borrowedAmountWad * cumulativeBorrowRateWads) / WAD;
let debt_to_repay_wad = actual_debt_wad * CLOSE_FACTOR;
let debt_to_repay = debt_to_repay_wad / WAD;
```

**Neden Önemli?**
- `borrowedAmountWad`: İlk borçlanma miktarı
- `cumulativeBorrowRateWads`: Accrued interest faktörü
- Actual debt = İlk borç × Interest faktörü
- Close factor = %50 (Solend standard)

**Collateral Calculation**:
```rust
let liquidation_bonus = deposit_reserve.liquidation_bonus(); // %5 = 0.05
let collateral_to_seize_usd = debt_to_repay_usd * (1.0 + liquidation_bonus);
```

#### Adım 3c: Jupiter Quote

**Dynamic Slippage**:
```rust
let slippage_bps = if position_size_usd < 1000.0 {
    30u16  // Küçük: 0.3%
} else if position_size_usd < 10_000.0 {
    50u16  // Orta: 0.5%
} else if position_size_usd < 50_000.0 {
    100u16 // Büyük: 1.0%
} else {
    150u16 // Çok büyük: 1.5%
};
```

**Retry Mechanism**:
```rust
const REQUEST_TIMEOUT_SECS: u64 = 15; // 10 → 15 saniye
pub async fn get_jupiter_quote_with_retry(..., max_retries: u32) -> Result<JupiterQuote> {
    for attempt in 1..=max_retries {
        match get_jupiter_quote(...).await {
            Ok(quote) => return Ok(quote),
            Err(e) => {
                if attempt < max_retries {
                    tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                }
            }
        }
    }
}
```

**Neden Önemli?**
- Jupiter API yoğun zamanlarda 10+ saniye alabilir
- Retry ile fırsat kaçırma riski azalır
- Exponential backoff ile API'ye yük azalır

#### Adım 3d: Profit Calculation

```rust
let profit_usdc = collateral_value_usd 
    - debt_value_usd 
    - swap_fee_usd      // Jupiter price impact
    - jito_fee_usd      // Jito tip (0.01 SOL)
    - tx_fee_usd;       // Base transaction fee
```

**Fee Breakdown**:
- **Swap Fee**: Jupiter price impact'ten hesaplanır
- **Jito Fee**: 0.01 SOL (default, configurable)
- **TX Fee**: ~5000 lamports (base fee)

#### Adım 3e: Risk Limit Check

**Per-Liquidation Limit**:
```rust
let current_wallet_value_usd = get_wallet_value_usd(rpc, &wallet_pubkey).await?;
let current_max_position_usd = current_wallet_value_usd * config.max_position_pct;
if position_size_usd > current_max_position_usd {
    continue; // Skip
}
```

**Cumulative Risk Tracking**:
```rust
let mut cumulative_risk_usd = 0.0;
let mut pending_liquidation_value = 0.0; // Gönderilmiş ama execute olmamış

// Her liquidation öncesi:
let available_liquidity = current_wallet_value_usd - pending_liquidation_value;
let new_cumulative_risk = cumulative_risk_usd + position_size_usd;
if new_cumulative_risk > available_liquidity * config.max_position_pct {
    continue; // Skip
}

// Başarılı gönderim sonrası:
pending_liquidation_value += position_size_usd;
cumulative_risk_usd += position_size_usd;
```

**Neden Önemli?**
- Wallet balance her liquidation öncesi refresh edilir (race condition önleme)
- Pending liquidation'lar takip edilir (henüz execute olmamış)
- Block-wide cumulative risk limiti korunur

#### Adım 3f: Transaction Building & Sending

**Fresh Blockhash**:
```rust
// KRİTİK: Her liquidation için fresh blockhash
let blockhash = rpc.get_latest_blockhash()?;
let tx = build_liquidation_tx(..., blockhash).await?;
send_jito_bundle(tx, jito_client, ..., blockhash).await?;
```

**Neden Önemli?**
- Blockhash ~60 saniye geçerlidir
- Multiple liquidation'larda 2. liquidation'da stale olabilir
- Her liquidation için fresh blockhash alınır

**Jito Bundle**:
- MEV koruması için kullanılır
- Transaction'lar bundle olarak gönderilir
- Tip account ile öncelik verilir

---

## 🛡️ Güvenlik Mekanizmaları

### 1. Oracle Güvenliği

#### Pyth Validation
- ✅ Magic number kontrolü
- ✅ Version kontrolü
- ✅ Price status kontrolü (sadece Trading)
- ✅ Staleness kontrolü (valid_slot, last_slot)
- ✅ Confidence interval kontrolü
- ✅ Minimum price threshold (division by zero önleme)

#### Switchboard Validation
- ✅ Feed account parsing validation
- ✅ Staleness check (built-in)
- ✅ Cross-validation (Pyth ile deviation kontrolü)

#### Cross-Validation
- ✅ İki oracle arası deviation kontrolü (%2 max)
- ✅ Switchboard yoksa daha sıkı Pyth threshold (%2 vs %5)

### 2. Risk Yönetimi

#### Wallet Risk Limits
- ✅ Per-liquidation limit: `max_position_pct` (default %5)
- ✅ Block-wide cumulative limit
- ✅ Pending liquidation tracking
- ✅ Wallet balance refresh (her liquidation öncesi)

#### Profit Guards
- ✅ Minimum profit threshold: `min_profit_usdc` (default $5)
- ✅ Fee calculation (swap + jito + tx)
- ✅ Price impact consideration

### 3. Transaction Güvenliği

#### Blockhash Management
- ✅ Fresh blockhash (her liquidation için)
- ✅ Atomic operation (fetch → build → sign → send)

#### Jito Bundle
- ✅ MEV koruması
- ✅ Transaction ordering garantisi
- ✅ Tip account ile öncelik

### 4. Code Safety

#### Layout Validation
- ✅ Runtime account size validation
- ✅ Borsh deserialization error handling
- ✅ PDA verification (security check)

#### Error Handling
- ✅ Graceful fallback (oracle fail → Pyth-only mode)
- ✅ Retry mechanisms (Jupiter quote)
- ✅ Comprehensive logging

---

## 🚀 Yapılan İyileştirmeler

### 1. Debt Calculation Fix (KRİTİK)

**Problem**: Accrued interest hesaba katılmıyordu
**Çözüm**: `cumulativeBorrowRateWads` ile actual debt hesaplanıyor

```rust
// ÖNCE:
let debt_to_repay = borrowedAmountWad * CLOSE_FACTOR / WAD;

// SONRA:
let actual_debt_wad = (borrowedAmountWad * cumulativeBorrowRateWads) / WAD;
let debt_to_repay_wad = actual_debt_wad * CLOSE_FACTOR;
let debt_to_repay = debt_to_repay_wad / WAD;
```

**Etki**: Yanlış liquidation amount'ları önlendi

### 2. Jupiter Quote Retry Mechanism

**Problem**: Jupiter API timeout'ları fırsat kaçırıyordu
**Çözüm**: Retry mechanism + timeout artırıldı

```rust
const REQUEST_TIMEOUT_SECS: u64 = 15; // 10 → 15
pub async fn get_jupiter_quote_with_retry(..., max_retries: u32) -> Result<JupiterQuote>
```

**Etki**: API yoğunluğunda fırsat kaçırma riski azaldı

### 3. Fresh Blockhash Per Liquidation

**Problem**: Multiple liquidation'larda stale blockhash riski
**Çözüm**: Her liquidation için fresh blockhash

```rust
// ÖNCE: Loop başında bir kez
let blockhash = rpc.get_latest_blockhash()?;

// SONRA: Her liquidation için
for (obl_pubkey, obligation) in candidates {
    let blockhash = rpc.get_latest_blockhash()?; // Fresh!
    // ...
}
```

**Etki**: Stale blockhash transaction failure'ları önlendi

### 4. Dynamic Slippage

**Problem**: Sabit slippage (50 bps) tüm pozisyonlar için uygun değil
**Çözüm**: Position size bazlı dinamik slippage

```rust
let slippage_bps = if position_size_usd < 1000.0 {
    30u16  // Küçük: 0.3%
} else if position_size_usd < 10_000.0 {
    50u16  // Orta: 0.5%
} else if position_size_usd < 50_000.0 {
    100u16 // Büyük: 1.0%
} else {
    150u16 // Çok büyük: 1.5%
};
```

**Etki**: Büyük pozisyonlarda daha yüksek slippage tolerance, küçük pozisyonlarda daha düşük

### 5. Pending Liquidation Tracking

**Problem**: Jito bundle gönderildi ama henüz execute olmadı, risk limiti yanlış hesaplanıyordu
**Çözüm**: Pending liquidation tracking

```rust
let mut pending_liquidation_value = 0.0;
let available_liquidity = current_wallet_value_usd - pending_liquidation_value;
```

**Etki**: Race condition'lar önlendi, risk limiti doğru hesaplanıyor

### 6. Pyth Confidence Check İyileştirmesi

**Problem**: Edge case'lerde division by zero riski
**Çözüm**: Minimum price threshold artırıldı

```rust
const MIN_VALID_PRICE_USD: f64 = 1e-3; // 1e-6 → 1e-3
```

**Etki**: Floating point precision sorunları önlendi

### 7. Switchboard SDK Entegrasyonu

**Problem**: Switchboard parsing devre dışıydı (SDK API sorunu)
**Çözüm**: `bytemuck` ile `Pod` trait kullanarak parse

```rust
use bytemuck::Pod;
let feed = bytemuck::try_from_bytes::<PullFeedAccountData>(&oracle_account.data)?;
```

**Etki**: Switchboard oracle validation aktif, cross-validation çalışıyor

### 8. Size-Based Discriminator Detection (KRİTİK - 2025-12-07)

**Problem**: Solend Legacy hesapları (1300 byte) yanlış parse ediliyordu
- Eski kod: İlk 8 byte'ın sıfır olup olmadığına bakıyordu
- Version byte (1) görünce "discriminator var" sanıp 8 byte atlıyordu
- 1292 byte ile 1300 byte'lık yapıyı okumaya çalışıyordu → **Hata!**

**Çözüm**: Boyut bazlı discriminator tespiti

```rust
// ÖNCE (YANLIŞ):
let has_discriminator = !data[0..8].iter().all(|&b| b == 0);

// SONRA (DOĞRU):
let has_discriminator = if data.len() == 1308 {
    true  // Anchor format (discriminator var)
} else if data.len() == 1300 {
    false // Legacy format (discriminator yok)
} else {
    // Fallback logic
};
```

**Etki**: 
- ✅ Solend Legacy hesapları doğru parse ediliyor
- ✅ 1300 byte hesaplar artık hata vermiyor
- ✅ Hem Legacy hem Anchor format desteği

**Dosyalar**:
- `src/solend.rs::identify_solend_account_type()`
- `src/solend.rs::Obligation::from_account_data()`
- `src/solend.rs::Reserve::from_account_data()`

---

## 🎯 Kritik Kararlar ve Tasarım Seçimleri

### 1. Neden Solana 2.0?

- **v0 Transaction Desteği**: Daha düşük fee, daha iyi performans
- **LUT (Lookup Table) Desteği**: Transaction size limiti artırır
- **Future-Proof**: Solana ekosisteminin geleceği

### 2. Neden Jito Bundle?

- **MEV Koruması**: Transaction'lar bundle olarak gönderilir, front-running önlenir
- **Öncelik**: Tip account ile transaction'lar öncelikli işlenir
- **Atomicity**: Bundle içindeki transaction'lar birlikte execute edilir veya hiçbiri edilmez

### 3. Neden Çift Oracle (Pyth + Switchboard)?

- **Güvenlik**: Tek oracle source manipülasyon riski taşır
- **Cross-Validation**: İki oracle arası deviation kontrolü
- **Graceful Fallback**: Switchboard yoksa Pyth-only mode (daha sıkı threshold)

### 4. Neden Dynamic Slippage?

- **Price Impact**: Büyük pozisyonlarda slippage daha yüksek olur
- **Optimizasyon**: Küçük pozisyonlarda gereksiz yüksek slippage önlenir
- **Kârlılık**: Daha fazla fırsat yakalanır

### 5. Neden Fresh Blockhash Per Liquidation?

- **Staleness Risk**: Blockhash ~60 saniye geçerlidir
- **Multiple Liquidations**: Aynı cycle'da birden fazla liquidation olabilir
- **Reliability**: Transaction failure riski azalır

### 6. Neden Pending Liquidation Tracking?

- **Race Condition**: Jito bundle gönderildi ama henüz execute olmadı
- **Risk Management**: Wallet balance değişmeden önce risk limiti kontrol edilmeli
- **Accuracy**: Daha doğru risk hesaplama

### 7. Neden Borsh Deserialization?

- **Solend Native Program**: Anchor değil, Borsh kullanır
- **Layout Compatibility**: Solend'in account layout'u Borsh formatında
- **Performance**: Borsh, binary format, hızlı parsing

### 8. Neden Size-Based Discriminator Detection?

- **Legacy Format**: Solend Legacy hesapları 1300 byte, discriminator yok
- **Version Byte**: İlk byte version byte (0 veya 1), discriminator değil
- **Anchor Compatibility**: Gelecekte Anchor format (1308 byte) desteği için hazırlık
- **Reliability**: Boyut bazlı tespit daha güvenilir (zero-check yanıltıcı olabilir)

---

## 📊 Performans Metrikleri

### Cycle Metrics

Bot her cycle'da şu metrikleri toplar:

- `total_candidates`: Toplam liquidation adayı
- `skipped_oracle_fail`: Oracle validation başarısız
- `skipped_jupiter_fail`: Jupiter quote başarısız
- `skipped_insufficient_profit`: Kâr yetersiz
- `skipped_risk_limit`: Risk limiti aşıldı
- `failed_build_tx`: Transaction build hatası
- `failed_send_bundle`: Jito bundle gönderme hatası
- `successful`: Başarılı liquidation

### Logging

- **Info Level**: Cycle summary, başarılı liquidation'lar
- **Debug Level**: Detaylı hesaplamalar, oracle validation
- **Warn Level**: Oracle fallback, risk limit aşımı
- **Error Level**: Kritik hatalar, transaction failure'ları

---

## 🔮 Gelecek İyileştirmeler

### Potansiyel Geliştirmeler

1. **Multi-Strategy Support**: Farklı liquidation stratejileri
2. **Portfolio Management**: Multiple wallet yönetimi
3. **Advanced Risk Models**: Daha sofistike risk hesaplama
4. **Performance Optimization**: Parallel processing
5. **Monitoring Dashboard**: Real-time metrics görüntüleme
6. **Alert System**: Kritik durumlar için alert mekanizması

---

## 📚 Referanslar

- **Solend Protocol**: https://solend.fi/
- **Pyth Network**: https://pyth.network/
- **Switchboard**: https://switchboard.xyz/
- **Jupiter DEX**: https://jup.ag/
- **Jito**: https://jito.wtf/
- **Solana Docs**: https://docs.solana.com/

---

## 📝 Notlar

Bu dokümantasyon, projenin teknik mimarisini ve tasarım kararlarını detaylı olarak açıklar. Gelecekte yeni geliştiriciler veya projeye geri dönen ekip üyeleri için referans olarak kullanılabilir.

**Son Güncelleme**: 2025-12-07
**Versiyon**: 1.1.0

### Versiyon Geçmişi

#### v1.1.0 (2025-12-07)
- ✅ Size-based discriminator detection (Solend Legacy account parsing düzeltmesi)
- ✅ 1300 byte account desteği (discriminator olmadan)
- ✅ Runtime layout validation iyileştirmeleri
- ✅ Environment variable dokümantasyonu

#### v1.0.0 (2025-01-XX)
- ✅ İlk stabil sürüm
- ✅ Pyth + Switchboard oracle entegrasyonu
- ✅ Jupiter DEX entegrasyonu
- ✅ Jito bundle desteği
- ✅ Risk yönetimi sistemi

