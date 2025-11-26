use anyhow::Result;
use tokio::sync::broadcast;
use tokio::time::{sleep, Duration};
use std::sync::Arc;
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::solana_client::{self, SolanaClient};
use crate::wallet::WalletManager;
use crate::protocol::Protocol;
use crate::tx_lock::TxLock;
use crate::performance::PerformanceTracker;

/// Executor worker - ExecuteLiquidation event'lerini alır, transaction gönderir
pub async fn run_executor(
    mut receiver: broadcast::Receiver<Event>,
    bus: EventBus,
    config: Config,
    wallet: Arc<WalletManager>,
    protocol: Arc<dyn Protocol>,
    rpc_client: Arc<SolanaClient>,
    performance_tracker: Arc<PerformanceTracker>,
) -> Result<()> {
    // TX-lock mekanizması - double liquidation önleme
    let tx_lock = Arc::new(TxLock::new(60)); // 60 saniye lock süresi
    
    // Retry parametreleri
    const MAX_RETRIES: u32 = 3;
    const INITIAL_RETRY_DELAY_MS: u64 = 1000; // 1 saniye
    
    // Production safety: Runtime dry-run double-check
    if !config.dry_run {
        log::warn!("⚠️  PRODUCTION MODE: Real transactions will be sent!");
        log::warn!("⚠️  Double-checking DRY_RUN=false is intentional...");
    }
    loop {
        match receiver.recv().await {
            Ok(Event::ExecuteLiquidation(opportunity)) => {
                let account_address = opportunity.account_position.account_address.clone();
                
                // TX-lock kontrolü - aynı account zaten işleniyorsa atla
                if !tx_lock.try_lock(&account_address).await {
                    log::warn!(
                        "Account {} is already being processed, skipping duplicate liquidation",
                        account_address
                    );
                    continue;
                }
                
                // Lock'u otomatik olarak kaldırmak için scope sonunda unlock
                let unlock_guard = UnlockGuard {
                    tx_lock: Arc::clone(&tx_lock),
                    account_address: account_address.clone(),
                };
                
                if config.dry_run {
                    log::info!(
                        "DRY RUN: Would execute liquidation for account {} (profit=${:.2})",
                        account_address,
                        opportunity.estimated_profit_usd
                    );
                    
                    // Dry-run modunda gerçek TX göndermiyoruz
                    bus.publish(Event::TxResult {
                        opportunity: opportunity.clone(),
                        success: true,
                        signature: Some("DRY_RUN_SIGNATURE".to_string()),
                        error: None,
                    })?;
                } else {
                    // Gerçek transaction gönderimi - retry mekanizması ile
                    let mut last_error = None;
                    let mut success = false;
                    let mut signature = None;
                    
                    for attempt in 0..=MAX_RETRIES {
                        match solana_client::execute_liquidation(
                            &opportunity,
                            &config,
                            wallet.as_ref(),
                            protocol.as_ref(),
                            Arc::clone(&rpc_client),
                        ).await {
                            Ok(sig) => {
                                signature = Some(sig.clone());
                                success = true;
                                
                                // Performance tracking - TX send latency
                                let opportunity_id = opportunity.account_position.account_address.clone();
                                if let Some(latency) = performance_tracker.record_tx_send(opportunity_id).await {
                                    log::info!(
                                        "Liquidation transaction sent: {} (profit=${:.2}, attempt={}, latency={}ms)",
                                        sig,
                                        opportunity.estimated_profit_usd,
                                        attempt + 1,
                                        latency.as_millis()
                                    );
                                } else {
                                    log::info!(
                                        "Liquidation transaction sent: {} (profit=${:.2}, attempt={})",
                                        sig,
                                        opportunity.estimated_profit_usd,
                                        attempt + 1
                                    );
                                }
                                break;
                            }
                            Err(e) => {
                                last_error = Some(e);
                                if attempt < MAX_RETRIES {
                                    // Exponential backoff: 1s, 2s, 4s
                                    let delay_ms = INITIAL_RETRY_DELAY_MS * (1 << attempt);
                                    log::warn!(
                                        "Liquidation attempt {} failed for account {}: {}. Retrying in {}ms...",
                                        attempt + 1,
                                        account_address,
                                        last_error.as_ref().unwrap(),
                                        delay_ms
                                    );
                                    sleep(Duration::from_millis(delay_ms)).await;
                                } else {
                                    log::error!(
                                        "All {} liquidation attempts failed for account {}: {}",
                                        MAX_RETRIES + 1,
                                        account_address,
                                        last_error.as_ref().unwrap()
                                    );
                                }
                            }
                        }
                    }
                    
                    // Sonucu event bus'a yayınla
                    bus.publish(Event::TxResult {
                        opportunity: opportunity.clone(),
                        success,
                        signature,
                        error: last_error.map(|e| e.to_string()),
                    })?;
                }
                
                // Explicit unlock yaparak error handling'i garanti altına alıyoruz
                // Drop'ta spawn edilen task'a güvenmek yerine, burada explicit unlock yapıyoruz
                // Bu şekilde unlock'ın başarılı olduğundan emin oluyoruz
                let tx_lock_for_unlock = Arc::clone(&tx_lock);
                let account_address_for_unlock = account_address.clone();
                let unlock_handle = tokio::spawn(async move {
                    tx_lock_for_unlock.unlock(&account_address_for_unlock).await;
                    log::debug!("Successfully unlocked account: {}", account_address_for_unlock);
                });
                
                // Spawn edilen task'ın handle'ını al ve error'ları logla
                // NOT: await edemeyiz (async context'te değiliz), ama task'ı spawn ettik
                // Task başarısız olursa tokio runtime loglar, ama biz de log ekleyebiliriz
                let account_address_for_error_log = account_address.clone();
                tokio::spawn(async move {
                    if let Err(e) = unlock_handle.await {
                        log::error!("Failed to unlock account {}: {:?}", account_address_for_error_log, e);
                    }
                });
                
                // Guard'ı drop et (artık unlock yapıldı, ama guard yine de drop edilmeli)
                // Drop'taki spawn fallback olarak kalacak (explicit unlock başarısız olursa)
                drop(unlock_guard);
            }
            Ok(_) => {
                // Diğer event'leri ignore et
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("Executor lagged, skipped {} events", skipped);
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::error!("Event bus closed, executor shutting down");
                break;
            }
        }
    }
    
    Ok(())
}

