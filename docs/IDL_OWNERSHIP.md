# IDL Kimin? Herkes Kullanabilir mi?

## 🎯 Kısa Cevap

**IDL size özel DEĞİLDİR!** IDL, **program'a özeldir** ve **herkes tarafından kullanılabilir**.

## 📊 IDL Sahipliği

### ❌ Size Özel Değil
- IDL'yi siz oluşturmadınız
- IDL'yi siz sahiplenmediniz
- IDL sadece sizin için değil

### ✅ Program'a Özel
- IDL, **Solend programının** arayüzünü tanımlar
- Solend geliştiricileri tarafından oluşturuldu
- Herkes aynı IDL'yi kullanabilir

## 🔍 Kimler Kullanabilir?

### 1. **Program Geliştiricileri** (Solend Team)
- IDL'yi oluşturur
- Program güncellendiğinde IDL'yi günceller
- IDL'yi public repository'de yayınlar

### 2. **Client Geliştiricileri** (Siz, Bot Geliştiricileri)
- IDL'yi indirir
- IDL'yi kullanarak programla iletişim kurar
- Herkes aynı IDL'yi kullanabilir

### 3. **Herkes**
- IDL public'tir
- GitHub'da bulunabilir
- Herkes indirip kullanabilir

## 📁 IDL Nerede Bulunur?

### Solend IDL Örnekleri:

1. **Solend GitHub Repository**
   ```
   https://github.com/solendprotocol/solend-program
   ```

2. **Solend SDK**
   ```
   @solendprotocol/solend-sdk
   ```

3. **Program'dan Otomatik Çıkarılabilir**
   ```bash
   # Anchor programları IDL'yi otomatik üretir
   anchor build
   # idl/ klasöründe IDL dosyası oluşur
   ```

4. **Blockchain'den Okunabilir**
   ```rust
   // Program'ın IDL'si blockchain'de saklanabilir
   let idl_account = program_client.account::<IdlAccount>(&idl_address).await?;
   ```

## 🔄 IDL Paylaşımı

### Senaryo: 100 Bot Geliştiricisi

```
Solend Program (1 adet)
    ↓
    IDL (1 adet, herkes için aynı)
    ↓
    ├─ Bot Geliştiricisi 1 (siz)
    ├─ Bot Geliştiricisi 2
    ├─ Bot Geliştiricisi 3
    ├─ ...
    └─ Bot Geliştiricisi 100
```

**Hepsi aynı IDL'yi kullanır!**

## 💡 Pratik Örnekler

### Örnek 1: Web Sitesi
```javascript
// Solend web sitesi de aynı IDL'yi kullanır
import { SolendMarket } from "@solendprotocol/solend-sdk";
// IDL otomatik olarak SDK içinde gelir
```

### Örnek 2: Başka Bir Bot
```rust
// Başka bir liquidation bot da aynı IDL'yi kullanır
let obligation = SolendObligation::from_account_data(&data)?;
// Aynı struct, aynı parsing mantığı
```

### Örnek 3: Mobile App
```dart
// Solend mobile app de aynı IDL'yi kullanır
// Flutter/Dart'ta IDL'yi parse eder
```

## 🎓 IDL vs Private Key

| Özellik | IDL | Private Key |
|---------|-----|-------------|
| **Sahiplik** | Program'a özel | Size özel |
| **Paylaşılabilir mi?** | ✅ Evet (public) | ❌ Hayır (gizli) |
| **Herkes kullanabilir mi?** | ✅ Evet | ❌ Hayır |
| **GitHub'a yüklenebilir mi?** | ✅ Evet | ❌ ASLA! |

## 🔐 Güvenlik Notları

### ✅ Güvenli (IDL)
- IDL'yi GitHub'a yükleyebilirsiniz
- IDL'yi paylaşabilirsiniz
- IDL public bilgidir

### ❌ Güvensiz (Private Key)
- Private key'i GitHub'a yüklemeyin
- Private key'i paylaşmayın
- Private key gizli bilgidir

## 📝 Projenizdeki Durum

### `idl/solend.json`
```json
{
  "version": "0.1.0",
  "name": "solend_program",
  // ...
}
```

**Bu IDL:**
- ✅ Solend programına özel
- ✅ Herkes tarafından kullanılabilir
- ✅ Public repository'de paylaşılabilir
- ✅ GitHub'a yüklenebilir

### `wallet.json` (Private Key)
```
⚠️ ASLA GitHub'a yüklemeyin!
⚠️ ASLA paylaşmayın!
⚠️ Sadece sizin için!
```

## 🚀 Sonuç

| Soru | Cevap |
|------|-------|
| IDL size özel mi? | ❌ Hayır |
| IDL program'a özel mi? | ✅ Evet |
| Herkes kullanabilir mi? | ✅ Evet |
| GitHub'a yüklenebilir mi? | ✅ Evet |
| Paylaşılabilir mi? | ✅ Evet |

**IDL = Public Dokümantasyon** (herkes kullanabilir)  
**Private Key = Gizli Bilgi** (sadece sizin)

## 💬 Özet

- **IDL size özel DEĞİLDİR**
- **IDL program'a özeldir** (Solend programı)
- **Herkes aynı IDL'yi kullanabilir**
- **IDL public'tir, paylaşılabilir**
- **IDL GitHub'a yüklenebilir** (güvenli)

**IDL = Program'ın Kullanım Kılavuzu** (herkes okuyabilir) 📖

