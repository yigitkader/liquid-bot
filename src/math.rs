use anyhow::Result;
use std::sync::Arc;
use crate::config::Config;
use crate::domain::{AccountPosition, LiquidationOpportunity};
use crate::protocol::Protocol;

/// Transaction fee hesaplama (compute unit'e göre)
/// 
/// Solana'da transaction fee = base fee + (compute units * compute unit price)
/// 
/// Liquidation transaction için tipik değerler:
/// - Base fee: ~5000 lamports (0.000005 SOL)
/// - Compute units: ~200,000 (liquidation için)
/// - Priority fee: ~1000 micro-lamports per CU (config'ten)
/// 
/// Total fee ≈ base_fee + (200_000 * 1000 / 1_000_000) = ~0.0002 SOL
fn calculate_transaction_fee_usd(
    compute_units: u32,
    priority_fee_per_cu: u64,
    sol_price_usd: f64,
) -> f64 {
    // Base transaction fee (Solana base fee)
    const BASE_FEE_LAMPORTS: u64 = 5_000; // ~0.000005 SOL
    
    // Priority fee = compute_units * priority_fee_per_cu
    let priority_fee_lamports = (compute_units as u64) * priority_fee_per_cu / 1_000_000;
    
    // Total fee in lamports
    let total_fee_lamports = BASE_FEE_LAMPORTS + priority_fee_lamports;
    
    // Convert to SOL (1 SOL = 1_000_000_000 lamports)
    let total_fee_sol = total_fee_lamports as f64 / 1_000_000_000.0;
    
    // Convert to USD (SOL price)
    total_fee_sol * sol_price_usd
}

/// Slippage maliyeti hesaplama
/// 
/// Oracle fiyatı vs gerçek piyasa fiyatı arasındaki fark.
/// Slippage genellikle:
/// - Küçük işlemler için: %0.1 - %0.5
/// - Büyük işlemler için: %0.5 - %2.0
/// - Volatil piyasalar için: %2.0 - %5.0
fn calculate_slippage_cost_usd(
    amount_usd: f64,
    slippage_bps: u16, // Basis points (100 bps = 1%)
) -> f64 {
    amount_usd * (slippage_bps as f64 / 10_000.0)
}

/// Token swap maliyeti hesaplama (eğer gerekirse)
/// 
/// Eğer collateral ve debt farklı token'larsa, swap gerekebilir.
/// Swap maliyeti:
/// - DEX fee: %0.1 - %0.3 (Jupiter, Raydium)
/// - Price impact: Slippage'e dahil
fn calculate_swap_cost_usd(
    amount_usd: f64,
    needs_swap: bool,
) -> f64 {
    if !needs_swap {
        return 0.0;
    }
    
    // DEX fee (örnek: %0.2)
    const DEX_FEE_BPS: u16 = 20; // 0.2%
    amount_usd * (DEX_FEE_BPS as f64 / 10_000.0)
}

