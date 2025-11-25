use anyhow::Result;
use crate::config::Config;
use crate::domain::{AccountPosition, LiquidationOpportunity};

/// Likidasyon fırsatı hesaplama
/// 
/// Protokol parametrelerini kullanarak gerçekçi hesaplamalar yapar.
/// Not: Protokol parametreleri Protocol trait'inden alınmalı, şu an config'ten alınıyor.
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
    let transaction_fee_estimate = 0.0005; // ~0.0005 SOL transaction fee (yaklaşık $0.001)
    
    // Max liquidatable amount hesaplama
    // Protokol limiti: close_factor * total_debt
    let max_liquidatable_debt_usd = position.total_debt_usd * close_factor;
    let max_liquidatable = (max_liquidatable_debt_usd) as u64;
    
    // Seizable collateral hesaplama
    // Likidasyon bonusu ile birlikte alınacak teminat
    // Formül: liquidated_debt * (1 + liquidation_bonus)
    let seizable_collateral_usd = max_liquidatable_debt_usd * (1.0 + liquidation_bonus);
    let seizable_collateral = seizable_collateral_usd as u64;
    
    // Estimated profit hesaplama
    // Profit = Seizable collateral - Liquidated debt - Transaction fees
    let estimated_profit = seizable_collateral_usd - max_liquidatable_debt_usd - transaction_fee_estimate;
    
    // Minimum profit kontrolü (config'ten)
    if estimated_profit < config.min_profit_usd {
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
        estimated_profit_usd: estimated_profit,
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

