use crate::core::events::{Event, EventBus};
use crate::core::types::{Position, Opportunity};
use crate::core::config::Config;
use crate::protocol::Protocol;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct Analyzer {
    event_bus: EventBus,
    protocol: Arc<dyn Protocol>,
    config: Config,
}

impl Analyzer {
    pub fn new(
        event_bus: EventBus,
        protocol: Arc<dyn Protocol>,
        config: Config,
    ) -> Self {
        Analyzer {
            event_bus,
            protocol,
            config,
        }
    }

    pub async fn run(&self) -> Result<()> {
        let mut receiver = self.event_bus.subscribe();

        loop {
            match receiver.recv().await {
                Ok(Event::AccountUpdated { position, .. })
                | Ok(Event::AccountDiscovered { position, .. }) => {
                    log::debug!(
                        "Analyzer: received position update for {} (hf={})",
                        position.address,
                        position.health_factor
                    );
                    if self.is_liquidatable(&position) {
                        if let Some(opportunity) = self.calculate_opportunity(position).await {
                            log::info!(
                                "Analyzer: opportunity found for {} (net_profit={:.4}, debt_mint={}, collateral_mint={})",
                                opportunity.position.address,
                                opportunity.estimated_profit,
                                opportunity.debt_mint,
                                opportunity.collateral_mint
                            );
                            self.event_bus.publish(Event::OpportunityFound {
                                opportunity,
                            })?;
                        }
                    } else {
                        log::debug!(
                            "Analyzer: position {} is not liquidatable (hf={} >= threshold={})",
                            position.address,
                            position.health_factor,
                            self.config.hf_liquidation_threshold
                        );
                    }
                }
                Ok(_) => {
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("Analyzer lagged, skipped {} events", skipped);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    log::error!("Event bus closed, analyzer shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    fn is_liquidatable(&self, position: &Position) -> bool {
        position.health_factor < self.config.hf_liquidation_threshold
    }

    async fn calculate_opportunity(&self, position: Position) -> Option<Opportunity> {
        let params = self.protocol.liquidation_params();

        let max_liquidatable_usd = position.debt_usd * params.close_factor;
        let seizable_collateral_usd = max_liquidatable_usd * (1.0 + params.bonus);

        log::debug!(
            "Analyzer: calculating opportunity for {} -> debt_usd={:.4}, collateral_usd={:.4}, max_liquidatable_usd={:.4}, seizable_collateral_usd={:.4}",
            position.address,
            position.debt_usd,
            position.collateral_usd,
            max_liquidatable_usd,
            seizable_collateral_usd
        );

        let (debt_mint, collateral_mint) = self.select_best_pair(&position, max_liquidatable_usd, seizable_collateral_usd).await?;

        use crate::strategy::profit_calculator::ProfitCalculator;
        let profit_calc = ProfitCalculator::new(self.config.clone());
        
        let temp_opp = Opportunity {
            position: position.clone(),
            max_liquidatable: (max_liquidatable_usd * 1_000_000.0) as u64,
            seizable_collateral: (seizable_collateral_usd * 1_000_000.0) as u64,
            estimated_profit: 0.0,
            debt_mint,
            collateral_mint,
        };
        
        let net_profit = profit_calc.calculate_net_profit(&temp_opp);

        if net_profit < self.config.min_profit_usd {
            log::info!(
                "Analyzer: rejecting opportunity for {} - net_profit={:.4} < min_profit_usd={:.4}",
                position.address,
                net_profit,
                self.config.min_profit_usd
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

    async fn select_best_pair(
        &self,
        position: &Position,
        max_liquidatable_usd: f64,
        seizable_collateral_usd: f64,
    ) -> Option<(solana_sdk::pubkey::Pubkey, solana_sdk::pubkey::Pubkey)> {
        use crate::strategy::profit_calculator::ProfitCalculator;
        use crate::core::types::Opportunity;
        
        if position.debt_assets.is_empty() || position.collateral_assets.is_empty() {
            log::warn!(
                "Analyzer: position {} has empty debt_assets or collateral_assets, skipping",
                position.address
            );
            return None;
        }

        let profit_calc = ProfitCalculator::new(self.config.clone());
        let mut best_profit = f64::NEG_INFINITY;
        let mut best_pair: Option<(solana_sdk::pubkey::Pubkey, solana_sdk::pubkey::Pubkey)> = None;

        for debt_asset in &position.debt_assets {
            if debt_asset.amount_usd < max_liquidatable_usd * 0.1 {
                log::debug!(
                    "Analyzer: skipping debt asset {} (amount_usd={:.4} < 10% of max_liquidatable_usd={:.4})",
                    debt_asset.mint,
                    debt_asset.amount_usd,
                    max_liquidatable_usd
                );
                continue;
            }

            for collateral_asset in &position.collateral_assets {
                if collateral_asset.amount_usd < seizable_collateral_usd * 0.1 {
                    log::debug!(
                        "Analyzer: skipping collateral asset {} (amount_usd={:.4} < 10% of seizable_collateral_usd={:.4})",
                        collateral_asset.mint,
                        collateral_asset.amount_usd,
                        seizable_collateral_usd
                    );
                    continue;
                }

                let temp_opp = Opportunity {
                    position: position.clone(),
                    max_liquidatable: (max_liquidatable_usd * 1_000_000.0) as u64,
                    seizable_collateral: (seizable_collateral_usd * 1_000_000.0) as u64,
                    estimated_profit: 0.0,
                    debt_mint: debt_asset.mint,
                    collateral_mint: collateral_asset.mint,
                };

                let net_profit = profit_calc.calculate_net_profit(&temp_opp);

                if net_profit > best_profit {
                    best_profit = net_profit;
                    best_pair = Some((debt_asset.mint, collateral_asset.mint));
                    log::debug!(
                        "Analyzer: new best pair for {} -> debt_mint={}, collateral_mint={}, net_profit={:.4}",
                        position.address,
                        debt_asset.mint,
                        collateral_asset.mint,
                        net_profit
                    );
                }
            }
        }

        if best_pair.is_none() {
            log::info!(
                "Analyzer: no profitable debt/collateral pair found for position {}",
                position.address
            );
        }

        best_pair
    }
}
