use crate::config::Config;
use crate::event_bus::EventBus;
use crate::health::HealthManager;
use crate::protocol::Protocol;
use crate::rpc_poller;
use crate::solana_client::SolanaClient;
use anyhow::Result;
use std::sync::Arc;

pub async fn run_data_source(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    // Data source selection: Currently only RPC polling is implemented
    // Future: Add WebSocket support and config-based selection
    // For now, RPC polling is the default and only option
    rpc_poller::run_rpc_poller(bus, config, rpc_client, protocol, health_manager).await
}
