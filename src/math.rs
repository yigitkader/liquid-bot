use anyhow::Result;
use crate::config::Config;
use crate::domain::{AccountPosition, LiquidationOpportunity};

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

/// Likidasyon fırsatı hesaplama (GERÇEKÇİ)
/// 
/// Gerçekçi profit hesaplaması yapar:
/// - Slippage maliyeti
/// - Compute unit'e göre transaction fee
/// - Token swap maliyeti (eğer gerekirse)
/// - Konservatif profit tahmini
/// 
/// NOT: Protokol parametreleri Protocol trait'inden alınmalı, şu an config'ten alınıyor.
pub async fn calculate_liquidation_opportunity(
    position: &AccountPosition,
    config: &Config,
) -> Result<Option<LiquidationOpportunity>> {
    // Health factor kontrolü
    if position.health_factor >= 1.0 || position.total_debt_usd <= 0.0 {
        return Ok(None);
    }
    
    // Protokol parametreleri (şu an config'ten, gelecekte Protocol trait'inden alınacak)
    // Solend için tipik değerler:
    let close_factor = 0.5; // %50'ye kadar likide edilebilir (protokol limiti)
    let liquidation_bonus = 0.05; // %5 liquidation bonus (protokol parametresi)
    
    // Max liquidatable amount hesaplama
    // Protokol limiti: close_factor * total_debt
    let max_liquidatable_debt_usd = position.total_debt_usd * close_factor;
    let max_liquidatable = (max_liquidatable_debt_usd) as u64;
    
    // Seizable collateral hesaplama
    // Likidasyon bonusu ile birlikte alınacak teminat
    // Formül: liquidated_debt * (1 + liquidation_bonus)
    let seizable_collateral_usd = max_liquidatable_debt_usd * (1.0 + liquidation_bonus);
    let seizable_collateral = seizable_collateral_usd as u64;
    
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
    // Config'ten max_slippage_bps kullan, ancak gerçek slippage genellikle daha düşüktür
    // Konservatif tahmin: max_slippage'in %50'si (gerçek slippage genellikle daha düşük)
    let estimated_slippage_bps = (config.max_slippage_bps as f64 * 0.5) as u16;
    let slippage_cost_usd = calculate_slippage_cost_usd(
        seizable_collateral_usd,
        estimated_slippage_bps,
    );
    
    // 3. Token swap maliyeti (eğer collateral ve debt farklı token'larsa)
    let needs_swap = position.debt_assets.first()
        .and_then(|d| position.collateral_assets.first()
            .map(|c| c.mint != d.mint))
        .unwrap_or(false);
    
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
    
    // 6. Güvenlik marjı (konservatif profit tahmini)
    // Gerçek profit genellikle tahminden düşük olabilir, bu yüzden %10 güvenlik marjı ekle
    let conservative_profit_usd = estimated_profit_usd * 0.9;
    
    log::debug!(
        "Profit calculation: gross=${:.2}, tx_fee=${:.4}, slippage=${:.4}, swap=${:.4}, total_cost=${:.4}, net=${:.2}, conservative=${:.2}",
        gross_profit_usd,
        transaction_fee_usd,
        slippage_cost_usd,
        swap_cost_usd,
        total_cost_usd,
        estimated_profit_usd,
        conservative_profit_usd
    );
    
    // Minimum profit kontrolü (conservative profit ile)
    if conservative_profit_usd < config.min_profit_usd {
        log::debug!(
            "Opportunity rejected: conservative profit ${:.2} < min ${:.2}",
            conservative_profit_usd,
            config.min_profit_usd
        );
        return Ok(None);
    }
    
    // İlk debt ve collateral asset'leri kullan (gerçekte daha karmaşık seçim olacak)
    // Gelecek iyileştirme: En kârlı asset çiftini seçme algoritması
    let target_debt_mint = position.debt_assets.first()
        .map(|d| d.mint.clone())
        .unwrap_or_default();
    let target_collateral_mint = position.collateral_assets.first()
        .map(|c| c.mint.clone())
        .unwrap_or_default();
    
    Ok(Some(LiquidationOpportunity {
        account_position: position.clone(),
        max_liquidatable_amount: max_liquidatable,
        seizable_collateral,
        liquidation_bonus,
        estimated_profit_usd: conservative_profit_usd, // Konservatif profit kullan
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

