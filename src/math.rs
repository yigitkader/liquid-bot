use crate::config::Config;
use crate::domain::{AccountPosition, LiquidationOpportunity};
use crate::protocol::Protocol;
use crate::protocols::oracle_helper::{get_oracle_accounts_from_mint, read_oracle_price};
use crate::solana_client::SolanaClient;
use anyhow::Result;
use std::sync::Arc;

fn calculate_transaction_fee_usd(
    compute_units: u32,
    priority_fee_per_cu: u64,
    sol_price_usd: f64,
) -> f64 {
    // todo: validate is it correct Base transaction fee (Solana base fee)
    const BASE_FEE_LAMPORTS: u64 = 5_000; // ~0.000005 SOL
    let priority_fee_lamports = (compute_units as u64) * priority_fee_per_cu / 1_000_000;
    let total_fee_lamports = BASE_FEE_LAMPORTS + priority_fee_lamports;
    let total_fee_sol = total_fee_lamports as f64 / 1_000_000_000.0;
    total_fee_sol * sol_price_usd
}

fn calculate_slippage_cost_usd(
    amount_usd: f64,
    slippage_bps: u16, // Basis points (100 bps = 1%)
) -> f64 {
    amount_usd * (slippage_bps as f64 / 10_000.0)
}

/// Token swap maliyeti hesaplama (eğer gerekirse)
///
/// todo: validate => Eğer collateral ve debt farklı token'larsa, swap gerekebilir.
/// Swap maliyeti:
/// - DEX fee: %0.1 - %0.3 (Jupiter, Raydium)
/// - Price impact: Slippage'e dahil
fn calculate_swap_cost_usd(amount_usd: f64, needs_swap: bool) -> f64 {
    if !needs_swap {
        return 0.0;
    }

    // DEX fee (örnek: %0.2) // todo : is is correct info?
    const DEX_FEE_BPS: u16 = 20; // 0.2%
    amount_usd * (DEX_FEE_BPS as f64 / 10_000.0)
}

