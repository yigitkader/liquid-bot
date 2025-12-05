# 🚀 Solana Liquidation Bot – MASTER DESIGN DOCUMENT (Final)

Minimal ama eksiksiz, Google/Microsoft seviyesinde, production-grade bir Solana **Solend liquidation botu** tasarımı.

---

## 1. Amaç ve Vizyon

Hedef:

* Mevcut over-engineered botu:

    * **Minimal**
    * **Hızlı**
    * **Doğru**
    * **Güvenilir**
    * **Tam otomatik şema uyumlu**
    * **Data-oriented**
      hale getirmek.
* Bot:

    * Solend **Obligation** hesaplarını tarar.
    * **HF < 1.0** olanları bulur.
    * Jupiter ile kârlı mı check eder.
    * Jito bundle ile güvenli liquidation gönderir.
* Tüm Solend layout’ları **otomatik** üretilir; manuel struct yasak.

---

## 2. Mimarinin Özeti

### 2.1. Minimal Dosya Yapısı

```text
src/
  main.rs          # Giriş, config yükleme, runtime doğrulama, loop başlatma
  pipeline.rs      # Ana liquidation loop (tek async loop)
  solend.rs        # Otomatik üretilen layout + HF helper'ları
  jup.rs           # Jupiter quote entegrasyonu
  utils.rs         # Wallet, Jito, logging, helper'lar

build.rs           # Solend layout codegen (IDL JSON -> Rust struct)
idl/               # TS SDK'den dump edilen layout JSON'ları
secret/            # Wallet keypair (main.json)
Cargo.toml
```

### 2.2. Tasarım Prensipleri

* **Over-engineering yok**:

    * EventBus, Scanner, Analyzer, Executor, custom WS client → **yok**.
* **Tek loop**:

    * `run_liquidation_loop` her şeyi yönetir.
* **Şema otomatik**:

    * Solend account layout’ları `build.rs` ile generate edilir.
* **Manual byte parsing yok**:

    * `data[offset..]` yazmak yasak.
* **Güvenlik gömülü**:

    * Wallet risk limitleri, oracle guard, kârlılık kontrolü, Jito bundle.

---

## 3. Solana ve Solend – Minimal Zorunlu Bilgi

### 3.1. Solana Temelleri

* **Account**: On-chain veri depolayan yapılar.
* **Program Account**: Programın kodu.
* **PDA**: Program tarafından türetilen adresler.
* **SPL Token**: Token transferleri için standart program.
* **RPC**:

    * `getProgramAccounts(program_id)` → programın tüm hesaplarını getirir.

### 3.2. Solend Ana Hesap Türleri

* `LendingMarket`:

    * Global konfig (ör. quote currency).
* `Reserve`:

    * Her token için likidite havuzu + risk parametreleri.
* `Obligation`:

    * Bir kullanıcının tüm deposit/borrow pozisyonları.
* `LastUpdate`:

    * Güncelleme slot/stale bilgisi.

Bunların **binary layout’u** Solend TypeScript SDK’da `*Layout` değişkenleri olarak export edilir (BufferLayout). ([sdk.solend.fi][1])

---

## 4. Obligation ve Health Factor

### 4.1. Obligation Hesabı – Özet Alanlar

(Struct isimleri `build.rs` ile generate edilecek, burada mantığı anlatıyoruz.)

* `version: u8`
* `last_update: LastUpdate`
* `lending_market: Pubkey`
* `owner: Pubkey`
* `deposits: [ObligationCollateral; N]`
* `borrows: [ObligationLiquidity; N]`
* Ek risk/istatistik alanları (Solend layout’a göre).

**ObligationCollateral** (örnek alanlar):

* `deposit_reserve: Pubkey`
* `deposited_amount: u64`
* `market_value: u128`

**ObligationLiquidity** (örnek alanlar):

* `borrow_reserve: Pubkey`
* `borrowed_amount_wads: u128`
* `market_value: u128`
* `cumulative_borrow_rate_wads: u128`

### 4.2. Health Factor (HF) Mantığı

**Temel prensip**:

```text
HF = (Toplam Collateral Değeri * Liquidation Threshold) / Toplam Borrow Değeri
HF < 1.0 → liquidation mümkün
```

Bot:

1. Obligation içinden:

    * Toplam collateral market değerini,
    * Toplam borrow market değerini okur.
