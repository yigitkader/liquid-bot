# IDL (Interface Definition Language) Nedir?

## 📖 Genel Tanım

**IDL (Interface Definition Language)**, bir blockchain programının (smart contract) **hangi fonksiyonları** (instructions) sunduğunu, **hangi parametreleri** aldığını ve **hangi veri yapılarını** (accounts) kullandığını tanımlayan bir **dokümantasyon formatıdır**.

## 🎯 Ne İşe Yarar?

### 1. **Program Arayüzünü Tanımlar**
IDL, bir Solana programının dış dünyaya nasıl erişilebileceğini tanımlar:
- Hangi instruction'lar (fonksiyonlar) var?
- Her instruction hangi parametreleri alır?
- Hangi account'lar gerekli?
- Account'ların yapısı nasıl?

### 2. **Client-Server İletişimi**
IDL, client uygulamalarının (bot, web app, CLI) blockchain programıyla nasıl iletişim kuracağını bilmesini sağlar.

### 3. **Type Safety**
IDL sayesinde:
- Veri tipleri doğru parse edilir
- Instruction'lar doğru parametrelerle çağrılır
- Account yapıları doğru deserialize edilir

## 🔍 IDL Örneği (Solend)

```json
{
  "version": "0.1.0",
  "name": "solend_program",
  "instructions": [
    {
      "name": "liquidateObligation",
      "accounts": [
        {
          "name": "liquidator",
          "isMut": false,
          "isSigner": true
        },
        {
          "name": "obligation",
          "isMut": true,
          "isSigner": false
        }
      ],
      "args": [
        {
          "name": "liquidityAmount",
          "type": "u64"
        }
      ]
    }
  ],
  "accounts": [
    {
      "name": "Obligation",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "lastUpdateSlot",
            "type": "u64"
          },
          {
            "name": "depositedValue",
            "type": {
              "defined": "Number"
            }
          }
        ]
      }
    }
  ]
}
```

## 🛠️ Pratik Kullanım Senaryoları

### Senaryo 1: Account Parsing
**Sorun:** Blockchain'den bir account'u okudunuz, ama içeriğini nasıl parse edeceksiniz?

**Çözüm:** IDL'deki account tanımını kullanarak:
```rust
// IDL'den: Obligation account'u şu yapıda:
// - lastUpdateSlot: u64
// - depositedValue: Number
// - borrowedValue: Number
// - deposits: Vec<ObligationCollateral>

// IDL'yi kullanarak parse edebilirsiniz:
let obligation = SolendObligation::from_account_data(&account_data)?;
let deposited = obligation.deposited_value.to_f64();
```

### Senaryo 2: Instruction Oluşturma
**Sorun:** Bir liquidation transaction'ı göndermek istiyorsunuz, ama hangi account'ları eklemelisiniz?

**Çözüm:** IDL'deki instruction tanımını kullanarak:
```rust
// IDL'den: liquidateObligation instruction'ı şu account'ları ister:
// 1. liquidator (signer)
// 2. obligation
// 3. reserve
// 4. tokenProgram
// ...

// IDL'yi kullanarak doğru account listesini oluşturabilirsiniz:
let accounts = vec![
    AccountMeta::new(liquidator, true),
    AccountMeta::new(obligation, false),
    // ... IDL'den gelen diğer account'lar
];
```

### Senaryo 3: Type Safety
**Sorun:** Instruction'a yanlış tip parametre gönderirseniz ne olur?

**Çözüm:** IDL sayesinde compile-time'da hata yakalarsınız:
```rust
// IDL'den: liquidityAmount: u64
// Yanlış kullanım:
instruction_data.push(amount as f64); // ❌ Hata!

// Doğru kullanım:
instruction_data.extend_from_slice(&amount.to_le_bytes()); // ✅
```

## 🔐 Anchor Framework'te IDL

### Anchor Nedir?
**Anchor**, Solana program geliştirmeyi kolaylaştıran bir framework'tür. Anchor kullanan programlar otomatik olarak IDL üretir.

### Anchor IDL Özellikleri:
1. **Otomatik IDL Üretimi:** Program yazıldığında IDL otomatik oluşur
2. **Discriminator:** Her instruction ve account için 8-byte discriminator
3. **Type Safety:** Rust type'ları IDL'ye otomatik map edilir

### Discriminator Nedir?
Anchor'da her instruction ve account için **8-byte discriminator** vardır:
```rust
// Instruction discriminator = sha256("global:instructionName")[0..8]
// Account discriminator = sha256("account:AccountName")[0..8]
```

Bu sayede:
- Hangi instruction çağrıldığını anlayabilirsiniz
- Account'un tipini doğrulayabilirsiniz

## 📊 IDL'nin Bot Projesindeki Rolü

### Önceki Durum (Placeholder):
```rust
// ❌ Placeholder - gerçek yapıyı bilmiyoruz
let position = AccountPosition {
    total_collateral_usd: 0.0, // TODO: Gerçek değeri hesapla
    total_debt_usd: 0.0,       // TODO: Gerçek değeri hesapla
    // ...
};
```

### Şimdiki Durum (IDL ile):
```rust
// ✅ IDL'den gerçek yapıyı biliyoruz
let obligation = SolendObligation::from_account_data(&account_data)?;
let position = AccountPosition {
    total_collateral_usd: obligation.total_deposited_value_usd(), // ✅ Gerçek değer
    total_debt_usd: obligation.total_borrowed_value_usd(),        // ✅ Gerçek değer
    // ...
};
```

## 🎓 Özet

| Özellik | Açıklama |
|---------|----------|
| **Ne?** | Program arayüzünü tanımlayan dokümantasyon formatı |
| **Neden?** | Client'ların programla doğru iletişim kurması için |
| **Nasıl?** | JSON formatında instruction, account ve type tanımları |
| **Ne Zaman?** | Program geliştirilirken (Anchor otomatik üretir) |
| **Nerede?** | `idl/` klasöründe veya program repository'sinde |

## 🔗 İlgili Dosyalar

- `idl/solend.json` - Solend IDL tanımı
- `src/protocols/solend.rs` - IDL'yi kullanan Rust implementasyonu
- `src/protocols/solend.rs` (solend_idl modülü) - IDL'den türetilen Rust struct'ları

## 💡 Pratik İpuçları

1. **IDL'yi Nereden Bulurum?**
   - Program repository'sinde (GitHub)
   - Program'ın kendi web sitesinde
   - Anchor programları otomatik üretir

2. **IDL Olmadan Ne Olur?**
   - Account'ları parse edemezsiniz
   - Instruction'ları doğru oluşturamazsınız
   - Type safety olmaz
   - Manuel reverse engineering gerekir (çok zor!)

3. **IDL Güncellenirse?**
   - Program güncellendiğinde IDL de güncellenir
   - Client kodunuzu da güncellemeniz gerekir
   - Eski IDL ile yeni program çalışmaz

## 🚀 Sonuç

IDL, blockchain programlarıyla iletişim kurmanın **standart yolu**dur. Olmadan:
- ❌ Account'ları parse edemezsiniz
- ❌ Instruction'ları doğru oluşturamazsınız
- ❌ Type safety olmaz

IDL ile:
- ✅ Doğru account parsing
- ✅ Doğru instruction building
- ✅ Type safety
- ✅ Kolay entegrasyon

**Bu yüzden Solend IDL'yi projeye ekledik!** 🎯

