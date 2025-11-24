use anyhow::Result;
use tokio::sync::broadcast;
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::solana_client;

/// Executor worker - ExecuteLiquidation event'lerini alır, transaction gönderir
pub async fn run_executor(mut receiver: broadcast::Receiver<Event>, bus: EventBus, config: Config) -> Result<()> {
    loop {
        match receiver.recv().await {
            Ok(Event::ExecuteLiquidation(opportunity)) => {
                if config.dry_run {
                    log::info!(
                        "DRY RUN: Would execute liquidation for account {} (profit=${:.2})",
                        opportunity.account_position.account_address,
                        opportunity.estimated_profit_usd
                    );
                    
                    // Dry-run modunda gerçek TX göndermiyoruz
                    bus.publish(Event::TxResult {
                        opportunity: opportunity.clone(),
                        success: true,
                        signature: Some("DRY_RUN_SIGNATURE".to_string()),
                        error: None,
                    })?;
                } else {
                    // Gerçek transaction gönderimi
                    match solana_client::execute_liquidation(&opportunity, &config).await {
                        Ok(signature) => {
                            log::info!(
                                "Liquidation transaction sent: {} (profit=${:.2})",
                                signature,
                                opportunity.estimated_profit_usd
                            );
                            
                            bus.publish(Event::TxResult {
                                opportunity: opportunity.clone(),
                                success: true,
                                signature: Some(signature),
                                error: None,
                            })?;
                        }
                        Err(e) => {
                            log::error!(
                                "Failed to execute liquidation: {}",
                                e
                            );
                            
                            bus.publish(Event::TxResult {
                                opportunity: opportunity.clone(),
                                success: false,
                                signature: None,
                                error: Some(e.to_string()),
                            })?;
                        }
                    }
                }
            }
            Ok(_) => {
                // Diğer event'leri ignore et
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("Executor lagged, skipped {} events", skipped);
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::error!("Event bus closed, executor shutting down");
                break;
            }
        }
    }
    
    Ok(())
}

