# 🏗️ Solana Liquidation Bot - Mimari ve Sistem Dokümantasyonu

## 📋 İçindekiler

1. [Proje Özeti](#proje-özeti)
2. [Sistem Mimarisi](#sistem-mimarisi)
3. [Teknoloji Stack](#teknoloji-stack)
4. [Oracle Entegrasyonları](#oracle-entegrasyonları)
5. [Liquidation Algoritması](#liquidation-algoritması)
6. [Güvenlik Mekanizmaları](#güvenlik-mekanizmaları)
7. [Yapılan İyileştirmeler](#yapılan-iyileştirmeler)
8. [Kritik Kararlar ve Tasarım Seçimleri](#kritik-kararlar-ve-tasarım-seçimleri)

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

**Son Güncelleme**: 2025-01-XX
**Versiyon**: 1.0.0

