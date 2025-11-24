use anyhow::Result;
use tokio::sync::broadcast;
use std::sync::Arc;
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::math;
use crate::performance::PerformanceTracker;

/// Analyzer worker - AccountUpdated event'lerini alır, HF kontrolü yapar
pub async fn run_analyzer(
    mut receiver: broadcast::Receiver<Event>,
    bus: EventBus,
    config: Config,
    performance_tracker: Arc<PerformanceTracker>,
) -> Result<()> {
    loop {
        match receiver.recv().await {
            Ok(Event::AccountUpdated(position)) => {
                // Health Factor kontrolü
                if position.health_factor < config.hf_liquidation_threshold {
                    // Potansiyel likidasyon fırsatı hesapla
                    if let Some(opportunity) = math::calculate_liquidation_opportunity(&position, &config).await? {
                        // Opportunity ID oluştur (account + timestamp için unique)
                        let opportunity_id = format!("{}_{}", position.account_address, 
                            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default().as_millis());
                        
                        // Performance tracking - opportunity detection zamanını kaydet
                        performance_tracker.record_opportunity_detection(opportunity_id.clone()).await;
                        
                        log::info!(
                            "Potentially liquidatable position found: {} (HF: {:.4})",
                            position.account_address,
                            position.health_factor
                        );
                        bus.publish(Event::PotentiallyLiquidatable(opportunity))?;
                    }
                }
            }
            Ok(_) => {
                // Diğer event'leri ignore et
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

