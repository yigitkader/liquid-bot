/// Analyzer module - detects liquidation opportunities
/// 
/// This module is responsible for:
/// - Listening to account updates
/// - Analyzing positions for liquidation opportunities
/// - Publishing opportunities to the event bus
/// 
/// The analyzer uses a worker pool pattern to handle high-volume scenarios.

mod opportunity_calculator;
mod worker_manager;

use crate::core::config::Config;
use crate::core::events::{Event, EventBus};
use crate::protocol::Protocol;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use std::time::Instant;
use tokio::time::Duration;

use opportunity_calculator::OpportunityCalculator;
use worker_manager::WorkerManager;

pub struct Analyzer {
    event_bus: EventBus,
    protocol: Arc<dyn Protocol>,
    config: Config,
    worker_manager: WorkerManager,
    metrics: Option<Arc<crate::utils::metrics::Metrics>>,
}

impl Analyzer {
    pub fn new(event_bus: EventBus, protocol: Arc<dyn Protocol>, config: Config) -> Self {
        Analyzer {
            event_bus,
            protocol,
            worker_manager: WorkerManager::new(config.clone()),
            config,
            metrics: None,
        }
    }

    pub fn with_metrics(mut self, metrics: Arc<crate::utils::metrics::Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub async fn run(&self) -> Result<()> {
        let mut receiver = self.event_bus.subscribe();
        let mut tasks = JoinSet::new();
        let semaphore = self.worker_manager.create_semaphore();

        let mut last_lag = Instant::now();
        let mut last_scale_down = Instant::now();
        let mut last_cleanup = Instant::now();
        const CLEANUP_INTERVAL: Duration = Duration::from_millis(100);
        const MAX_CONCURRENT_TASKS: usize = 1000;

        'main_loop: loop {
            tokio::select! {
                event_result = receiver.recv() => {
                    match event_result {
                        Ok(Event::AccountUpdated { position, .. })
                        | Ok(Event::AccountDiscovered { position, .. }) => {
                            // Check task limit before spawning
                            if tasks.len() >= MAX_CONCURRENT_TASKS {
                                log::warn!(
                                    "Analyzer: Task queue full ({} tasks), dropping event for position {}",
                                    tasks.len(),
                                    position.address
                                );
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

                                    // Check if liquidatable and calculate opportunity
                                    if OpportunityCalculator::is_liquidatable(&position, &config) {
                                        if let Some(opportunity) =
                                            OpportunityCalculator::calculate_opportunity(
                                                position.clone(),
                                                protocol,
                                                config,
                                            ).await
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
                            self.worker_manager.handle_lag(&semaphore, skipped as usize);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            log::error!("Event bus closed, analyzer shutting down");
                            break 'main_loop;
                        }
                    }
                }
                // Periodic cleanup - remove completed tasks even when no events arrive
                _ = tokio::time::sleep(CLEANUP_INTERVAL) => {
                    if last_cleanup.elapsed() >= CLEANUP_INTERVAL {
                        WorkerManager::cleanup_tasks(&mut tasks);
                        last_cleanup = Instant::now();
                    }
                }
            }

            // Also cleanup after processing each event (non-blocking)
            WorkerManager::cleanup_tasks(&mut tasks);

            // Try to scale down workers if no lag
            self.worker_manager.try_scale_down(&semaphore, last_lag, &mut last_scale_down);
        }

        Ok(())
    }
}