fn select_most_profitable_collateral(
    collateral_assets: &[crate::domain::CollateralAsset],
    required_collateral_usd: f64,
) -> Option<&crate::domain::CollateralAsset> {
    collateral_assets
        .iter()
        .filter(|asset| asset.amount_usd >= required_collateral_usd)
        .max_by(|a, b| {
            a.amount_usd
                .partial_cmp(&b.amount_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.ltv
                        .partial_cmp(&b.ltv)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        })
}

fn select_most_profitable_debt(
    debt_assets: &[crate::domain::DebtAsset],
) -> Option<&crate::domain::DebtAsset> {
    debt_assets.iter().max_by(|a, b| {
        a.amount_usd
            .partial_cmp(&b.amount_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.borrow_rate
                    .partial_cmp(&a.borrow_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    })
}

pub async fn calculate_liquidation_opportunity(
    position: &AccountPosition,
    config: &Config,
    protocol: Arc<dyn Protocol>,
    rpc_client: Option<Arc<SolanaClient>>,
) -> Result<Option<LiquidationOpportunity>> {
    if position.health_factor >= 1.0 || position.total_debt_usd <= 0.0 {
        return Ok(None);
    }

    let liquidation_params = protocol.get_liquidation_params();
    let close_factor = liquidation_params.close_factor;
    let liquidation_bonus = liquidation_params.liquidation_bonus;

    let max_liquidatable_debt_usd = position.total_debt_usd * close_factor;

    let seizable_collateral_usd = max_liquidatable_debt_usd * (1.0 + liquidation_bonus);

    let debt_asset = select_most_profitable_debt(&position.debt_assets)
        .ok_or_else(|| anyhow::anyhow!("No debt assets found in position"))?;

    let collateral_asset =
        select_most_profitable_collateral(&position.collateral_assets, seizable_collateral_usd)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No sufficient collateral assets found. Required: ${:.2}, Available: {:?}",
                    seizable_collateral_usd,
                    position
                        .collateral_assets
                        .iter()
                        .map(|a| a.amount_usd)
                        .collect::<Vec<_>>()
                )
            })?;

    let debt_token_price_usd = if debt_asset.amount > 0 {
        debt_asset.amount_usd / debt_asset.amount as f64
    } else {
        return Ok(None); // Division by zero protection
    };

    let max_liquidatable = (max_liquidatable_debt_usd / debt_token_price_usd) as u64;
    let collateral_token_price_usd = if collateral_asset.amount > 0 {
        collateral_asset.amount_usd / collateral_asset.amount as f64
    } else {
        return Ok(None); // Division by zero protection
    };

    let seizable_collateral = (seizable_collateral_usd / collateral_token_price_usd) as u64;

    const LIQUIDATION_COMPUTE_UNITS: u32 = 200_000;
    const PRIORITY_FEE_PER_CU: u64 = 1_000; // todo: micro-lamports per CU (config'ten alınabilir)
    const SOL_PRICE_USD: f64 = 150.0; // todo: SOL fiyatı (yaklaşık, gerçekte oracle'dan alınmalı)

    let transaction_fee_usd = calculate_transaction_fee_usd(
        LIQUIDATION_COMPUTE_UNITS,
        PRIORITY_FEE_PER_CU,
        SOL_PRICE_USD,
    );

    let size_multiplier = if seizable_collateral_usd < 10_000.0 {
        0.5
    } else if seizable_collateral_usd > 100_000.0 {
        0.8
    } else {
        0.6
    };

    let estimated_slippage_bps = (config.max_slippage_bps as f64 * size_multiplier) as u16;

    let oracle_slippage_bps = if let Some(client) = &rpc_client {
        match collateral_asset.mint.parse::<solana_sdk::pubkey::Pubkey>() {
            Ok(mint_pubkey) => {
                match get_oracle_accounts_from_mint(&mint_pubkey) {
                    Ok((pyth, switchboard)) => {
                        match read_oracle_price(
                            pyth.as_ref(),
                            switchboard.as_ref(),
                            Arc::clone(client),
                        )
                        .await
                        {
                            Ok(Some(oracle_price)) => {
                                let confidence_ratio = oracle_price.confidence / oracle_price.price;
                                let confidence_bps = (confidence_ratio * 10_000.0) as u16;
                                log::debug!(
                                    "Oracle confidence slippage: {} bps (confidence: ${:.4}, price: ${:.4})",
                                    confidence_bps,
                                    oracle_price.confidence,
                                    oracle_price.price
                                );
                                confidence_bps
                            }
                            Ok(None) => {
                                log::warn!("Oracle price not available, using default confidence slippage: 100 bps");
                                100 // Varsayılan: %1 // todo : is it correct?
                            }
                            Err(e) => {
                                log::warn!("Failed to read oracle price: {}, using default confidence slippage: 100 bps", e);
                                100 // Varsayılan: %1 // todo : is it correct?
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to get oracle accounts: {}, using default confidence slippage: 100 bps", e);
                        100 // Varsayılan: %1 // todo : is it correct?
                    }
                }
            }
            Err(e) => {
                log::warn!("Failed to parse collateral mint: {}, using default confidence slippage: 100 bps", e);
                100 // Varsayılan: %1 // todo : is it correct?
            }
        }
    } else {
        log::debug!("RPC client not provided, using default oracle confidence slippage: 100 bps");
        100 // Varsayılan: %1 (RPC client yoksa) // todo : is it correct?
    };

    // Total slippage = (todo: neden tahmin?)tahmini slippage + oracle confidence slippage
    let total_slippage_bps = estimated_slippage_bps.saturating_add(oracle_slippage_bps);
    let base_slippage_cost_usd =
        calculate_slippage_cost_usd(seizable_collateral_usd, total_slippage_bps);

    // Güvenlik marjı ekle (%20) - gerçek slippage tahminden daha yüksek olabilir todo: validate
    let slippage_cost_usd = base_slippage_cost_usd * 1.2;

    log::debug!(
        "Slippage calculation: estimated={} bps, oracle_confidence={} bps, total={} bps, cost=${:.4} (with 20% safety margin)",
        estimated_slippage_bps,
        oracle_slippage_bps,
        total_slippage_bps,
        slippage_cost_usd
    );

    let needs_swap = collateral_asset.mint != debt_asset.mint;

    let swap_cost_usd = if needs_swap {
        calculate_swap_cost_usd(seizable_collateral_usd, true)
    } else {
        0.0
    };

    let total_cost_usd = transaction_fee_usd + slippage_cost_usd + swap_cost_usd;
    let gross_profit_usd = seizable_collateral_usd - max_liquidatable_debt_usd;
    let estimated_profit_usd = gross_profit_usd - total_cost_usd;

    const MIN_PROFIT_MARGIN_BPS: u16 = 100; // %1 = 100 basis points // todo : is it correct?
    let min_profit_margin_usd =
        max_liquidatable_debt_usd * (MIN_PROFIT_MARGIN_BPS as f64 / 10_000.0);

    if estimated_profit_usd < min_profit_margin_usd {
        log::debug!(
            "Opportunity rejected: estimated profit ${:.2} < min margin ${:.2} ({}% of debt)",
            estimated_profit_usd,
            min_profit_margin_usd,
            MIN_PROFIT_MARGIN_BPS as f64 / 100.0
        );
        return Ok(None);
    }

    log::debug!(
        "Profit calculation: gross=${:.2}, tx_fee=${:.4}, slippage=${:.4}, swap=${:.4}, total_cost=${:.4}, net=${:.2}",
        gross_profit_usd,
        transaction_fee_usd,
        slippage_cost_usd,
        swap_cost_usd,
        total_cost_usd,
        estimated_profit_usd
    );

    if estimated_profit_usd < config.min_profit_usd {
        log::debug!(
            "Opportunity rejected: estimated profit ${:.2} < min ${:.2}",
            estimated_profit_usd,
            config.min_profit_usd
        );
        return Ok(None);
    }

    let target_debt_mint = debt_asset.mint.clone();
    let target_collateral_mint = collateral_asset.mint.clone();

    Ok(Some(LiquidationOpportunity {
        account_position: position.clone(),
        max_liquidatable_amount: max_liquidatable,
        seizable_collateral,
        liquidation_bonus,
        estimated_profit_usd, // todo:(neden tahmin?) Zaten konservatif tahmin (slippage, fees, swap dahil)
        target_debt_mint,
        target_collateral_mint,
    }))
}

pub fn calculate_health_factor(total_collateral_usd: f64, total_debt_usd: f64, ltv: f64) -> f64 {
    if total_debt_usd == 0.0 {
        return f64::INFINITY;
    }

    (total_collateral_usd * ltv) / total_debt_usd
}
