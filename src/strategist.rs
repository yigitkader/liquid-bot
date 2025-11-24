use anyhow::Result;
use tokio::sync::broadcast;
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;

/// Strategist worker - PotentiallyLiquidatable event'lerini iş kurallarına göre filtreler
pub async fn run_strategist(mut receiver: broadcast::Receiver<Event>, bus: EventBus, config: Config) -> Result<()> {
    loop {
        match receiver.recv().await {
            Ok(Event::PotentiallyLiquidatable(opportunity)) => {
                // İş kuralları kontrolü
                if opportunity.estimated_profit_usd >= config.min_profit_usd {
                    // Slippage kontrolü (ileride eklenecek)
                    // Sermaye kontrolü (ileride eklenecek)
                    
                    log::info!(
                        "Liquidation opportunity approved: profit=${:.2}, account={}",
                        opportunity.estimated_profit_usd,
                        opportunity.account_position.account_address
                    );
                    
                    bus.publish(Event::ExecuteLiquidation(opportunity))?;
                } else {
                    log::debug!(
                        "Liquidation opportunity rejected: profit=${:.2} < min=${:.2}",
                        opportunity.estimated_profit_usd,
                        config.min_profit_usd
                    );
                }
            }
            Ok(_) => {
                // Diğer event'leri ignore et
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("Strategist lagged, skipped {} events", skipped);
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::error!("Event bus closed, strategist shutting down");
                break;
            }
        }
    }
    
    Ok(())
}

