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
    // İlk versiyonda RPC polling ile başlayalım
    // todo: İleride config'ten seçim yapılabilir
    rpc_poller::run_rpc_poller(bus, config, rpc_client, protocol, health_manager).await
}
