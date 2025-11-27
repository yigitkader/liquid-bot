use crate::config::Config;
use crate::domain::{AccountPosition, LiquidationOpportunity};
use crate::protocol::Protocol;
use crate::protocols::oracle_helper::{get_oracle_accounts_from_mint, read_oracle_price};
use crate::solana_client::SolanaClient;
use anyhow::Result;
use std::sync::Arc;
use std::str::FromStr;

fn calculate_transaction_fee_usd(
    compute_units: u32,
    priority_fee_per_cu: u64,
    base_fee_lamports: u64,
    sol_price_usd: f64,
) -> f64 {
    // Base transaction fee: Solana charges a base fee per transaction
    // Reference: https://docs.solana.com/developing/programming-model/runtime#transaction-fees
    // Default: ~5,000 lamports = 0.000005 SOL
    let priority_fee_lamports = (compute_units as u64) * priority_fee_per_cu / 1_000_000;
    let total_fee_lamports = base_fee_lamports + priority_fee_lamports;
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
/// Note: Eğer collateral ve debt farklı token'larsa, swap gerekebilir.
/// Swap maliyeti:
/// - DEX fee: %0.1 - %0.3 (Jupiter, Raydium)
/// - Price impact: Slippage'e dahil
fn calculate_swap_cost_usd(amount_usd: f64, needs_swap: bool, dex_fee_bps: u16) -> f64 {
    if !needs_swap {
        return 0.0;
    }

    // DEX fee: Typical DEX fees are 0.1-0.3% (10-30 bps)
    // Jupiter and Raydium typically charge around 0.2% (20 bps)
    amount_usd * (dex_fee_bps as f64 / 10_000.0)
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
    
    // Get SOL price from oracle if available, otherwise use a conservative fallback
    let sol_price_usd = if let Some(client) = &rpc_client {
        // Try to get SOL price from oracle (SOL mint: So11111111111111111111111111111111111111112)
        let sol_mint = solana_sdk::pubkey::Pubkey::from_str("So11111111111111111111111111111111111111112")
            .unwrap();
        match get_oracle_accounts_from_mint(&sol_mint) {
            Ok((pyth, switchboard)) => {
                match read_oracle_price(pyth.as_ref(), switchboard.as_ref(), Arc::clone(client)).await {
                    Ok(Some(price)) => {
                        log::debug!("SOL price from oracle: ${:.2}", price.price);
                        price.price
                    }
                    _ => {
                        log::warn!("Failed to get SOL price from oracle, using fallback: $150");
                        150.0 // Fallback
                    }
                }
            }
            _ => {
                log::warn!("SOL oracle accounts not found, using fallback: $150");
                150.0 // Fallback
            }
        }
    } else {
        log::warn!("RPC client not available, using fallback SOL price: $150");
        150.0 // Fallback when RPC client not available
    };

    let transaction_fee_usd = calculate_transaction_fee_usd(
        LIQUIDATION_COMPUTE_UNITS,
        config.priority_fee_per_cu,
        config.base_transaction_fee_lamports,
        sol_price_usd,
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
                                // Pyth confidence = %68 confidence interval (1 sigma)
                                // %95 confidence interval için 2 sigma (2x çarpan) kullanılmalı
                                // Bot %68 confidence kullanırsa → Riski az gösterir
                                let confidence_95 = oracle_price.confidence * 2.0;
                                let confidence_ratio = confidence_95 / oracle_price.price;
                                let confidence_bps = (confidence_ratio * 10_000.0) as u16;
                                log::debug!(
                                    "Oracle confidence slippage: {} bps (confidence_68: ${:.4}, confidence_95: ${:.4}, price: ${:.4})",
                                    confidence_bps,
                                    oracle_price.confidence,
                                    confidence_95,
                                    oracle_price.price
                                );
                                confidence_bps
                            }
                            Ok(None) => {
                                log::warn!("Oracle price not available, using default confidence slippage: {} bps", config.default_oracle_confidence_slippage_bps);
                                config.default_oracle_confidence_slippage_bps
                            }
                            Err(e) => {
                                log::warn!("Failed to read oracle price: {}, using default confidence slippage: {} bps", e, config.default_oracle_confidence_slippage_bps);
                                config.default_oracle_confidence_slippage_bps
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to get oracle accounts: {}, using default confidence slippage: {} bps", e, config.default_oracle_confidence_slippage_bps);
                        config.default_oracle_confidence_slippage_bps
                    }
                }
            }
            Err(e) => {
                log::warn!("Failed to parse collateral mint: {}, using default confidence slippage: {} bps", e, config.default_oracle_confidence_slippage_bps);
                config.default_oracle_confidence_slippage_bps
            }
        }
    } else {
        log::debug!("RPC client not provided, using default oracle confidence slippage: {} bps", config.default_oracle_confidence_slippage_bps);
        config.default_oracle_confidence_slippage_bps
    };

    // Oracle confidence zaten slippage'in bir parçası, double counting yapmamak için
    // estimated_slippage ve oracle_slippage'i toplamak yerine max alıyoruz
    // Bu, oracle confidence'ın zaten price uncertainty'yi temsil ettiğini kabul eder
    let oracle_adjusted_slippage_bps = estimated_slippage_bps.max(oracle_slippage_bps);
    let base_slippage_cost_usd =
        calculate_slippage_cost_usd(seizable_collateral_usd, oracle_adjusted_slippage_bps);

    // Safety margin: 10% (1.1x) yeterli - %20 çok yüksek ve kârı gereksiz azaltıyor
    // Gerçek slippage genelde tahminlerden çok farklı değil, %10 buffer yeterli
    let slippage_cost_usd = base_slippage_cost_usd * 1.1;

    log::debug!(
        "Slippage calculation: estimated={} bps, oracle_confidence={} bps, adjusted={} bps (max), cost=${:.4} (with 10% safety margin)",
        estimated_slippage_bps,
        oracle_slippage_bps,
        oracle_adjusted_slippage_bps,
        slippage_cost_usd
    );

    let needs_swap = collateral_asset.mint != debt_asset.mint;

    let swap_cost_usd = if needs_swap {
        calculate_swap_cost_usd(seizable_collateral_usd, true, config.dex_fee_bps)
    } else {
        0.0
    };

    let total_cost_usd = transaction_fee_usd + slippage_cost_usd + swap_cost_usd;
    let gross_profit_usd = seizable_collateral_usd - max_liquidatable_debt_usd;
    let estimated_profit_usd = gross_profit_usd - total_cost_usd;

    // Minimum profit margin: ensures we have a buffer above transaction costs
    // Default: 1% (100 bps) of debt amount
    let min_profit_margin_usd =
        max_liquidatable_debt_usd * (config.min_profit_margin_bps as f64 / 10_000.0);

    if estimated_profit_usd < min_profit_margin_usd {
        log::debug!(
            "Opportunity rejected: estimated profit ${:.2} < min margin ${:.2} ({}% of debt)",
            estimated_profit_usd,
            min_profit_margin_usd,
            config.min_profit_margin_bps as f64 / 100.0
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
        estimated_profit_usd, // Conservative estimate: includes slippage, fees, and swap costs
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
