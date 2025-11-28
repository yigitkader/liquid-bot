use crate::config::Config;
use crate::event_bus::EventBus;
use crate::health::HealthManager;
use crate::protocol::Protocol;
use crate::rpc_poller;
use crate::solana_client::SolanaClient;
use crate::ws_listener;
use anyhow::Result;
use std::sync::Arc;

/// Data source runner - WebSocket-first approach with automatic RPC polling fallback
/// 
/// Strategy:
/// 1. Always try WebSocket first (best practice - real-time, no rate limits)
/// 2. If WebSocket fails after max retries, automatically fallback to RPC polling
/// 3. This ensures best performance while maintaining reliability
pub async fn run_data_source(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    log::info!("📡 Starting data source (WebSocket-first with RPC polling fallback)");
    log::info!("   Primary: WebSocket - Real-time updates, no rate limits");
    log::info!("   Fallback: RPC polling - Used if WebSocket fails");
    log::info!("   WebSocket URL: {}", config.rpc_ws_url);
    
    // Try WebSocket first (best practice)
    let ws_result = ws_listener::run_ws_listener(
        bus.clone(),
        config.clone(),
        Arc::clone(&rpc_client),
        Arc::clone(&protocol),
        Arc::clone(&health_manager),
    )
    .await;
    
    match ws_result {
        Ok(()) => {
            // WebSocket completed normally (shouldn't happen in production)
            log::info!("WebSocket completed normally");
            Ok(())
        }
        Err(e) => {
            // WebSocket failed - fallback to RPC polling
            log::error!("❌ WebSocket failed: {}", e);
            log::warn!("");
            log::warn!("⚠️  Falling back to RPC polling...");
            log::warn!("   This is less optimal but will keep the bot running");
            log::warn!("   Consider fixing WebSocket connection for best performance");
            log::warn!("");
            
            let is_free_rpc = config.is_free_rpc_endpoint();
            
            if is_free_rpc {
                log::warn!("⚠️  OPERASYONEL RİSK: Free RPC endpoint + RPC polling fallback!");
                log::warn!("   Free RPC'ler getProgramAccounts için 1 req/10s limit koyar");
                log::warn!("   POLL_INTERVAL_MS={}ms kullanılıyor", config.poll_interval_ms);
                if config.poll_interval_ms < 10000 {
                    log::error!("   ⚠️  Polling interval çok kısa! Rate limit hataları bekleniyor.");
                    log::error!("   Önerilen: POLL_INTERVAL_MS=10000 (10s) minimum");
                }
                log::warn!("");
            }
            
            // Fallback to RPC polling
            log::info!("📡 Using RPC polling as fallback data source");
            log::info!("   Poll interval: {}ms", config.poll_interval_ms);
            log::info!("   RPC endpoint: {}", config.rpc_http_url);
            
            rpc_poller::run_rpc_poller(bus, config, rpc_client, protocol, health_manager).await
        }
    }
}
