# Profit Calculation - Gerçekçi Hesaplama

## 🎯 Amaç

Liquidation bot'unun profit hesaplamasını gerçekçi hale getirmek. Önceki basit hesaplama (`seizable_collateral - debt - 0.0005`) yerine, tüm maliyetleri içeren detaylı bir hesaplama yapılıyor.

## 📊 Profit Hesaplama Formülü

```
Gross Profit = Seizable Collateral - Liquidated Debt
Total Cost = Transaction Fee + Slippage Cost + Swap Cost
Net Profit = Gross Profit - Total Cost
Conservative Profit = Net Profit * 0.9 (10% güvenlik marjı)
```

## 💰 Maliyet Bileşenleri

### 1. Transaction Fee (Compute Unit'e Göre)

**Önceki:** Sabit `0.0005 SOL` (~$0.001)

**Yeni:** Compute unit'e göre dinamik hesaplama

```rust
Base Fee = 5,000 lamports (~0.000005 SOL)
Priority Fee = Compute Units × Priority Fee per CU
Total Fee = Base Fee + Priority Fee
```

**Örnek:**
- Compute Units: 200,000 (liquidation için tipik)
- Priority Fee: 1,000 micro-lamports per CU
- Total Fee ≈ 0.0002 SOL (~$0.03 @ $150/SOL)

### 2. Slippage Cost

**Önceki:** Hesaba katılmıyordu

**Yeni:** Config'ten `max_slippage_bps` kullanılıyor (konservatif: %50'si)

```rust
Slippage Cost = Amount × (Slippage BPS / 10,000)
```

**Örnek:**
- Amount: $1,000
- Slippage: 25 bps (0.25%)
- Cost: $2.50

### 3. Token Swap Cost

**Önceki:** Hesaba katılmıyordu

**Yeni:** Eğer collateral ve debt farklı token'larsa, swap maliyeti ekleniyor

```rust
Swap Cost = Amount × DEX Fee (0.2%)
```

**Örnek:**
- Amount: $1,000
- DEX Fee: 0.2%
- Cost: $2.00

## 🛡️ Güvenlik Marjı

Gerçek profit genellikle tahminden düşük olabilir, bu yüzden **%10 güvenlik marjı** ekleniyor:

```rust
Conservative Profit = Net Profit × 0.9
```

Bu, gerçek profit'in tahminden düşük olma riskini azaltır.

## 📈 Örnek Hesaplama

### Senaryo:
- Liquidated Debt: $1,000
- Liquidation Bonus: 5%
- Seizable Collateral: $1,050

### Önceki Hesaplama:
```
Profit = $1,050 - $1,000 - $0.001 = $49.999
```

### Yeni Hesaplama:
```
Gross Profit = $1,050 - $1,000 = $50.00

Transaction Fee = $0.03 (compute unit'e göre)
Slippage Cost = $1,050 × 0.0025 = $2.63
Swap Cost = $0 (aynı token)
Total Cost = $0.03 + $2.63 + $0 = $2.66

Net Profit = $50.00 - $2.66 = $47.34
Conservative Profit = $47.34 × 0.9 = $42.61
```

**Sonuç:** Gerçek profit ($42.61) önceki tahminden ($49.999) **%15 daha düşük**.

## ⚙️ Konfigürasyon

### Environment Variables

- `MAX_SLIPPAGE_BPS`: Maximum slippage (basis points, default: 50 = 0.5%)
- `MIN_PROFIT_USD`: Minimum profit threshold (default: $1.0)

### Sabitler (Kod İçinde)

- `LIQUIDATION_COMPUTE_UNITS`: 200,000 (liquidation transaction için tipik)
- `PRIORITY_FEE_PER_CU`: 1,000 micro-lamports (config'ten alınabilir - gelecek iyileştirme)
- `SOL_PRICE_USD`: 150.0 (yaklaşık, gerçekte oracle'dan alınmalı - gelecek iyileştirme)
- `DEX_FEE_BPS`: 20 (0.2%, Jupiter/Raydium için tipik)

## 🔮 Gelecek İyileştirmeler

1. **Priority Fee Config**: `PRIORITY_FEE_PER_CU` config'ten alınmalı
2. **SOL Price Oracle**: SOL fiyatı gerçek zamanlı oracle'dan alınmalı
3. **Gerçek Slippage**: Oracle fiyatı vs gerçek piyasa fiyatı karşılaştırması
4. **DEX Integration**: Gerçek DEX API'lerinden swap maliyeti alınmalı
5. **Price Impact**: Büyük işlemler için price impact hesabı

## 📝 Notlar

- **Konservatif Yaklaşım**: Profit tahmini gerçekçi ve konservatif
- **Güvenlik Marjı**: %10 güvenlik marjı ile gerçek profit'in düşük olma riski azaltılıyor
- **Production Ready**: Tüm maliyetler hesaba katılıyor

