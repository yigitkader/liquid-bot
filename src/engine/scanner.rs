use crate::core::events::{Event, EventBus};
use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::WsClient;
use crate::protocol::Protocol;
use crate::utils::cache::AccountCache;
use anyhow::Result;
use std::sync::Arc;

pub struct Scanner {
    rpc: Arc<RpcClient>,
    ws: Arc<WsClient>,
    protocol: Arc<dyn Protocol>,
    event_bus: EventBus,
    cache: Arc<AccountCache>,
}

impl Scanner {
    pub fn new(
        rpc: Arc<RpcClient>,
        ws: Arc<WsClient>,
        protocol: Arc<dyn Protocol>,
        event_bus: EventBus,
        cache: Arc<AccountCache>,
    ) -> Self {
        Scanner {
            rpc,
            ws,
            protocol,
            event_bus,
            cache,
        }
    }

    pub async fn discover_accounts(&self) -> Result<usize> {
        let program_id = self.protocol.program_id();
        let accounts = self.rpc.get_program_accounts(&program_id).await?;

        let mut count = 0;
        for (pubkey, account) in accounts {
            if let Some(position) = self.protocol.parse_position(&account).await {
                self.cache.insert(pubkey, position.clone()).await;
                self.event_bus.publish(Event::AccountDiscovered {
                    pubkey,
                    position,
                })?;
                count += 1;
            }
        }

        Ok(count)
    }

    pub async fn start_monitoring(&self) -> Result<()> {
        let program_id = self.protocol.program_id();
        log::info!("Starting WebSocket monitoring for program: {}", program_id);
        Ok(())
    }

    pub async fn run(&self) -> Result<()> {
        self.discover_accounts().await?;
        self.start_monitoring().await
    }
}
