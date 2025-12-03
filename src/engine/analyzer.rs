use crate::core::events::{Event, EventBus};
use crate::core::types::{Position, Opportunity};
use crate::core::config::Config;
use crate::protocol::Protocol;
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::broadcast;
use tokio::task::JoinSet;

pub struct Analyzer {
    event_bus: EventBus,
    protocol: Arc<dyn Protocol>,
    config: Config,
    // Paralel processing için worker pool
    current_workers: Arc<AtomicUsize>,
    max_workers_limit: usize,
}

impl Analyzer {
    pub fn new(
        event_bus: EventBus,
        protocol: Arc<dyn Protocol>,
        config: Config,
    ) -> Self {
        let initial_workers = config.analyzer_max_workers;
        let max_workers_limit = config.analyzer_max_workers_limit;
        Analyzer {
            event_bus,
            protocol,
            config,
            current_workers: Arc::new(AtomicUsize::new(initial_workers)),
            max_workers_limit,
        }
    }

    pub async fn run(&self) -> Result<()> {
        use std::time::Instant;
        use tokio::time::Duration;
        
        let mut receiver = self.event_bus.subscribe();
        let mut tasks = JoinSet::new();
        
        // Dynamic semaphore that can be adjusted based on lag
        // Use Arc<Semaphore> directly - add_permits() is thread-safe
        let semaphore = Arc::new(
            tokio::sync::Semaphore::new(self.current_workers.load(Ordering::Relaxed))
        );

        // ✅ CRITICAL: Track lag and scale-down timing for adaptive scaling
        let mut last_lag = Instant::now();
        let mut last_scale_down = Instant::now();
        const SCALE_DOWN_THRESHOLD: Duration = Duration::from_secs(60); // Scale down if no lag for 60s
        const SCALE_DOWN_INTERVAL: Duration = Duration::from_secs(30); // Minimum 30s between scale downs

        loop {
            match receiver.recv().await {
                Ok(Event::AccountUpdated { position, .. })
                | Ok(Event::AccountDiscovered { position, .. }) => {
                    // BACKPRESSURE: Wait for available worker slot before processing
                    // This prevents buffer overflow by controlling the rate of event processing
                    let permit = match semaphore.clone().acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => {
                            log::warn!("Semaphore closed, analyzer shutting down");
                            break;
                        }
                    };
                    
                    // Clone dependencies for task
                    let event_bus = self.event_bus.clone();
                    let protocol = Arc::clone(&self.protocol);
                    let config = self.config.clone();
                    let position = position.clone();
                    
                    // Spawn parallel task
                    tasks.spawn(async move {
                        let _permit = permit; // Drop edildiğinde slot serbest kalır
                        
                        if Self::is_liquidatable_static(&position, &config) {
                            if let Some(opportunity) = Self::calculate_opportunity_static(
                                position,
                                protocol,
                                config,
                            ).await {
                                log::info!("Analyzer: opportunity found for {}", 
                                    opportunity.position.address);
                                let _ = event_bus.publish(Event::OpportunityFound {
                                    opportunity,
                                });
                            }
                        }
                    });
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // ✅ CRITICAL FIX: Log lagged events - these are lost forever!
                    // Buffer had too many events, some were dropped
                    // This indicates system is overloaded or buffer is too small
                    log::warn!(
                        "⚠️  CRITICAL: Analyzer lagged {} events - these events are LOST FOREVER!",
                        skipped
                    );
                    log::warn!("   Consider: 1) Increase EVENT_BUS_BUFFER_SIZE");
                    log::warn!("            2) Increase ANALYZER_MAX_WORKERS_LIMIT");
                    log::warn!("            3) Optimize calculation logic");
                    log::warn!("            4) Restart bot to trigger full account re-scan");
                    
                    // Update last lag time for scale-down logic
                    last_lag = Instant::now();
                    
                    // ADAPTIVE SCALING: Increase workers when lag detected
                    let current = self.current_workers.load(Ordering::Relaxed);
                    
                    // ✅ CRITICAL FIX: Hard limit to prevent semaphore from growing indefinitely
                    // This prevents memory leaks when tasks don't complete quickly
                    // Even if max_workers_limit is high, we cap at a reasonable concurrent limit
                    const MAX_CONCURRENT_VALIDATIONS: usize = 100;
                    
                    if current < self.max_workers_limit {
                        // Increase workers by 50% or at least 2, up to limit
                        let increase = std::cmp::max(2, current / 2);
                        let new_workers = std::cmp::min(
                            current + increase,
                            self.max_workers_limit
                        );
                        
                        // ✅ CRITICAL FIX: Add permits based on target, but respect limits
                        // Since scale down doesn't remove permits (just updates target),
                        // we need to be careful not to add too many permits
                        // Strategy: Add permits to reach new_workers target, but respect MAX_CONCURRENT_VALIDATIONS
                        let permits_to_add = new_workers.saturating_sub(current);
                        
                        // Respect MAX_CONCURRENT_VALIDATIONS limit
                        // Estimate total permits: current (best guess, may be higher if scale down didn't remove permits)
                        let estimated_total = current;
                        let max_allowed = MAX_CONCURRENT_VALIDATIONS.saturating_sub(estimated_total);
                        let permits_to_add = permits_to_add.min(max_allowed);
                        
                        if permits_to_add > 0 {
                            // Add permits to existing semaphore (preserves waiting tasks)
                            semaphore.add_permits(permits_to_add);
                            let actual_new_workers = current + permits_to_add;
                            self.current_workers.store(actual_new_workers, Ordering::Relaxed);
                            
                            log::warn!(
                                "⚠️  Analyzer lagged, skipped {} events. Increasing workers: {} -> {} (added {} permits, available: {})",
                                skipped,
                                current,
                                actual_new_workers,
                                permits_to_add,
                                semaphore.available_permits()
                            );
                        } else if permits_to_add == 0 && new_workers <= current {
                            // Target is already reached or exceeded (due to scale down not removing permits)
                            // Just update target to match current
                            self.current_workers.store(new_workers, Ordering::Relaxed);
                            log::warn!(
                                "⚠️  Analyzer lagged, skipped {} events. Target updated: {} -> {} (permits already sufficient, available: {})",
                                skipped,
                                current,
                                new_workers,
                                semaphore.available_permits()
                            );
                        } else {
                            log::error!(
                                "⚠️  CRITICAL: Analyzer lagged {} events, but cannot add more permits (max concurrent: {}, current: {})",
                                skipped,
                                MAX_CONCURRENT_VALIDATIONS,
                                current
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
                    break;
                }
            }
            
            // Periodically cleanup finished tasks
            while let Some(result) = tasks.try_join_next() {
                if let Err(e) = result {
                    log::error!("Analyzer task failed: {}", e);
                }
            }
            
            // ✅ CRITICAL: Scale DOWN if no lag for threshold duration
            // This prevents semaphore from staying at high permit count when system is idle
            // ✅ CRITICAL FIX: Don't try to remove permits - just update target
            // Problem with removing permits:
            //   - Scale down: 16 → 10 workers (6 permit kaldırılmalı)
            //   - 10 task çalışıyor, 6 permit available
            //   - Sadece 6 available permit kaldırılabiliyor
            //   - 4 permit hala task'larda kullanılıyor
            //   - Task'lar tamamlandığında 4 permit serbest kalıyor
            //   - Ama target 10, semaphore'da 10 permit var
            //   - Sonraki scale up'da bu 4 permit üzerine yeni permit'ler ekleniyor → Leak!
            // Solution: Just update target, don't remove permits
            //   - Task'lar tamamlandığında permit'ler serbest kalacak
            //   - Scale up sırasında mevcut permit sayısını kontrol edip sadece gerekli kadar ekleyeceğiz
            if last_lag.elapsed() > SCALE_DOWN_THRESHOLD 
                && last_scale_down.elapsed() > SCALE_DOWN_INTERVAL
            {
                let current = self.current_workers.load(Ordering::Relaxed);
                let target_workers = self.config.analyzer_max_workers;
                
                if current > target_workers {
                    // Scale down by 25% (or at least 1)
                    let decrease = std::cmp::max(1, current / 4);
                    let new_workers = std::cmp::max(
                        target_workers,
                        current - decrease
                    );
                    
                    // ✅ FIX: Just update target, don't try to remove permits
                    // Permits in use by running tasks will be released when tasks complete
                    // Scale up logic will check current permit count and only add what's needed
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
        // CRITICAL FIX: Use unified liquidation_safety_margin from config
        // Safety margin to avoid race conditions at exact threshold
        // With 0.95 margin (default), we only liquidate when HF < 0.95 * threshold
        // This gives us a 5% buffer to avoid competing with other bots at the exact threshold
        position.health_factor < (config.hf_liquidation_threshold * config.liquidation_safety_margin)
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
        ).await?;

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
        
        // ✅ Get hop count from Jupiter API if swap is needed
        // This ensures accurate multi-hop fee calculation
        let hop_count = if debt_mint != collateral_mint && config.use_jupiter_api {
            use crate::strategy::slippage_estimator::SlippageEstimator;
            let estimator = SlippageEstimator::new(config.clone());
            // Use a reasonable amount for quote (1M = 1 token with 6 decimals)
            let amount = 1_000_000u64;
            match estimator.estimate_dex_slippage_with_route(debt_mint, collateral_mint, amount).await {
                Ok((_slippage, hop_count)) => {
                    log::debug!(
                        "Analyzer: Got hop_count={} from Jupiter API for swap {} -> {}",
                        hop_count,
                        debt_mint,
                        collateral_mint
                    );
                    Some(hop_count)
                },
                Err(e) => {
                    log::warn!(
                        "Analyzer: Failed to get hop count from Jupiter API: {} - using default (1 hop)",
                        e
                    );
                    None // Fallback to 1 hop
                }
            }
        } else {
            None // No swap needed or Jupiter API disabled - will default to 1 hop
        };
        
        let net_profit = profit_calc.calculate_net_profit(&temp_opp, hop_count);

        if net_profit < config.min_profit_usd {
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
        use crate::strategy::profit_calculator::ProfitCalculator;
        use crate::core::types::Opportunity;
        
        if position.debt_assets.is_empty() || position.collateral_assets.is_empty() {
            return None;
        }

        let profit_calc = ProfitCalculator::new(config.clone());
        let mut best_profit = f64::NEG_INFINITY;
        let mut best_pair: Option<(solana_sdk::pubkey::Pubkey, solana_sdk::pubkey::Pubkey)> = None;

        // Optimize: Sadece en büyük debt ve collateral'ı kontrol et
        // Tüm kombinasyonları denemek yerine
        let top_debts: Vec<_> = position.debt_assets.iter()
            .filter(|d| d.amount_usd >= max_liquidatable_usd * 0.1)
            .take(2) // En fazla 2 debt asset kontrol et
            .collect();

        let top_collaterals: Vec<_> = position.collateral_assets.iter()
            .filter(|c| c.amount_usd >= seizable_collateral_usd * 0.1)
            .take(2) // En fazla 2 collateral asset kontrol et
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

                // ✅ Get hop count for accurate fee calculation
                // For pair selection, we can use a quick estimate (default to 1 hop)
                // Full hop count will be calculated in calculate_opportunity_static
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
