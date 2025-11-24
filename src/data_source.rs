use anyhow::Result;
use crate::config::Config;
use crate::event_bus::EventBus;
use crate::rpc_poller;

/// Data source kontrol katmanı - WS veya RPC seçimi burada yapılır
pub async fn run_data_source(bus: EventBus, config: Config) -> Result<()> {
    // İlk versiyonda RPC polling ile başlayalım
    // İleride config'ten seçim yapılabilir
    rpc_poller::run_rpc_poller(bus, config).await
}

