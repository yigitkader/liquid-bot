# Wallet Setup Guide

## 📁 Wallet Dosya Yapısı

Projede wallet dosyası şu konumda olmalıdır:

```
liqid-bot/
├── solanakey/
│   └── bot-wallet.json    ← Wallet dosyası burada
├── .env                   ← WALLET_PATH buraya yazılacak
└── ...
```

## 🔑 Wallet Oluşturma

### 1. Solana CLI Kurulumu

```bash
# macOS
sh -c "$(curl -sSfL https://release.solana.com/stable/install)"

# Linux
sh -c "$(curl -sSfL https://release.solana.com/stable/install)"

# Windows
# https://docs.solana.com/cli/install-solana-cli-tools#windows
```

### 2. Wallet Oluşturma

```bash
# solanakey klasörünü oluştur (eğer yoksa)
mkdir -p solanakey

# Wallet oluştur
solana-keygen new -o ./solanakey/bot-wallet.json
```

Bu komut:
- `bot-wallet.json` dosyasını oluşturur
- Public Key (adres) gösterir
- Private Key'i şifreli olarak dosyaya kaydeder

### 3. Public Key'i Kaydet

Komut çıktısında şöyle bir satır göreceksiniz:

```
pubkey: 7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU
```

Bu public key'i kaydedin - para göndermek için gerekecek.

## 💰 Wallet'a Para Yükleme

### Adımlar:

1. **Borsaya Git** (Binance, Coinbase, Paribu, vb.)
2. **SOL Satın Al**
3. **Withdraw (Çekme)** kısmına gel
4. **Ağ Seçimi:** Solana (SOL)
5. **Adres:** Public key'inizi yapıştırın
6. **Miktar:** Test için 1-2 SOL yeterli
7. **Gönder**

### Kontrol:

```bash
# Wallet bakiyesini kontrol et
solana balance -k ./solanakey/bot-wallet.json
```

## ⚙️ .env Dosyası Yapılandırması

`.env` dosyanızda wallet path'i şöyle olmalı:

```env
WALLET_PATH=./solanakey/bot-wallet.json
```

## 🔒 Güvenlik

### ✅ Yapılması Gerekenler:

- ✅ `solanakey/` klasörü `.gitignore`'da
- ✅ Wallet dosyası asla git'e commit edilmemeli
- ✅ Wallet dosyasını yedekleyin (güvenli bir yerde)
- ✅ Public key'i paylaşabilirsiniz (güvenli)
- ✅ Private key'i ASLA paylaşmayın

### ❌ Yapılmaması Gerekenler:

- ❌ Wallet dosyasını GitHub'a yüklemeyin
- ❌ Wallet dosyasını email ile göndermeyin
- ❌ Private key'i ekran görüntüsü almayın
- ❌ Wallet dosyasını cloud'a yüklemeyin (şifrelenmemişse)

## 📝 Özet

1. ✅ `solanakey/` klasörü oluşturuldu
2. ✅ `bot-wallet.json` dosyası oluşturuldu
3. ✅ `.env` dosyasında `WALLET_PATH=./solanakey/bot-wallet.json` ayarlanmalı
4. ✅ Wallet'a para yüklenmeli (test için 1-2 SOL)
5. ✅ Public key kaydedilmeli

## 🚀 Sonraki Adımlar

1. `.env` dosyasını oluşturun: `cp .env.example .env`
2. `.env` dosyasında `WALLET_PATH` değerini kontrol edin
3. Wallet'a para yükleyin
4. Bot'u çalıştırın: `cargo run --release`

## 📚 Referanslar

- Solana CLI Docs: https://docs.solana.com/cli
- Wallet Security: https://docs.solana.com/wallet-guide
- `secret/initialize.md` - Detaylı kurulum talimatları

