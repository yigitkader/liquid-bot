use crate::event::Event;
use crate::health::HealthManager;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;

pub async fn run_logger(
    mut receiver: broadcast::Receiver<Event>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    let mut metrics = Metrics::new();

    loop {
        match receiver.recv().await {
            Ok(event) => match &event {
                Event::AccountUpdated(position) => {
                    log::debug!(
                        "Account updated: {} (HF: {:.4})",
                        position.account_address,
                        position.health_factor
                    );
                }
                Event::PotentiallyLiquidatable(opp) => {
                    metrics.opportunities_found += 1;
                    health_manager.record_opportunity().await;
                    log::info!(
                        "Opportunity found: account={}, profit=${:.2}",
                        opp.account_position.account_address,
                        opp.estimated_profit_usd
                    );
                }
                Event::ExecuteLiquidation(opp) => {
                    metrics.tx_sent += 1;
                    log::info!(
                        "Executing liquidation: account={}, profit=${:.2}",
                        opp.account_position.account_address,
                        opp.estimated_profit_usd
                    );
                }
                Event::TxResult {
                    opportunity,
                    success,
                    signature,
                    error,
                } => {
                    health_manager.record_transaction(*success).await;
                    if *success {
                        metrics.tx_success += 1;
                        metrics.total_profit_usd += opportunity.estimated_profit_usd;
                        log::info!(
                            "Transaction successful: sig={:?}, profit=${:.2}",
                            signature,
                            opportunity.estimated_profit_usd
                        );
                    } else {
                        log::error!(
                            "Transaction failed: account={}, error={:?}",
                            opportunity.account_position.account_address,
                            error
                        );
                    }

                    if metrics.tx_success % 10 == 0 {
                        log::info!(
                                "Metrics: opportunities={}, tx_sent={}, tx_success={}, total_profit=${:.2}",
                                metrics.opportunities_found,
                                metrics.tx_sent,
                                metrics.tx_success,
                                metrics.total_profit_usd
                            );
                    }
                }
            },
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("Logger lagged, skipped {} events", skipped);
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::error!("Event bus closed, logger shutting down");
                break;
            }
        }
    }

    Ok(())
}

struct Metrics {
    opportunities_found: u64,
    tx_sent: u64,
    tx_success: u64,
    total_profit_usd: f64,
}

impl Metrics {
    fn new() -> Self {
        Metrics {
            opportunities_found: 0,
            tx_sent: 0,
            tx_success: 0,
            total_profit_usd: 0.0,
        }
    }
}