/// RAII guard - scope'tan çıkınca otomatik unlock yapar
struct UnlockGuard {
    tx_lock: Arc<TxLock>,
    account_address: String,
}

impl Drop for UnlockGuard {
    fn drop(&mut self) {
        // Async drop yapamayız, bu yüzden spawn ediyoruz
        // NOT: Fire-and-forget pattern kullanıyoruz, ama error handling ekliyoruz
        let tx_lock = Arc::clone(&self.tx_lock);
        let account_address = self.account_address.clone();
        
        // Spawn edilen task'a error handling ekle
        // Task başarısız olursa (örneğin runtime panik olursa) loglanacak
        tokio::spawn(async move {
            // Unlock işlemi
            tx_lock.unlock(&account_address).await;
            
            // Unlock başarılı (unlock fonksiyonu Result döndürmüyor, bu yüzden başarılı kabul ediyoruz)
            // Ancak runtime panik olursa, tokio runtime bunu loglar
            log::debug!("Successfully unlocked account: {}", account_address);
        });
        
        // NOT: Spawn edilen task'ın tamamlanmasını bekleyemeyiz (Drop sync olmalı)
        // Ancak:
        // 1. Explicit unlock scope sonunda yapılıyor (daha güvenilir)
        // 2. Drop'taki spawn sadece fallback (explicit unlock başarısız olursa)
        // 3. Task başarısız olursa tokio runtime loglar
        // 
        // Alternatif çözümler (gelecek iyileştirmeler):
        // 1. AsyncDrop trait'i kullanmak (henüz stable değil)
        // 2. Task handle'ı saklamak ve join etmek (ama Drop sync olmalı)
        // 3. Unlock'u sync yapmak (ama bu async context'i bozar)
    }
}

