use anyhow::Result;
use std::sync::Arc;
use crate::config::Config;
use crate::event_bus::EventBus;
use crate::rpc_poller;
use crate::solana_client::SolanaClient;
use crate::protocol::Protocol;
use crate::health::HealthManager;

/// Data source kontrol katmanı - WS veya RPC seçimi burada yapılır
pub async fn run_data_source(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    // İlk versiyonda RPC polling ile başlayalım
    // İleride config'ten seçim yapılabilir
    rpc_poller::run_rpc_poller(bus, config, rpc_client, protocol, health_manager).await
}

