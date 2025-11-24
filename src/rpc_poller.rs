use anyhow::{Context, Result};
use tokio::time::{sleep, Duration};
use std::sync::Arc;
use crate::config::Config;
use crate::event_bus::EventBus;
use crate::event::Event;
use crate::solana_client::SolanaClient;
use crate::protocol::Protocol;
use crate::health::HealthManager;

/// RPC polling ile account'ları tarar ve güncellemeleri event bus'a gönderir
pub async fn run_rpc_poller(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let program_id = protocol.program_id();
    
    log::info!("Starting RPC poller for protocol: {} (program: {})", protocol.id(), program_id);
    
    let mut consecutive_errors = 0;
    const MAX_CONSECUTIVE_ERRORS: u32 = 10;
    
    loop {
        match fetch_and_publish_positions(&bus, &rpc_client, protocol.as_ref()).await {
            Ok(count) => {
                consecutive_errors = 0; // Reset error counter on success
                health_manager.record_successful_poll().await;
                if count > 0 {
                    log::debug!("Polled {} positions", count);
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                let error_msg = format!("Error polling accounts: {}", e);
                log::error!("{} (attempt {}/{})", error_msg, consecutive_errors, MAX_CONSECUTIVE_ERRORS);
                health_manager.record_error(error_msg).await;
                
                // Çok fazla ardışık hata varsa durdur (production safety)
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    log::error!("Too many consecutive errors ({}), stopping poller", consecutive_errors);
                    return Err(anyhow::anyhow!("Too many consecutive polling errors"));
                }
                
                // Exponential backoff on error
                let backoff_ms = poll_interval.as_millis() as u64 * (1 << consecutive_errors.min(3));
                log::warn!("Backing off for {}ms before retry", backoff_ms);
                sleep(Duration::from_millis(backoff_ms)).await;
                continue;
            }
        }
        
        sleep(poll_interval).await;
    }
}

/// Program account'larını çeker, parse eder ve event bus'a yayınlar
async fn fetch_and_publish_positions(
    bus: &EventBus,
    rpc_client: &SolanaClient,
    protocol: &dyn Protocol,
) -> Result<usize> {
    let program_id = protocol.program_id();
    
    // getProgramAccounts ile tüm program account'larını çek
    let accounts = rpc_client.get_program_accounts(&program_id).await
        .context("Failed to fetch program accounts")?;
    
    log::debug!("Fetched {} accounts for program {}", accounts.len(), program_id);
    
    let mut position_count = 0;
    
    // Her account'u parse et
    for (account_pubkey, account) in accounts {
        // Protocol trait'i ile account'u parse et
        match protocol.parse_account_position(&account_pubkey, &account).await {
            Ok(Some(position)) => {
                // AccountPosition başarıyla parse edildi, event yayınla
                bus.publish(Event::AccountUpdated(position))
                    .map_err(|e| anyhow::anyhow!("Failed to publish event: {}", e))?;
                position_count += 1;
            }
            Ok(None) => {
                // Bu account bir position değil (örneğin market account, reserve account vb.)
                // Ignore et
            }
            Err(e) => {
                log::warn!("Failed to parse account {}: {}", account_pubkey, e);
                // Parse hatası durumunda devam et
            }
        }
    }
    
    Ok(position_count)
}

