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
        let mut receiver = self.event_bus.subscribe();
        let mut tasks = JoinSet::new();
        
        // Dynamic semaphore that can be adjusted based on lag
        // Use Arc<Mutex<Arc<Semaphore>>> to allow dynamic resizing
        let semaphore = Arc::new(tokio::sync::Mutex::new(Arc::new(
            tokio::sync::Semaphore::new(self.current_workers.load(Ordering::Relaxed))
        )));

        loop {
            match receiver.recv().await {
                Ok(Event::AccountUpdated { position, .. })
                | Ok(Event::AccountDiscovered { position, .. }) => {
                    // BACKPRESSURE: Wait for available worker slot before processing
                    // This prevents buffer overflow by controlling the rate of event processing
                    let semaphore_guard = semaphore.lock().await;
                    let permit = match semaphore_guard.clone().acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => {
                            log::warn!("Semaphore closed, analyzer shutting down");
                            break;
                        }
                    };
                    drop(semaphore_guard); // Release lock before spawning task
                    
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
                    // ADAPTIVE SCALING: Increase workers when lag detected
                    let current = self.current_workers.load(Ordering::Relaxed);
                    if current < self.max_workers_limit {
                        // Increase workers by 50% or at least 2, up to limit
                        let increase = std::cmp::max(2, current / 2);
                        let new_workers = std::cmp::min(
                            current + increase,
                            self.max_workers_limit
                        );
                        
                        self.current_workers.store(new_workers, Ordering::Relaxed);
                        
                        // Recreate semaphore with new capacity
                        let mut semaphore_guard = semaphore.lock().await;
                        *semaphore_guard = Arc::new(tokio::sync::Semaphore::new(new_workers));
                        drop(semaphore_guard);
                        
                        log::warn!(
                            "⚠️  Analyzer lagged, skipped {} events. Increasing workers: {} -> {}",
                            skipped,
                            current,
                            new_workers
                        );
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
        }

        Ok(())
    }

    fn is_liquidatable_static(position: &Position, config: &Config) -> bool {
        position.health_factor < config.hf_liquidation_threshold
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
        
        let net_profit = profit_calc.calculate_net_profit(&temp_opp);

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

                let net_profit = profit_calc.calculate_net_profit(&temp_opp);

                if net_profit > best_profit {
                    best_profit = net_profit;
                    best_pair = Some((debt_asset.mint, collateral_asset.mint));
                }
            }
        }

        best_pair
    }
}