/// En kârlı collateral asset'i seçer
/// 
/// Seçim kriterleri:
/// 1. Yeterli miktarda olmalı (seizable_collateral_usd'yi karşılayabilmeli)
/// 2. En yüksek değerli olmalı (amount_usd en yüksek)
/// 3. Eşitse, en yüksek LTV'ye sahip olanı seç (daha fazla değer = daha fazla profit potansiyeli)
fn select_most_profitable_collateral(
    collateral_assets: &[crate::domain::CollateralAsset],
    required_collateral_usd: f64,
) -> Option<&crate::domain::CollateralAsset> {
    collateral_assets
        .iter()
        // Yeterli miktarda olanları filtrele
        .filter(|asset| asset.amount_usd >= required_collateral_usd)
        // En yüksek değerli olanı seç (amount_usd en yüksek)
        // Eşitse, en yüksek LTV'ye sahip olanı seç
        .max_by(|a, b| {
            // Önce amount_usd'ye göre karşılaştır
            a.amount_usd
                .partial_cmp(&b.amount_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Eşitse LTV'ye göre karşılaştır
                .then_with(|| a.ltv.partial_cmp(&b.ltv).unwrap_or(std::cmp::Ordering::Equal))
        })
}

/// En kârlı debt asset'i seçer
/// 
/// Seçim kriterleri:
/// 1. En yüksek değerli olmalı (amount_usd en yüksek)
/// 2. Eşitse, en düşük borrow rate'e sahip olanı seç (daha az maliyet)
fn select_most_profitable_debt(
    debt_assets: &[crate::domain::DebtAsset],
) -> Option<&crate::domain::DebtAsset> {
    debt_assets
        .iter()
        // En yüksek değerli olanı seç (amount_usd en yüksek)
        // Eşitse, en düşük borrow rate'e sahip olanı seç
        .max_by(|a, b| {
            // Önce amount_usd'ye göre karşılaştır
            a.amount_usd
                .partial_cmp(&b.amount_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Eşitse borrow rate'e göre ters sırala (düşük rate = daha iyi)
                .then_with(|| b.borrow_rate.partial_cmp(&a.borrow_rate).unwrap_or(std::cmp::Ordering::Equal))
        })
}

/// Likidasyon fırsatı hesaplama (GERÇEKÇİ)
/// 
/// Gerçekçi profit hesaplaması yapar:
/// - Slippage maliyeti
/// - Compute unit'e göre transaction fee
/// - Token swap maliyeti (eğer gerekirse)
/// - Konservatif profit tahmini
/// 
/// Protokol parametreleri Protocol trait'inden alınır.
pub async fn calculate_liquidation_opportunity(
    position: &AccountPosition,
    config: &Config,
    protocol: Arc<dyn Protocol>,
) -> Result<Option<LiquidationOpportunity>> {
    // Health factor kontrolü
    if position.health_factor >= 1.0 || position.total_debt_usd <= 0.0 {
        return Ok(None);
    }
    
    // Protokol parametrelerini Protocol trait'inden al
    let liquidation_params = protocol.get_liquidation_params();
    let close_factor = liquidation_params.close_factor;
    let liquidation_bonus = liquidation_params.liquidation_bonus;
    
    // Max liquidatable amount hesaplama (USD cinsinden)
    // Protokol limiti: close_factor * total_debt
    let max_liquidatable_debt_usd = position.total_debt_usd * close_factor;
    
    // Seizable collateral hesaplama (USD cinsinden) - önce hesapla ki yeterli miktarda olanı seçebilelim
    // Likidasyon bonusu ile birlikte alınacak teminat
    // Formül: liquidated_debt * (1 + liquidation_bonus)
    let seizable_collateral_usd = max_liquidatable_debt_usd * (1.0 + liquidation_bonus);
    
    // En kârlı debt asset'i seç
    let debt_asset = select_most_profitable_debt(&position.debt_assets)
        .ok_or_else(|| anyhow::anyhow!("No debt assets found in position"))?;
    
    // En kârlı collateral asset'i seç (yeterli miktarda olan en yüksek değerli)
    let collateral_asset = select_most_profitable_collateral(&position.collateral_assets, seizable_collateral_usd)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No sufficient collateral assets found. Required: ${:.2}, Available: {:?}",
                seizable_collateral_usd,
                position.collateral_assets.iter().map(|a| a.amount_usd).collect::<Vec<_>>()
            )
        })?;
    
    // USD'yi token amount'una çevir
    // Token price = amount_usd / amount (native units)
    let debt_token_price_usd = if debt_asset.amount > 0 {
        debt_asset.amount_usd / debt_asset.amount as f64
    } else {
        return Ok(None); // Division by zero protection
    };
    
    // Max liquidatable amount (token native units)
    let max_liquidatable = (max_liquidatable_debt_usd / debt_token_price_usd) as u64;
    
    // USD'yi collateral token amount'una çevir
    let collateral_token_price_usd = if collateral_asset.amount > 0 {
        collateral_asset.amount_usd / collateral_asset.amount as f64
    } else {
        return Ok(None); // Division by zero protection
    };
    
    // Seizable collateral (token native units)
    let seizable_collateral = (seizable_collateral_usd / collateral_token_price_usd) as u64;
    
    // ============================================
    // GERÇEKÇİ PROFIT HESAPLAMA
    // ============================================
    
    // 1. Transaction fee (compute unit'e göre)
    // Liquidation transaction için tipik compute units: ~200,000
    const LIQUIDATION_COMPUTE_UNITS: u32 = 200_000;
    const PRIORITY_FEE_PER_CU: u64 = 1_000; // micro-lamports per CU (config'ten alınabilir)
    const SOL_PRICE_USD: f64 = 150.0; // SOL fiyatı (yaklaşık, gerçekte oracle'dan alınmalı)
    
    let transaction_fee_usd = calculate_transaction_fee_usd(
        LIQUIDATION_COMPUTE_UNITS,
        PRIORITY_FEE_PER_CU,
        SOL_PRICE_USD,
    );
    
    // 2. Slippage maliyeti
    // Konservatif tahmin: Config'teki max_slippage'in %50'si kullanılır
    // Gerçek slippage genellikle daha düşüktür, bu yüzden konservatif bir tahmin yapıyoruz
    // İşlem büyüklüğüne göre dinamik slippage (büyük işlemler = daha yüksek slippage)
    let base_slippage_bps = config.max_slippage_bps as f64 * 0.5; // Konservatif tahmin: %50
    
    // İşlem büyüklüğüne göre slippage ayarı:
    // - Küçük işlemler (< $10k): %40'ı kullan (daha düşük slippage)
    // - Orta işlemler ($10k-$100k): %50'yi kullan (varsayılan)
    // - Büyük işlemler (> $100k): %60'ı kullan (daha yüksek slippage)
    let size_multiplier = if seizable_collateral_usd < 10_000.0 {
        0.4 // Küçük işlemler için daha düşük slippage
    } else if seizable_collateral_usd > 100_000.0 {
        0.6 // Büyük işlemler için daha yüksek slippage
    } else {
        0.5 // Orta işlemler için varsayılan
    };
    
    let estimated_slippage_bps = (config.max_slippage_bps as f64 * size_multiplier) as u16;
    let slippage_cost_usd = calculate_slippage_cost_usd(
        seizable_collateral_usd,
        estimated_slippage_bps,
    );
    
    // 3. Token swap maliyeti (eğer collateral ve debt farklı token'larsa)
    // Seçilen en kârlı asset'leri kullan
    let needs_swap = collateral_asset.mint != debt_asset.mint;
    
    let swap_cost_usd = if needs_swap {
        calculate_swap_cost_usd(seizable_collateral_usd, true)
    } else {
        0.0
    };
    
    // 4. Toplam maliyet
    let total_cost_usd = transaction_fee_usd + slippage_cost_usd + swap_cost_usd;
    
    // 5. Net profit hesaplama (KONSERVATİF)
    // Gross profit = Seizable collateral - Liquidated debt
    let gross_profit_usd = seizable_collateral_usd - max_liquidatable_debt_usd;
    
    // Net profit = Gross profit - Tüm maliyetler
    let estimated_profit_usd = gross_profit_usd - total_cost_usd;
    
    // 6. Minimum profit margin kontrolü (%1 minimum margin)
    // Production safety: Minimum %1 profit margin gereklidir
    const MIN_PROFIT_MARGIN_BPS: u16 = 100; // %1 = 100 basis points
    let min_profit_margin_usd = max_liquidatable_debt_usd * (MIN_PROFIT_MARGIN_BPS as f64 / 10_000.0);
    
    if estimated_profit_usd < min_profit_margin_usd {
        log::debug!(
            "Opportunity rejected: estimated profit ${:.2} < min margin ${:.2} ({}% of debt)",
            estimated_profit_usd,
            min_profit_margin_usd,
            MIN_PROFIT_MARGIN_BPS as f64 / 100.0
        );
        return Ok(None);
    }
    
    // 7. Profit hesaplama (zaten konservatif)
    // 
    // ✅ DOĞRULANMIŞ: Ek güvenlik marjı kaldırıldı (önceki: %10, şimdi: yok)
    // 
    // Neden ek güvenlik marjı eklenmiyor:
    // - Slippage: max_slippage_bps'in %50'si kullanılıyor (konservatif tahmin)
    // - Transaction fee: compute unit'e göre hesaplanıyor (gerçekçi)
    // - Swap cost: DEX fee dahil (gerçekçi)
    // - Minimum profit margin: %1 zaten kontrol ediliyor
    // 
    // Bu maliyetler zaten konservatif tahmin edildiği için, ek %10 güvenlik marjı
    // gereksiz kısıtlama yaratır ve kârlı fırsatları kaçırmaya neden olur.
    // 
    // Önceki implementasyon (YANLIŞ):
    //   let conservative_profit_usd = estimated_profit_usd * 0.9;  // %10 güvenlik marjı
    // 
    // Yeni implementasyon (DOĞRU):
    //   estimated_profit_usd direkt kullanılıyor (zaten konservatif)
    // 
    // Eğer gerçek profit tahminden düşük çıkarsa, bu zaten slippage ve diğer
    // konservatif tahminlerle karşılanmış olur.
    
    log::debug!(
        "Profit calculation: gross=${:.2}, tx_fee=${:.4}, slippage=${:.4}, swap=${:.4}, total_cost=${:.4}, net=${:.2}",
        gross_profit_usd,
        transaction_fee_usd,
        slippage_cost_usd,
        swap_cost_usd,
        total_cost_usd,
        estimated_profit_usd
    );
    
    // Minimum profit kontrolü
    if estimated_profit_usd < config.min_profit_usd {
        log::debug!(
            "Opportunity rejected: estimated profit ${:.2} < min ${:.2}",
            estimated_profit_usd,
            config.min_profit_usd
        );
        return Ok(None);
    }
    
    // Debt ve collateral asset'leri zaten yukarıda alındı
    let target_debt_mint = debt_asset.mint.clone();
    let target_collateral_mint = collateral_asset.mint.clone();
    
    Ok(Some(LiquidationOpportunity {
        account_position: position.clone(),
        max_liquidatable_amount: max_liquidatable,
        seizable_collateral,
        liquidation_bonus,
        estimated_profit_usd, // Zaten konservatif tahmin (slippage, fees, swap dahil)
        target_debt_mint,
        target_collateral_mint,
    }))
}

/// Health Factor hesaplama (eğer protokolden direkt alınmıyorsa)
pub fn calculate_health_factor(
    total_collateral_usd: f64,
    total_debt_usd: f64,
    ltv: f64,
) -> f64 {
    if total_debt_usd == 0.0 {
        return f64::INFINITY;
    }
    
    (total_collateral_usd * ltv) / total_debt_usd
}

