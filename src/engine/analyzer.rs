use crate::core::config::Config;
use crate::core::events::{Event, EventBus};
use crate::core::types::{Opportunity, Position};
use crate::protocol::Protocol;
use anyhow::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinSet;

pub struct Analyzer {
    event_bus: EventBus,
    protocol: Arc<dyn Protocol>,
    config: Config,
    current_workers: Arc<AtomicUsize>,
    max_workers_limit: usize,
    metrics: Option<Arc<crate::utils::metrics::Metrics>>,
}

impl Analyzer {
    pub fn new(event_bus: EventBus, protocol: Arc<dyn Protocol>, config: Config) -> Self {
        let initial_workers = config.analyzer_max_workers;
        let max_workers_limit = config.analyzer_max_workers_limit;
        Analyzer {
            event_bus,
            protocol,
            config,
            current_workers: Arc::new(AtomicUsize::new(initial_workers)),
            max_workers_limit,
            metrics: None,
        }
    }

    pub fn with_metrics(mut self, metrics: Arc<crate::utils::metrics::Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub async fn run(&self) -> Result<()> {
        use std::time::Instant;
        use tokio::time::Duration;

        let mut receiver = self.event_bus.subscribe();
        let mut tasks = JoinSet::new();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            self.current_workers.load(Ordering::Relaxed),
        ));

        let mut last_lag = Instant::now();
        let mut last_scale_down = Instant::now();
        let mut last_cleanup = Instant::now();
        const SCALE_DOWN_THRESHOLD: Duration = Duration::from_secs(60);
        const SCALE_DOWN_INTERVAL: Duration = Duration::from_secs(30);
        // ✅ FIX: Task limit to prevent unlimited task spawning
        const MAX_CONCURRENT_TASKS: usize = 1000;
        // ✅ FIX: Periodic cleanup interval to prevent task queue from staying full
        const CLEANUP_INTERVAL: Duration = Duration::from_millis(100);

