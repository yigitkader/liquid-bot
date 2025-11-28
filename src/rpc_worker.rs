use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::protocol::Protocol;
use crate::solana_client::SolanaClient;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::sync::broadcast;

/// RPC Worker Task
/// 
/// Event bus'tan gelen event'leri işler:
/// - AccountCheckRequest: Account'ı RPC ile oku, parse et, health factor hesapla
/// - PriceUpdate: Price cache'i güncelle (gelecekte kullanılabilir)
/// - SlotUpdate: Periodic re-check tetikle (şimdilik sadece log)
/// 
/// Bu worker RPC-bound işlemleri yapar:
/// - Account okuma (getAccountInfo)
/// - Account parsing (protocol.parse_account_position)
/// - Health factor hesaplama
/// 
/// WebSocket event'leri event bus'a yazılır, bu worker event bus'tan okur ve RPC işlemlerini yapar.
pub async fn run_rpc_worker(
    mut receiver: broadcast::Receiver<Event>,
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
) -> Result<()> {
    log::info!("✅ RPC worker started");
    
    loop {
        match receiver.recv().await {
            Ok(Event::AccountCheckRequest { account_address, reason }) => {
                match handle_account_check(
                    &account_address,
                    &reason,
                    &bus,
                    &config,
                    Arc::clone(&rpc_client),
                    protocol.as_ref(),
                )
                .await
                {
                    Ok(()) => {
                        log::debug!("Account check completed: {} (reason: {})", account_address, reason);
                    }
                    Err(e) => {
                        log::warn!("Account check failed for {}: {}", account_address, e);
                    }
                }
            }
            Ok(Event::PriceUpdate { mint, price, confidence, timestamp }) => {
                // Price cache update (gelecekte kullanılabilir)
                log::debug!("Price update received: mint={}, price=${:.4}, confidence=${:.4}, timestamp={}", mint, price, confidence, timestamp);
                // TODO: Price cache'e yaz (şimdilik sadece log)
            }
            Ok(Event::SlotUpdate { slot }) => {
                // Slot update (periodic re-check için kullanılabilir)
                log::debug!("Slot update received: slot={}", slot);
                // TODO: Periodic re-check tetikle (şimdilik sadece log)
            }
            Ok(_) => {
                // Diğer event'leri ignore et
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("RPC worker lagged, skipped {} events", skipped);
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::error!("Event bus closed, RPC worker shutting down");
                break;
            }
        }
    }
    
    log::info!("⏹️  RPC worker stopped");
    Ok(())
}

async fn handle_account_check(
    account_address: &str,
    _reason: &str,
    bus: &EventBus,
    _config: &Config,
    rpc_client: Arc<SolanaClient>,
    protocol: &dyn Protocol,
) -> Result<()> {
    let account_pubkey = account_address
        .parse::<Pubkey>()
        .context("Invalid account address")?;
    
    // RPC ile account'ı oku
    let account = rpc_client
        .get_account(&account_pubkey)
        .await
        .context("Failed to fetch account via RPC")?;
    
    // Protocol ile parse et
    match protocol
        .parse_account_position(&account_pubkey, &account, Some(Arc::clone(&rpc_client)))
        .await
    {
        Ok(Some(position)) => {
            // AccountUpdated event'ini yayınla
            bus.publish(Event::AccountUpdated(position))
                .map_err(|e| anyhow::anyhow!("Failed to publish event: {}", e))?;
        }
        Ok(None) => {
            // Bu account bir position değil (reserve, market, vs.)
            log::debug!("Account {} is not a position, skipping", account_address);
        }
        Err(e) => {
            log::warn!("Failed to parse account {}: {}", account_address, e);
        }
    }
    
    Ok(())
}