2. Reserve.config.liquidation_threshold ile çarpar.
3. HF hesaplar.
4. HF < 1.0 ise → candidate liquidation.

---

## 5. Reserve ve Oracle Yapısı

### 5.1. Reserve

Üst seviye alanlar:

* `liquidity`:

    * `available_amount: u64`
    * `mint_pubkey: Pubkey`
* `collateral`:

    * collateral mint/supply bilgileri.
* `config`:

    * `loan_to_value_ratio`
    * `liquidation_threshold`
    * `liquidation_bonus`
    * `reserve_factor`
    * `pyth_oracle_pubkey`
    * `switchboard_oracle_pubkey`

### 5.2. Oracle Katmanı: Pyth + Switchboard

* Reserve, primary ve backup oracle adreslerini tutar.
* Bot şu kontrolleri yapar:

    * Pyth fiyatı geçerli mi? (confidence, stale, slot farkı)
    * Switchboard varsa, Pyth ile sapma fazla mı?
    * Oracle hesapları expected program id’ye mi ait?

**Oracle guard geçmezse liquidation yapılmaz.**

---

## 6. Wallet ve Güvenlik

### 6.1. Secret Yönetimi

* `secret/main.json`:

    * Standart Solana keypair JSON.
* **Kesin kurallar**:

    * `secret/` **.gitignore** içinde olmalı.
    * Keypair hiçbir zaman repo’da commit edilmez.
    * Prod ortamda environment vault (örn. KMS) kullanılması tercih edilir.

### 6.2. Config Yapısı

```rust
pub struct Config {
    pub rpc_url: String,
    pub jito_url: String,
    pub jupiter_url: String,
    pub keypair_path: std::path::PathBuf, // "secret/main.json"
    pub liquidation_mode: LiquidationMode,
    pub min_profit_usdc: f64,
    pub max_position_pct: f64, // Örn: 0.05 => cüzdanın %5'i max risk
}

pub enum LiquidationMode {
    DryRun,
    Live,
}
```

### 6.3. Startup Safety Checks

Uygulama başlarken:

1. Keypair dosyası okunur.
2. RPC üzerinden:

    * Wallet SOL balance
    * USDC ATA balance
      alınır.
3. Eğer:

    * SOL fee + Jito tip için yetersizse, **panic**:

        * `"Insufficient SOL balance."`
    * USDC strateji için yetersizse, **panic**:

        * `"Insufficient USDC balance."`

### 6.4. Hard Risk Limit

* Her liquidation’da kullanılacak tutar:

    * `max_position_pct * current_wallet_value`’ı aşamaz.
* Tek blok içinde kullanılan toplam risk de aynı limit ile sınırlıdır.

---

## 7. Jupiter – Kârlılık Hesabı

Likidasyon öncesi:

1. Obligation’dan:

    * Hangi token borçlanmış (debt mint),
    * Hangi collateral seize edilecek (collateral mint)
      belirlenir.

2. Bot:

   ```text
   collateral_amount → Jupiter → debt token amount
   ```

3. Jupiter Quote API’den:

    * `out_amount`
    * `route_plan`
    * `slippage_bps`
      vs. alınır.

**Net profit formülü**:

```text
profit = collateral_value_usd
       - debt_repaid_value_usd
       - swap_fee_usd
       - jito_fee_usd
       - tx_fee_usd
```

Koşul:

```text
profit >= min_profit_usdc
```

sağlanmıyorsa liquidation yapılmaz.

---

## 8. Jito – MEV Koruması

* Likidasyon normal `send_transaction` ile gönderilmez.
* Tüm liquidation tx'leri **Jito Block Engine**’e bundle olarak gönderilir.

Bot:

1. Liquidation tx inşa eder.
2. Compute budget instruction ekler.
3. Priority fee / tip belirler.
4. Bir bundle içine tek liquidation ekler.
5. Aynı obligation address, aynı blokta birden fazla kez hedeflenmez.

Bu sayede:

* Front-run
* Back-run
* MEV sızdırma

riskleri minimize edilir.

---

## 9. Ana Pipeline (run_liquidation_loop)