        'main_loop: loop {
            // ✅ FIX: Use tokio::select! to handle multiple events concurrently
            // This allows periodic cleanup even when events are arriving slowly
            tokio::select! {
                event_result = receiver.recv() => {
                    match event_result {
                Ok(Event::AccountUpdated { position, .. })
                | Ok(Event::AccountDiscovered { position, .. }) => {
                    // ✅ FIX: Check task limit before spawning
                    if tasks.len() >= MAX_CONCURRENT_TASKS {
                        log::warn!(
                            "Analyzer: Task queue full ({} tasks), dropping event for position {}",
                            tasks.len(),
                            position.address
                        );
                        // Skip spawning task - continue to next loop iteration
                        // Note: This is inside tokio::select!, so we just skip the rest of this branch
                    } else {

                    let permit = match semaphore.clone().acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => {
                            log::warn!("Semaphore closed, analyzer shutting down");
                            break 'main_loop;
                        }
                    };

                    let event_bus = self.event_bus.clone();
                    let protocol = Arc::clone(&self.protocol);
                    let config = self.config.clone();
                    let position = position.clone();
                    let metrics = self.metrics.as_ref().map(Arc::clone);

                    tasks.spawn(async move {
                        let _permit = permit;

                        // Run actual work
                        // ✅ FIX: Permit will be dropped here (RAII), releasing semaphore even if task panics
                        // Panics are caught by JoinSet in the cleanup loop below
                        if Self::is_liquidatable_static(&position, &config) {
                            if let Some(opportunity) =
                                Self::calculate_opportunity_static(position.clone(), protocol, config).await
                            {
                                log::info!(
                                    "Analyzer: ✅ Opportunity found for position {} (HF={:.6}, debt={}, collateral={}, max_liquidatable={}, est_profit=${:.4})",
                                    opportunity.position.address,
                                    opportunity.position.health_factor,
                                    opportunity.debt_mint,
                                    opportunity.collateral_mint,
                                    opportunity.max_liquidatable,
                                    opportunity.estimated_profit
                                );
                                if let Some(ref metrics) = metrics {
                                    metrics.record_opportunity();
                                }
                                let _ = event_bus.publish(Event::OpportunityFound { opportunity });
                            } else {
                                log::debug!(
                                    "Analyzer: Position {} is liquidatable (HF={:.6}) but no profitable opportunity calculated",
                                    position.address,
                                    position.health_factor
                                );
                            }
                        }
                    });
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!(
                        "⚠️  CRITICAL: Analyzer lagged {} events - these events are LOST FOREVER!",
                        skipped
                    );
                    log::warn!("   Consider: 1) Increase EVENT_BUS_BUFFER_SIZE");
                    log::warn!("            2) Increase ANALYZER_MAX_WORKERS_LIMIT");
                    log::warn!("            3) Optimize calculation logic");
                    log::warn!("            4) Restart bot to trigger full account re-scan");

                    last_lag = Instant::now();

                    let current_target = self.current_workers.load(Ordering::Relaxed);
                    const MAX_CONCURRENT_VALIDATIONS: usize = 100;

                    if current_target < self.max_workers_limit {
                        let increase = std::cmp::max(2, current_target / 2);
                        let new_target =
                            std::cmp::min(current_target + increase, self.max_workers_limit);

                        let current_available = semaphore.available_permits();

                        let permits_to_add = if current_available < new_target {
                            new_target - current_available
                        } else {
                            // Already have enough or too many permits (leak scenario)
                            // Just update target, don't add more permits
                            0
                        };

                        let estimated_total_after = current_available + permits_to_add;
                        let final_permits_to_add =
                            if estimated_total_after > MAX_CONCURRENT_VALIDATIONS {
                                MAX_CONCURRENT_VALIDATIONS.saturating_sub(current_available)
                            } else {
                                permits_to_add
                            };

                        if final_permits_to_add > 0 {
                            semaphore.add_permits(final_permits_to_add);
                            let actual_new_target = current_target + final_permits_to_add;
                            self.current_workers
                                .store(actual_new_target, Ordering::Relaxed);

                            if estimated_total_after > MAX_CONCURRENT_VALIDATIONS {
                                log::warn!(
                                    "⚠️  Analyzer lagged, skipped {} events. Increasing workers: {} -> {} (added {} permits, available: {}, capped at max {})",
                                    skipped,
                                    current_target,
                                    actual_new_target,
                                    final_permits_to_add,
                                    semaphore.available_permits(),
                                    MAX_CONCURRENT_VALIDATIONS
                                );
                            } else {
                                log::warn!(
                                    "⚠️  Analyzer lagged, skipped {} events. Increasing workers: {} -> {} (added {} permits, available: {})",
                                    skipped,
                                    current_target,
                                    actual_new_target,
                                    final_permits_to_add,
                                    semaphore.available_permits()
                                );
                            }
                        } else if permits_to_add > 0 {
                            log::error!(
                                "⚠️  CRITICAL: Analyzer lagged {} events, but cannot add more permits (max concurrent: {}, available: {})",
                                skipped,
                                MAX_CONCURRENT_VALIDATIONS,
                                current_available
                            );
                        } else {
                            self.current_workers.store(new_target, Ordering::Relaxed);

                            log::warn!(
                                "⚠️  Analyzer lagged, skipped {} events. Target updated: {} -> {} (permits already sufficient: available={}, target={})",
                                skipped,
                                current_target,
                                new_target,
                                current_available,
                                new_target
                            );
                        }
                    } else {
                        log::error!(
                            "⚠️  CRITICAL: Analyzer lagged {} events, max workers ({}) reached!",
                            skipped,
                            self.max_workers_limit
                        );
                        log::error!("   Consider: 1) Increase EVENT_BUS_BUFFER_SIZE");
                        log::error!("            2) Increase ANALYZER_MAX_WORKERS_LIMIT");
                        log::error!("            3) Optimize calculation logic");
                    }
                }
                        Err(broadcast::error::RecvError::Closed) => {
                            log::error!("Event bus closed, analyzer shutting down");
                            break 'main_loop;
                        }
                    }
                }
                // ✅ FIX: Periodic cleanup - remove completed tasks even when no events arrive
                // This prevents task queue from staying full when tasks complete slowly
                _ = tokio::time::sleep(CLEANUP_INTERVAL) => {
                    // Only cleanup if enough time has passed since last cleanup
                    if last_cleanup.elapsed() >= CLEANUP_INTERVAL {
                        // Cleanup completed tasks
                        let mut cleaned_count = 0;
                        while let Some(result) = tasks.try_join_next() {
                            cleaned_count += 1;
                            if let Err(e) = result {
                                log::error!("Analyzer task failed: {}", e);
                            }
                        }
                        
                        if cleaned_count > 0 {
                            log::debug!(
                                "Analyzer: Cleaned up {} completed task(s), {} remaining (last cleanup: {}ms ago)",
                                cleaned_count,
                                tasks.len(),
                                last_cleanup.elapsed().as_millis()
                            );
                        }
                        
                        // If task queue is still full after cleanup, log warning
                        if tasks.len() >= MAX_CONCURRENT_TASKS {
                            log::warn!(
                                "Analyzer: Task queue still full after cleanup ({} tasks). Tasks may be completing slowly.",
                                tasks.len()
                            );
                        }
                        
                        last_cleanup = Instant::now();
                    }
                }
            }

