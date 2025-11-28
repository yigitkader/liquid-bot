use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::health::HealthManager;
use crate::protocol::Protocol;
use crate::solana_client::SolanaClient;
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub async fn run_rpc_poller(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let program_id = protocol.program_id();

    // ⚠️ RATE LIMITING UYARISI
    // getProgramAccounts çok ağır bir RPC çağrısıdır ve ücretsiz RPC'ler bunu sınırlar
    // 
    // Implementation Status: ✅ COMPLETE
    // - Exponential backoff with jitter for 429 errors (implemented in solana_client.rs)
    // - Configurable polling interval (POLL_INTERVAL_MS)
    // - Configurable max consecutive errors (MAX_CONSECUTIVE_ERRORS)
    // - Rate limiter for RPC calls (RateLimiter in solana_client.rs)
    // 
    // Önerilen çözümler:
    // 1. Polling interval'ı artır: POLL_INTERVAL_MS=10000 (10 saniye) - ücretsiz RPC için
    // 2. Premium RPC kullan (Helius, Triton) - rate limit yok, getProgramAccounts destekli
    // 3. WebSocket kullan (önerilir) - account subscription, real-time updates, rate limit yok
    
    let is_free_rpc = config.is_free_rpc_endpoint();
    let is_premium_rpc = config.is_premium_rpc_endpoint();
    
    if is_free_rpc && poll_interval.as_secs() < 10 {
        log::error!(
            "🚨 OPERASYONEL RİSK: Free RPC endpoint + kısa polling interval!"
        );
        log::error!(
            "   RPC: {} (ücretsiz endpoint)",
            config.rpc_http_url
        );
        log::error!(
            "   Polling interval: {}ms (önerilen: 10000ms minimum)",
            config.poll_interval_ms
        );
        log::error!(
            "   Free RPC'ler getProgramAccounts için 1 req/10s limit koyar!"
        );
        log::error!("");
        log::error!("   Bu konfigürasyon rate limit hatalarına yol açacaktır!");
        log::error!("   Exponential backoff var ama production'da sorun çıkarabilir.");
        log::error!("");
                log::error!("   ÇÖZÜM:");
                log::error!("   1. POLL_INTERVAL_MS=10000 (10 saniye) ayarlayın");
                log::error!("   2. WebSocket bağlantısını düzeltin (varsayılan, önerilen)");
                log::error!("   3. VEYA Premium RPC kullanın (Helius, Triton, QuickNode)");
        log::error!("");
    } else if poll_interval.as_secs() < 10 && !is_premium_rpc {
        log::warn!(
            "⚠️  Polling interval {}ms is too short for getProgramAccounts!",
            config.poll_interval_ms
        );
        log::warn!(
            "⚠️  Recommended: POLL_INTERVAL_MS=10000 (10s) minimum for RPC polling fallback"
        );
    }

    log::info!(
        "Starting RPC poller for protocol: {} (program: {}), poll_interval: {}ms",
        protocol.id(),
        program_id,
        config.poll_interval_ms
    );

    let mut consecutive_errors = 0;
    let max_consecutive_errors = config.max_consecutive_errors;

    loop {
        match fetch_and_publish_positions(&bus, Arc::clone(&rpc_client), protocol.as_ref()).await {
            Ok(count) => {
                consecutive_errors = 0;
                health_manager.record_successful_poll().await;
                if count > 0 {
                    log::debug!("Polled {} positions", count);
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                let error_msg = format!("Error polling accounts: {}", e);
                log::error!(
                    "{} (attempt {}/{})",
                    error_msg,
                    consecutive_errors,
                    max_consecutive_errors
                );
                health_manager.record_error(error_msg).await;

                if consecutive_errors >= max_consecutive_errors {
                    log::error!(
                        "Too many consecutive errors ({}), stopping poller",
                        consecutive_errors
                    );
                    return Err(anyhow::anyhow!("Too many consecutive polling errors"));
                }

                let backoff_ms =
                    poll_interval.as_millis() as u64 * (1 << consecutive_errors.min(3));
                log::warn!("Backing off for {}ms before retry", backoff_ms);
                sleep(Duration::from_millis(backoff_ms)).await;
                continue;
            }
        }

        sleep(poll_interval).await;
    }
}

async fn fetch_and_publish_positions(
    bus: &EventBus,
    rpc_client: Arc<SolanaClient>,
    protocol: &dyn Protocol,
) -> Result<usize> {
    let program_id = protocol.program_id();

    // ⚠️ getProgramAccounts çok ağır bir RPC çağrısıdır
    // Ücretsiz RPC'ler genelde 1 req/10s limit koyar
    // Premium RPC (Helius, Triton) veya WebSocket kullanılması önerilir
    // Note: get_program_accounts now includes automatic retry with exponential backoff for 429 errors
    let accounts = rpc_client
        .get_program_accounts(&program_id)
        .await
        .context("Failed to fetch program accounts after retries. Note: getProgramAccounts is rate-limited on free RPC endpoints. Consider using premium RPC or WebSocket.")?;

    log::debug!(
        "Fetched {} accounts for program {}",
        accounts.len(),
        program_id
    );

    let mut position_count = 0;

    for (account_pubkey, account) in accounts {
        match protocol
            .parse_account_position(&account_pubkey, &account, Some(Arc::clone(&rpc_client)))
            .await
        {
            Ok(Some(position)) => {
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
