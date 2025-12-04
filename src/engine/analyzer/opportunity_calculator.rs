/// Opportunity calculation logic for the analyzer
/// 
/// This module handles the core logic for:
/// - Determining if a position is liquidatable
/// - Calculating liquidation opportunities
/// - Selecting optimal debt/collateral pairs

use crate::core::config::Config;
use crate::core::types::{Opportunity, Position};
use crate::protocol::Protocol;
use std::sync::Arc;

pub struct OpportunityCalculator;

impl OpportunityCalculator {
    /// Check if a position is liquidatable based on health factor
    pub fn is_liquidatable(position: &Position, config: &Config) -> bool {
        position.health_factor
            < (config.hf_liquidation_threshold * config.liquidation_safety_margin)
    }

    /// Calculate a liquidation opportunity for a position
    /// 
    /// Returns `None` if:
    /// - Position is not liquidatable
    /// - No profitable opportunity found
    /// - Insufficient debt or collateral
    pub async fn calculate_opportunity(
        position: Position,
        protocol: Arc<dyn Protocol>,
        config: Config,
    ) -> Option<Opportunity> {
        let params = protocol.liquidation_params();

        let max_liquidatable_usd = position.debt_usd * params.close_factor;
        let seizable_collateral_usd = max_liquidatable_usd * (1.0 + params.bonus);

        let (debt_mint, collateral_mint) = Self::select_best_pair(
            &position,
            max_liquidatable_usd,
            seizable_collateral_usd,
            &config,
        )
        .await?;

        // Validate that selected debt and collateral assets have sufficient amounts
        let debt_asset = position
            .debt_assets
            .iter()
            .find(|a| a.mint == debt_mint);

        if let Some(debt_asset) = debt_asset {
            if debt_asset.amount_usd < max_liquidatable_usd {
                log::debug!(
                    "OpportunityCalculator: Insufficient debt in selected asset {}: need ${:.2}, have ${:.2}",
                    debt_mint,
                    max_liquidatable_usd,
                    debt_asset.amount_usd
                );
                return None;
            }
        } else {
            log::debug!(
                "OpportunityCalculator: Selected debt mint {} not found in position debt assets",
                debt_mint
            );
            return None;
        }

        let collateral_asset = position
            .collateral_assets
            .iter()
            .find(|a| a.mint == collateral_mint);

        if let Some(collateral_asset) = collateral_asset {
            if collateral_asset.amount_usd < seizable_collateral_usd {
                log::debug!(
                    "OpportunityCalculator: Insufficient collateral in selected asset {}: need ${:.2}, have ${:.2}",
                    collateral_mint,
                    seizable_collateral_usd,
                    collateral_asset.amount_usd
                );
                return None;
            }
        } else {
            log::debug!(
                "OpportunityCalculator: Selected collateral mint {} not found in position collateral assets",
                collateral_mint
            );
            return None;
        }

        use crate::strategy::profit_calculator::ProfitCalculator;
        let profit_calc = ProfitCalculator::new(config.clone());

        let temp_opp = Opportunity {
            position: position.clone(),
            max_liquidatable: (max_liquidatable_usd * 1_000_000.0) as u64,
            seizable_collateral: (seizable_collateral_usd * 1_000_000.0) as u64,
            estimated_profit: 0.0,
            debt_mint,
            collateral_mint,
        };

        // Get hop_count from slippage estimator (handles fallback internally)
        let hop_count = if debt_mint != collateral_mint {
            use crate::strategy::slippage_estimator::SlippageEstimator;
            let estimator = SlippageEstimator::new(config.clone());
            let amount = 1_000_000u64;
            match estimator
                .estimate_dex_slippage_with_route(debt_mint, collateral_mint, amount)
                .await
            {
                Ok((_slippage, hop_count)) => {
                    log::debug!(
                        "OpportunityCalculator: Got hop_count={} for swap {} -> {}",
                        hop_count,
                        debt_mint,
                        collateral_mint
                    );
                    Some(hop_count)
                }
                Err(e) => {
                    log::error!(
                        "OpportunityCalculator: Failed to get hop count from slippage estimator: {} - using conservative default",
                        e
                    );
                    let is_stablecoin_pair = profit_calc.is_stablecoin_pair(&debt_mint, &collateral_mint);
                    let estimated_hop_count = if is_stablecoin_pair { 1 } else { 3 };
                    log::debug!(
                        "OpportunityCalculator: Using emergency fallback hop_count={} for {} -> {} (stablecoin_pair={})",
                        estimated_hop_count,
                        debt_mint,
                        collateral_mint,
                        is_stablecoin_pair
                    );
                    Some(estimated_hop_count)
                }
            }
        } else {
            // No swap needed (same mint)
            Some(0)
        };

        let net_profit = profit_calc.calculate_net_profit(&temp_opp, hop_count).await;

        if net_profit < config.min_profit_usd {
            let gross = temp_opp.seizable_collateral as f64 / 1_000_000.0
                - temp_opp.max_liquidatable as f64 / 1_000_000.0;
            log::debug!(
                "OpportunityCalculator: Position {} rejected due to low profit: net=${:.6} < min=${:.2} (gross=${:.6}, debt_mint={}, collateral_mint={})",
                position.address,
                net_profit,
                config.min_profit_usd,
                gross,
                debt_mint,
                collateral_mint
            );
            return None;
        }

        Some(Opportunity {
            position,
            max_liquidatable: (max_liquidatable_usd * 1_000_000.0) as u64,
            seizable_collateral: (seizable_collateral_usd * 1_000_000.0) as u64,
            estimated_profit: net_profit,
            debt_mint,
            collateral_mint,
        })
    }

    /// Select the best debt/collateral pair for liquidation
    /// 
    /// Evaluates top debt and collateral assets to find the most profitable pair.
    async fn select_best_pair(
        position: &Position,
        max_liquidatable_usd: f64,
        seizable_collateral_usd: f64,
        config: &Config,
    ) -> Option<(solana_sdk::pubkey::Pubkey, solana_sdk::pubkey::Pubkey)> {
        use crate::strategy::profit_calculator::ProfitCalculator;

        if position.debt_assets.is_empty() || position.collateral_assets.is_empty() {
            return None;
        }

        let profit_calc = ProfitCalculator::new(config.clone());
        let mut best_profit = f64::NEG_INFINITY;
        let mut best_pair: Option<(solana_sdk::pubkey::Pubkey, solana_sdk::pubkey::Pubkey)> = None;

        // Consider top 2 debt assets and top 2 collateral assets
        let top_debts: Vec<_> = position
            .debt_assets
            .iter()
            .filter(|d| d.amount_usd >= max_liquidatable_usd * 0.1)
            .take(2)
            .collect();

        let top_collaterals: Vec<_> = position
            .collateral_assets
            .iter()
            .filter(|c| c.amount_usd >= seizable_collateral_usd * 0.1)
            .take(2)
            .collect();

        for debt_asset in &top_debts {
            for collateral_asset in &top_collaterals {
                let temp_opp = Opportunity {
                    position: position.clone(),
                    max_liquidatable: (max_liquidatable_usd * 1_000_000.0) as u64,
                    seizable_collateral: (seizable_collateral_usd * 1_000_000.0) as u64,
                    estimated_profit: 0.0,
                    debt_mint: debt_asset.mint,
                    collateral_mint: collateral_asset.mint,
                };

                let net_profit = profit_calc.calculate_net_profit(&temp_opp, None).await;

                if net_profit > best_profit {
                    best_profit = net_profit;
                    best_pair = Some((debt_asset.mint, collateral_asset.mint));
                }
            }
        }

        best_pair
    }
}