```rust
pub async fn run_liquidation_loop(
    rpc: std::sync::Arc<solana_client::rpc_client::RpcClient>,
    config: Config,
) -> anyhow::Result<()> {
    let keypair = load_keypair(&config.keypair_path)?;
    let wallet = keypair.pubkey();

    loop {
        // 1. Solend obligation account'larını çek
        let accounts = rpc.get_program_accounts(&SOLEND_PROGRAM_ID)?;

        // 2. HF < 1.0 olanları bul
        let mut candidates = Vec::new();
        for (pk, acc) in accounts {
            if let Ok(obligation) = Obligation::try_from_slice(&acc.data) {
                let hf = obligation.health_factor();
                if hf < 1.0 {
                    candidates.push((pk, obligation));
                }
            }
        }

        // 3. Her candidate için liquidation denemesi
        for (obl_pubkey, obligation) in candidates {
            // a) Oracle + reserve load + HF confirm
            let ctx = build_liquidation_context(&rpc, &obligation).await?;
            if !ctx.oracle_ok {
                continue;
            }

            // b) Jupiter'den kârlılık kontrolü
            let quote = get_jupiter_quote(&ctx).await?;
            if quote.profit_usdc < config.min_profit_usdc {
                continue;
            }

            // c) Wallet risk limiti
            if !is_within_risk_limits(&rpc, &wallet, &quote, &config).await? {
                continue;
            }

            // d) Jito bundle ile gönder
            if matches!(config.liquidation_mode, LiquidationMode::Live) {
                let tx = build_liquidation_tx(&keypair, &ctx, &quote)?;
                send_jito_bundle(&tx, &config).await?;
            } else {
                log::info!(
                    "DryRun: would liquidate obligation {} with profit ~{} USDC",
                    obl_pubkey,
                    quote.profit_usdc
                );
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
```

---

## 10. Solend Layout – Strateji

### 10.1. Neden Otomatik Layout?

* Solend lending programı **Anchor değil**, bu yüzden klasik Anchor IDL JSON’u yok.
* Layout’lar Solend TS SDK’da `*Layout` değişkenleriyle tanımlı (BufferLayout). ([sdk.solend.fi][1])
* Manual Rust struct yazmak:

    * Hata riskini artırır,
    * Protokol güncellemelerine karşı kırılgandır.

Bu yüzden:

> **Kaynak gerçeğimiz**: `@solendprotocol/solend-sdk` Layout objeleri
> **Ara format**: `idl/*.json`
> **Son format**: `build.rs` ile generate edilmiş Rust struct’lar

---

## 11. ***IDL / Layout’lar Nasıl İndirilir ve Üretilir?***  🔥

Burası senin özellikle sorduğun kısım:
**Solend IDL/layout bilgisi nasıl elde edilir?**
Cevap: **TS SDK → Node script → JSON → build.rs → Rust**

### 11.1. Adım 0 – Önkoşullar

* Node.js (>= 18)
* Yarn veya npm
* Rust toolchain

### 11.2. Adım 1 – Layout Dump Projesi Oluşturma

Projende örneğin şu yapıyı kullan:

```bash
mkdir -p tools/solend-layout-dump
cd tools/solend-layout-dump
npm init -y
npm install @solendprotocol/solend-sdk
```

İstersen TypeScript ile çalışmak için:

```bash
npm install --save-dev typescript ts-node @types/node
npx tsc --init
```

`package.json` içinde (ESM kullanmak istersen):

```json
{
  "type": "module",
  "scripts": {
    "dump-layouts": "ts-node src/dump-layouts.ts"
  }
}
```

### 11.3. Adım 2 – JSON Şemasını Tanımla

`idl/*.json` dosyalarının **şeması** sabit olsun:

```jsonc
{
  "meta": {
    "sdkVersion": "0.13.16",
    "sdkCommit": "xxxx",      // opsiyonel
    "generatedAt": "2025-01-01T00:00:00Z"
  },
  "types": [
    {
      "name": "LastUpdate",
      "fields": [
        { "kind": "scalar", "name": "slot", "type": "u64" },
        { "kind": "scalar", "name": "stale", "type": "bool" }
      ]
    }
  ],
  "accounts": [
    {
      "name": "Obligation",
      "fields": [
        { "kind": "scalar", "name": "version", "type": "u8" },
        { "kind": "custom", "name": "last_update", "type": "LastUpdate" },
        { "kind": "scalar", "name": "lending_market", "type": "Pubkey" },
        { "kind": "scalar", "name": "owner", "type": "Pubkey" },
        { "kind": "array", "name": "deposits", "elementType": "ObligationCollateral", "len": 10 },
        { "kind": "array", "name": "borrows", "elementType": "ObligationLiquidity", "len": 10 }
      ]
    }
  ]
}
```

