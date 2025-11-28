use crate::config::Config;
use crate::event_bus::EventBus;
use crate::health::HealthManager;
use crate::protocol::Protocol;
use crate::rpc_poller;
use crate::solana_client::SolanaClient;
use crate::ws_listener;
use anyhow::Result;
use std::sync::Arc;

pub async fn run_data_source(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    // Data source selection: RPC polling or WebSocket
    if config.use_websocket {
        log::info!("📡 Using WebSocket as data source (real-time updates, no rate limits)");
        log::info!("   WebSocket URL: {}", config.rpc_ws_url);
        ws_listener::run_ws_listener(bus, config, rpc_client, protocol, health_manager).await
    } else {
        log::info!("📡 Using RPC polling as data source");
        log::info!("   Poll interval: {}ms", config.poll_interval_ms);
        log::info!("   ⚠️  Note: For production, consider using WebSocket (USE_WEBSOCKET=true)");
        log::info!("   ⚠️  WebSocket provides real-time updates (<100ms latency) and no rate limits");
        rpc_poller::run_rpc_poller(bus, config, rpc_client, protocol, health_manager).await
    }
}
