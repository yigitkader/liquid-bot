# 🚀 Solana Liquidation Bot

Minimal, production-grade Solana **Solend liquidation bot** per Structure.md.

## 🎯 Özellikler

- ✅ **Minimal mimari**: Tek async loop, over-engineering yok
- ✅ **Otomatik layout üretimi**: Solend account layout'ları `build.rs` ile otomatik generate edilir
- ✅ **Güvenlik**: Oracle validation, wallet risk limitleri, kârlılık kontrolü, Jito bundle
- ✅ **Tam otomatik**: IDL/layout bilgisi otomatik üretilir, manuel struct yasak

## 📁 Proje Yapısı

Per Structure.md section 2.1:

```
src/
  main.rs          # Giriş, config yükleme, runtime doğrulama, loop başlatma
  pipeline.rs      # Ana liquidation loop (tek async loop)
  solend.rs        # Otomatik üretilen layout + HF helper'ları
  jup.rs           # Jupiter quote entegrasyonu
  utils.rs         # Wallet, Jito, logging, helper'lar

build.rs           # Solend layout codegen (IDL JSON -> Rust struct)
idl/               # TS SDK'den dump edilen layout JSON'ları
secret/             # Wallet keypair (main.json)
tools/
  solend-layout-dump/  # IDL dump script (Structure.md section 11)
```

## 🚀 Kurulum

### 1. Bağımlılıkları Yükleyin

```bash
cargo build
```

### 2. IDL Layout'larını Oluşturun

Per Structure.md section 11:

```bash
cd tools/solend-layout-dump
npm install
npm run dump-layouts
```

Bu komut `idl/` dizininde layout JSON dosyalarını oluşturur:
- `solend_last_update_layout.json`
- `solend_lending_market_layout.json`
- `solend_reserve_layout.json`
- `solend_obligation_layout.json`

**Not**: İlk kurulumda veya Solend SDK güncellemesinden sonra bu adımı tekrarlayın.

### 3. Wallet Oluşturun

```bash
mkdir -p secret
solana-keygen new -o secret/main.json
```

**ÖNEMLİ**: `secret/` dizini `.gitignore` içinde olmalı. Wallet dosyasını asla commit etmeyin!

### 4. Environment Variables Ayarlayın

`.env` dosyası oluşturun:

```bash
# RPC Configuration
RPC_URL=https://api.mainnet-beta.solana.com
# Premium RPC önerilir: Helius, Triton, QuickNode

# Jito Configuration
JITO_URL=https://mainnet.block-engine.jito.wtf

# Jupiter Configuration
JUPITER_URL=https://quote-api.jup.ag

# Bot Configuration
DRY_RUN=true                    # İlk kullanımda mutlaka true!
MIN_PROFIT_USDC=5.0             # Minimum kâr (USDC)
MAX_POSITION_PCT=0.05           # Max risk (%5 = 0.05)
```

## 🏃 Çalıştırma

```bash
# Development
cargo run

# Release (production)
cargo run --release
```

## ⚙️ Konfigürasyon

Tüm konfigürasyon environment variables üzerinden yönetilir:

| Variable | Açıklama | Default | Önerilen |
|----------|----------|---------|----------|
| `RPC_URL` | Solana RPC endpoint | `https://api.mainnet-beta.solana.com` | Premium RPC (Helius, Triton) |
| `JITO_URL` | Jito Block Engine endpoint | `https://mainnet.block-engine.jito.wtf` | - |
| `JUPITER_URL` | Jupiter Quote API | `https://quote-api.jup.ag` | - |
| `DRY_RUN` | Test modu (transaction göndermez) | `true` | İlk kullanımda `true` |
| `MIN_PROFIT_USDC` | Minimum kâr eşiği (USDC) | `5.0` | Production: `5.0-10.0` |
| `MAX_POSITION_PCT` | Max risk (cüzdanın %'si) | `0.05` | `0.05` (5%) |

## 🔄 Bot Nasıl Çalışır?

Per Structure.md section 9:

1. **Obligation Tarama**: `get_program_accounts(SOLEND_PROGRAM_ID)` ile tüm obligation hesaplarını çeker
2. **Health Factor Kontrolü**: HF < 1.0 olanları bulur
3. **Oracle Validation**: Pyth/Switchboard oracle'ları validate eder
4. **Kârlılık Kontrolü**: Jupiter ile swap kârlılığını hesaplar
5. **Risk Limiti**: Wallet risk limitlerini kontrol eder
6. **Liquidation**: Jito bundle ile güvenli liquidation gönderir

## 🛡️ Güvenlik Özellikleri

Per Structure.md:

- ✅ **Layout Validation**: Runtime'da account size'ları validate edilir
- ✅ **Oracle Guard**: Pyth confidence, stale check, Switchboard deviation kontrolü
- ✅ **Wallet Risk Limit**: `max_position_pct` ile risk sınırlandırılır
- ✅ **Min Profit Guard**: `min_profit_usdc` altındaki fırsatlar işlenmez
- ✅ **Jito Bundle**: MEV koruması için tüm liquidation'lar bundle olarak gönderilir

## 📋 Production Checklist

Production'a geçmeden önce:

1. ✅ **IDL Layout'ları Güncel**: `tools/solend-layout-dump` çalıştırıldı mı?
2. ✅ **Wallet Balance**: SOL (fee) ve USDC (strateji) yeterli mi?
3. ✅ **Dry Run Test**: `DRY_RUN=true` ile 24 saat test edildi mi?
4. ✅ **RPC Provider**: Premium RPC (Helius, Triton) kullanılıyor mu?
5. ✅ **Min Profit**: `MIN_PROFIT_USDC=5.0` veya daha yüksek mi?
6. ✅ **Risk Limit**: `MAX_POSITION_PCT=0.05` (5%) uygun mu?

## 🔧 Geliştirme

### IDL Layout Güncelleme

Solend SDK güncellendiğinde:

```bash
cd tools/solend-layout-dump
npm update @solendprotocol/solend-sdk
npm run dump-layouts
cargo build
```

### Build Process

`build.rs` otomatik olarak:
1. `idl/*.json` dosyalarını okur
2. Rust struct'ları generate eder
3. `OUT_DIR/solend_layout.rs` dosyasını oluşturur
4. `solend.rs` bu dosyayı `include!` ile dahil eder

**Önemli**: `build.rs` internet üzerinden bir şey indirmez. Tüm layout bilgisi önceden üretilmiş JSON'lardan gelir.

## ⚠️ Önemli Uyarılar

### Production Kullanımı

- **İlk kullanımda mutlaka `DRY_RUN=true` ile test edin!**
- Production'a geçmeden önce küçük sermaye ile test yapın
- Wallet dosyanızı asla git'e commit etmeyin
- Premium RPC provider kullanın (ücretsiz RPC rate limit sorunları yaşar)

### Güvenlik

- `.env` dosyasını asla git'e commit etmeyin
- `secret/main.json` dosyasını asla paylaşmayın
- Private key'inizi güvenli saklayın
- Production'da premium RPC provider kullanın

## 📚 Referans

Detaylı tasarım dokümanı için `Structure.md` dosyasına bakın.

## 📝 Lisans

Bu proje eğitim ve geliştirme amaçlıdır.