Bu şema:

* `types` → Nested struct tanımları
* `accounts` → Asıl account layout’ları
* `kind`:

    * `"scalar"` → primitive (u64, bool, Pubkey vs.)
    * `"array"` → fixed-length array
    * `"custom"` → başka bir struct

### 11.4. Adım 3 – TS SDK’den Layout Objelerini Kullan

Solend SDK, şu değişkenleri export eder (docs’ta listeleniyor): ([sdk.solend.fi][1])

* `LastUpdateLayout`
* `LendingMarketLayout`
* `ReserveLayout`
* `ObligationLayout`
* `ObligationCollateralLayout`
* `ObligationLiquidityLayout`
* `RESERVE_SIZE`
* `OBLIGATION_SIZE`
* `LENDING_MARKET_SIZE`

**dump-layouts.ts iskeleti (konsept):**

```ts
// tools/solend-layout-dump/src/dump-layouts.ts

import {
  LastUpdateLayout,
  LendingMarketLayout,
  ReserveLayout,
  ObligationLayout,
  ObligationCollateralLayout,
  ObligationLiquidityLayout,
  LENDING_MARKET_SIZE,
  RESERVE_SIZE,
  OBLIGATION_SIZE,
} from "@solendprotocol/solend-sdk";
import { writeFileSync, mkdirSync } from "fs";
import { join } from "path";
// import package.json to get sdkVersion if aynı projede istersen

type Field =
  | { kind: "scalar"; name: string; type: string }
  | { kind: "array"; name: string; elementType: string; len: number }
  | { kind: "custom"; name: string; type: string };

interface LayoutFile {
  meta: {
    sdkVersion: string;
    generatedAt: string;
  };
  types: { name: string; fields: Field[] }[];
  accounts: { name: string; fields: Field[] }[];
}

// NOT: Burada BufferLayout iç yapısını solend-sdk source'una göre
// sen dolduracaksın. Ama mantık şu:
//   - layout.fields üzerinden dön
//   - her field için name/type/len çıkar
//   - bizim JSON Field tipine map et

function dumpLayouts() {
  const outDir = join(process.cwd(), "..", "..", "idl");
  mkdirSync(outDir, { recursive: true });

  // Örnek: LastUpdate + LendingMarket
  const lendingMarketFile: LayoutFile = {
    meta: {
      sdkVersion: "0.13.16", // package.json'dan da çekebilirsin
      generatedAt: new Date().toISOString(),
    },
    types: [
      {
        name: "LastUpdate",
        fields: [
          { kind: "scalar", name: "slot", type: "u64" },
          { kind: "scalar", name: "stale", type: "bool" },
        ],
      },
    ],
    accounts: [
      {
        name: "LendingMarket",
        fields: [
          // Burayı LendingMarketLayout.fields'ten derive edeceksin
          // (name, type vs. mapping)
        ],
      },
    ],
  };

  writeFileSync(
    join(outDir, "solend_lending_market_layout.json"),
    JSON.stringify(lendingMarketFile, null, 2),
    "utf-8",
  );

  // Benzer şekilde:
  // - solend_reserve_layout.json
  // - solend_obligation_layout.json
  // - solend_last_update_layout.json
}

dumpLayouts();
```

> Burada gösterilen kod, **tasarım sözleşmesi**.
> Gerçek implementasyonda `*Layout.fields` yapısını inceleyip tam mapping’i uyguluyorsun (Solend SDK source içinde `src/state/*.ts` dosyalarında görülüyor).

**Prensip**:
Bu node script’i CI’de veya manuel çalıştırıyorsun:

```bash
cd tools/solend-layout-dump
npm run dump-layouts
```

Ve sonuçta repo kökünde:

```text
idl/
  solend_last_update_layout.json
  solend_lending_market_layout.json
  solend_reserve_layout.json
  solend_obligation_layout.json
```

dosyaların oluşmuş oluyor.

### 11.5. Adım 4 – build.rs Nasıl Çalışır?

`build.rs`:

* Bu `idl/` JSON’larını okur.
* JSON’daki `types` ve `accounts`’ı Rust struct’lara map eder.
* `OUT_DIR/solend_layout.rs` dosyasını yazar.

Örnek (önceden verdiğimiz iskelet):