            // ✅ FIX: Also cleanup after processing each event (non-blocking)
            // This ensures tasks are cleaned up as soon as they complete
            while let Some(result) = tasks.try_join_next() {
                if let Err(e) = result {
                    log::error!("Analyzer task failed: {}", e);
                }
            }

            // ✅ FIX: Also cleanup after processing each event (non-blocking)
            // This ensures tasks are cleaned up as soon as they complete
            while let Some(result) = tasks.try_join_next() {
                if let Err(e) = result {
                    log::error!("Analyzer task failed: {}", e);
                }
            }

            if last_lag.elapsed() > SCALE_DOWN_THRESHOLD
                && last_scale_down.elapsed() > SCALE_DOWN_INTERVAL
            {
                let current = self.current_workers.load(Ordering::Relaxed);
                let target_workers = self.config.analyzer_max_workers;

                if current > target_workers {
                    let decrease = std::cmp::max(1, current / 4);
                    let new_workers = std::cmp::max(target_workers, current - decrease);

                    self.current_workers.store(new_workers, Ordering::Relaxed);
                    last_scale_down = Instant::now();

                    log::info!(
                        "Analyzer: Scaling down target (no lag for {}s): {} -> {} (available permits: {})",
                        SCALE_DOWN_THRESHOLD.as_secs(),
                        current,
                        new_workers,
                        semaphore.available_permits()
                    );
                }
            }
        }

        Ok(())
    }

    fn is_liquidatable_static(position: &Position, config: &Config) -> bool {
        position.health_factor
            < (config.hf_liquidation_threshold * config.liquidation_safety_margin)
    }

    async fn calculate_opportunity_static(
        position: Position,
        protocol: Arc<dyn Protocol>,
        config: Config,
    ) -> Option<Opportunity> {
        let params = protocol.liquidation_params();

        let max_liquidatable_usd = position.debt_usd * params.close_factor;
        let seizable_collateral_usd = max_liquidatable_usd * (1.0 + params.bonus);

        let (debt_mint, collateral_mint) = Self::select_best_pair_static(
            &position,
            max_liquidatable_usd,
            seizable_collateral_usd,
            &config,
        )
        .await?;

        // ✅ FIX: Validate that selected debt and collateral assets have sufficient amounts
        // This prevents attempting to liquidate more than available, which would cause transaction failure
        let debt_asset = position
            .debt_assets
            .iter()
            .find(|a| a.mint == debt_mint);

        if let Some(debt_asset) = debt_asset {
            if debt_asset.amount_usd < max_liquidatable_usd {
                log::debug!(
                    "Analyzer: Insufficient debt in selected asset {}: need ${:.2}, have ${:.2}",
                    debt_mint,
                    max_liquidatable_usd,
                    debt_asset.amount_usd
                );
                return None;
            }
        } else {
            log::debug!(
                "Analyzer: Selected debt mint {} not found in position debt assets",
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
                    "Analyzer: Insufficient collateral in selected asset {}: need ${:.2}, have ${:.2}",
                    collateral_mint,
                    seizable_collateral_usd,
                    collateral_asset.amount_usd
                );
                return None;
            }
        } else {
            log::debug!(
                "Analyzer: Selected collateral mint {} not found in position collateral assets",
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

        // ✅ FIX: Determine hop_count with conservative fallback
        // If Jupiter API fails or is disabled, use conservative estimate based on pair type
        let hop_count = if debt_mint != collateral_mint {
            if config.use_jupiter_api {
                use crate::strategy::slippage_estimator::SlippageEstimator;
                let estimator = SlippageEstimator::new(config.clone());
                // Use a reasonable amount for quote (1M = 1 token with 6 decimals)
                let amount = 1_000_000u64;
                match estimator
                    .estimate_dex_slippage_with_route(debt_mint, collateral_mint, amount)
                    .await
                {
                    Ok((_slippage, hop_count)) => {
                        log::debug!(
                            "Analyzer: Got hop_count={} from Jupiter API for swap {} -> {}",
                            hop_count,
                            debt_mint,
                            collateral_mint
                        );
                        Some(hop_count)
                    }
                    Err(e) => {
                        log::warn!(
                            "Analyzer: Failed to get hop count from Jupiter API: {} - using conservative estimate",
                            e
                        );
                        // ✅ Fallback: Conservative estimate based on pair type
                        // Stablecoin pairs typically need 1 hop, others may need 2+ hops
                        let is_stablecoin_pair = profit_calc.is_stablecoin_pair(&debt_mint, &collateral_mint);
                        let estimated_hop_count = if is_stablecoin_pair { 1 } else { 2 };
                        log::debug!(
                            "Analyzer: Using conservative hop_count={} for {} -> {} (stablecoin_pair={})",
                            estimated_hop_count,
                            debt_mint,
                            collateral_mint,
                            is_stablecoin_pair
                        );
                        Some(estimated_hop_count)
                    }
                }
            } else {
                // ✅ Jupiter API disabled - use conservative estimate
                let is_stablecoin_pair = profit_calc.is_stablecoin_pair(&debt_mint, &collateral_mint);
                let estimated_hop_count = if is_stablecoin_pair { 1 } else { 2 };
                log::debug!(
                    "Analyzer: Jupiter API disabled, using conservative hop_count={} for {} -> {} (stablecoin_pair={})",
                    estimated_hop_count,
                    debt_mint,
                    collateral_mint,
                    is_stablecoin_pair
                );
                Some(estimated_hop_count)
            }
        } else {
            // No swap needed (same mint)
            Some(0)
        };

        let net_profit = profit_calc.calculate_net_profit(&temp_opp, hop_count);

        if net_profit < config.min_profit_usd {
            let gross = temp_opp.seizable_collateral as f64 / 1_000_000.0
                - temp_opp.max_liquidatable as f64 / 1_000_000.0;
            log::debug!(
                "Analyzer: Position {} rejected due to low profit: net=${:.6} < min=${:.2} (gross=${:.6}, debt_mint={}, collateral_mint={})",
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

    async fn select_best_pair_static(
        position: &Position,
        max_liquidatable_usd: f64,
        seizable_collateral_usd: f64,
        config: &Config,
    ) -> Option<(solana_sdk::pubkey::Pubkey, solana_sdk::pubkey::Pubkey)> {
        use crate::core::types::Opportunity;
        use crate::strategy::profit_calculator::ProfitCalculator;

        if position.debt_assets.is_empty() || position.collateral_assets.is_empty() {
            return None;
        }

        let profit_calc = ProfitCalculator::new(config.clone());
        let mut best_profit = f64::NEG_INFINITY;
        let mut best_pair: Option<(solana_sdk::pubkey::Pubkey, solana_sdk::pubkey::Pubkey)> = None;

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

                let net_profit = profit_calc.calculate_net_profit(&temp_opp, None);

                if net_profit > best_profit {
                    best_profit = net_profit;
                    best_pair = Some((debt_asset.mint, collateral_asset.mint));
                }
            }
        }

        best_pair
    }
}
