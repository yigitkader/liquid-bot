# Multi-Protocol Architecture

## 📋 Mimari Tasarım

Bu proje, **trait tabanlı mimari** kullanarak gelecekte çoklu protokol desteği için hazırlanmıştır.

### Şu Anki Durum: Tek Protokol (Solend)

- ✅ Sadece Solend protokolü kullanılıyor
- ✅ Tüm worker'lar trait üzerinden çalışıyor
- ✅ Protocol trait yapısı hazır
- ✅ ProtocolRegistry yapısı hazır

### Gelecek: Çoklu Protokol Desteği

Mimari, yeni protokol eklemek için hazırdır. Sadece:
1. Yeni protokol struct'ı oluştur
2. Protocol trait'ini implement et
3. Registry'ye ekle

## 🏗️ Mimari Yapı

### Protocol Trait

```rust
pub trait Protocol: Send + Sync {
    fn id(&self) -> &str;
    fn program_id(&self) -> Pubkey;
    async fn parse_account_position(...) -> Result<Option<AccountPosition>>;
    fn calculate_health_factor(...) -> Result<f64>;
    fn get_liquidation_params(&self) -> LiquidationParams;
    async fn build_liquidation_instruction(...) -> Result<Instruction>;
}
```

### ProtocolRegistry

```rust
pub struct ProtocolRegistry {
    protocols: Vec<Box<dyn Protocol>>,
}

impl ProtocolRegistry {
    pub fn register(&mut self, protocol: Box<dyn Protocol>);
    pub fn find(&self, protocol_id: &str) -> Option<&dyn Protocol>;
    pub fn all(&self) -> &[Box<dyn Protocol>];
}
```

## 📝 Yeni Protokol Ekleme Süreci

### Adım 1: Yeni Protokol Struct'ı Oluştur

`src/protocols/marginfi.rs` (örnek):

```rust
use crate::protocol::Protocol;

pub struct MarginFiProtocol {
    program_id: Pubkey,
}

impl MarginFiProtocol {
    pub fn new() -> Result<Self> {
        // MarginFi program ID'si
        Ok(MarginFiProtocol {
            program_id: Pubkey::from_str("...")?,
        })
    }
}

#[async_trait]
impl Protocol for MarginFiProtocol {
    fn id(&self) -> &str {
        "MarginFi"
    }
    
    fn program_id(&self) -> Pubkey {
        self.program_id
    }
    
    // ... diğer trait metodları
}
```

### Adım 2: Main.rs'de Registry'ye Ekle

```rust
// Main.rs'de
mod protocols {
    pub mod solend;
    pub mod marginfi; // Yeni protokol
}

// Protocol registry'ye ekle
let marginfi_protocol = MarginFiProtocol::new()?;
protocol_registry.register(Box::new(marginfi_protocol));
```

### Adım 3: Worker'lar Otomatik Çalışır

Tüm worker'lar trait üzerinden çalıştığı için:
- ✅ Analyzer: Tüm protokolleri destekler
- ✅ Strategist: Tüm protokolleri destekler
- ✅ Executor: Tüm protokolleri destekler
- ✅ Data Source: Tüm protokolleri destekler

**Ek kod değişikliği gerekmez!**

## 🎯 Avantajlar

### 1. Loose Coupling
- Worker'lar protokole bağımlı değil
- Sadece Protocol trait'ini bilirler
- Yeni protokol eklemek mevcut kodu bozmaz

### 2. Extensibility
- Yeni protokol = 1 yeni dosya + 1 register çağrısı
- Mevcut kod değişmez
- Test etmek kolay

### 3. Type Safety
- Trait sayesinde compile-time kontrol
- Runtime hataları azalır
- IDE desteği iyi

## 📊 Mevcut Durum

| Özellik | Durum | Açıklama |
|---------|-------|----------|
| Protocol Trait | ✅ | Hazır |
| ProtocolRegistry | ✅ | Hazır |
| SolendProtocol | ✅ | Implement edildi |
| Worker'lar (trait üzerinden) | ✅ | Hazır |
| Multi-protocol support | ⏳ | Mimari hazır, implementasyon bekliyor |

## 🔮 Gelecek Senaryolar

### Senaryo 1: İkinci Protokol Ekleme (MarginFi)

```rust
// 1. marginfi.rs oluştur
// 2. Protocol trait'ini implement et
// 3. Main.rs'de:
protocol_registry.register(Box::new(MarginFiProtocol::new()?));

// Worker'lar otomatik çalışır!
```

### Senaryo 2: Protokol Seçimi

```rust
// Config'den protokol seçimi
let protocol_id = config.protocol_id; // "Solend" veya "MarginFi"
let protocol = protocol_registry.find(&protocol_id)?;
```

### Senaryo 3: Tüm Protokolleri Tarama

```rust
// Tüm protokolleri tarayarak fırsat bulma
for protocol in protocol_registry.all() {
    let accounts = rpc_client.get_program_accounts(protocol.program_id()).await?;
    // ...
}
```

## ✅ Sonuç

- ✅ Mimari çoklu protokol için hazır
- ✅ Şu an tek protokol (Solend) kullanılıyor
- ✅ Yeni protokol eklemek çok kolay
- ✅ Mevcut kod değişmeden genişletilebilir

**Mimari: Production-ready ve Future-proof!** 🚀