```rust
// Kısaltılmış; tam versiyon daha önceki sürümde var.
println!("cargo:rerun-if-changed=idl/solend_obligation_layout.json");
// ...

let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
let dest_path = out_dir.join("solend_layout.rs");
let mut out = File::create(&dest_path)?;

let layout_files = vec![
    "idl/solend_last_update_layout.json",
    "idl/solend_lending_market_layout.json",
    "idl/solend_reserve_layout.json",
    "idl/solend_obligation_layout.json",
];

let mut generated = String::new();
generated.push_str("use borsh::{BorshDeserialize, BorshSerialize};\n");
generated.push_str("use solana_program::pubkey::Pubkey;\n\n");

// JSON -> LayoutFile parse, sonra render_struct(name, fields) ile Rust code yazma
// ... (önceki build.rs iskeletine bire bir uyuyor)

out.write_all(generated.as_bytes())?;
```

**Önemli**:

* `build.rs` **internet üzerinden bir şey indirmez**.
* Tüm IDL/layout bilgisi **önceden üretilmiş idl JSON’larından** gelir.
* Böylece:

    * Build deterministik,
    * CI’de offline çalışabilir,
    * Network hatalarına bağlı olmaz.

### 11.6. Runtime’da Layout Doğrulama

Runtime startup’ta:

1. Solend SDK’daki sabit account size’ları (`RESERVE_SIZE`, `OBLIGATION_SIZE`, `LENDING_MARKET_SIZE`) JSON içindeki `meta` veya ayrı config ile senkron tut.
2. Bot başlarken:

    * `get_program_accounts(SOLEND_PROGRAM_ID)` ile birkaç örnek account çek.
    * `data.len()` ile layout’tan beklenen size’ı karşılaştır.
3. Eşleşmiyorsa:

```text
"Solend account size mismatch. Layout değişmiş olabilir; lütfen idl JSON'larını güncelle ve botu yeniden build et."
```

ve uygulamayı **başlatma**.

---

## 12. solend.rs

```rust
// src/solend.rs
include!(concat!(env!("OUT_DIR"), "/solend_layout.rs"));

impl Obligation {
    pub fn health_factor(&self) -> f64 {
        // JSON/layout'tan gelen alanlara göre HF hesaplama.
        // (collateral value, borrow value, liquidation_threshold vs.)
        // Formül:
        // HF = allowed_borrow_value / borrowed_value
        1.0 // placeholder; gerçek implementasyon projede olacak.
    }
}
```

Bu dosyada sadece:

* Otomatik struct’lar (include!)
* HF helper’ları
* Ufak convenience fonksiyonlar

yer alır.

---

## 13. Hata Yönetimi ve Güvenlik

Bot şu durumlarda **fail-fast** yapar:

* Layout mismatch (account size tutmuyor).
* Oracle stale / confidence çok kötü.
* Wallet bakiyesi yetersiz.
* Jito endpoint unreachable (ve fallback yoksa).
* Jupiter profit < `min_profit_usdc`.

Her hata:

* Açık ve loggable bir mesaj üretir.
* Gerektiğinde süreci durdurur.

---

## 14. AI İçin Final System Prompt (Güncellenmiş)

AI’a verilecek **güncellenmiş system prompt** özetle:

1. Dosya yapısı: `main.rs`, `pipeline.rs`, `solend.rs`, `jup.rs`, `utils.rs`, `build.rs`.
2. `src/bin/`, `core/events`, `custom ws client` vb. her şey silinecek.
3. Solend layout:

    * **Elle struct yazamazsın.**
    * Layout bilgi kaynağın yalnızca `idl/*.json` dosyalarıdır.
    * `build.rs` bu JSON’lardan `OUT_DIR/solend_layout.rs` üretir.
    * `solend.rs` include! ile bunu projeye dahil eder.
4. Liquidation pipeline:

    * Tek async loop.
    * `get_program_accounts` → Obligation parse → HF < 1.0 → Oracle check → Jupiter profit → Wallet risk → Jito bundle.
5. Wallet:

    * `secret/main.json` kullanılır.
    * Risk limiti ve min profit zorunlu.
6. Oracle:

    * Pyth/Switchboard guard zorunlu.
7. Güvenlik:

    * Layout mismatch guard,
    * Account size guard,
    * Oracle deviation guard,
    * Min-profit guard,
    * Max-position-percentage guard.
8. Kod:

    * Minimal,
    * Over-engineering yok,
    * Google/Microsoft temizliği.

---

