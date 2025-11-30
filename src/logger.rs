use crate::event::Event;
use crate::health::HealthManager;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH, Duration};
use std::fs::OpenOptions;
use std::io::Write;
use tokio::sync::broadcast;
use tokio::time::interval;

pub async fn run_logger(
    mut receiver: broadcast::Receiver<Event>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    let mut metrics = Metrics::new();
    let dry_run = read_dry_run_flag();
    
    // Periyodik özet log için timer (her 5 dakikada bir)
    let mut summary_interval = interval(Duration::from_secs(300)); // 5 dakika
    let mut last_summary_time = SystemTime::now();

    loop {
        tokio::select! {
            // Event'leri dinle
            event_result = receiver.recv() => {
                match event_result {
                    Ok(event) => match &event {
                        Event::AccountUpdated(position) => {
                            metrics.accounts_scanned += 1;
                            
                            // Health factor'a göre daha anlaşılır log
                            if position.health_factor < 1.0 {
                                metrics.liquidatable_positions += 1;
                                log::info!(
                                    "🔍 POZİSYON BULUNDU: account={}, health_factor={:.4} (LİQUIDATABLE!)",
                                    position.account_address,
                                    position.health_factor
                                );
                                log::info!(
                                    "   💰 Collateral: ${:.2}, Debt: ${:.2}",
                                    position.total_collateral_usd,
                                    position.total_debt_usd
                                );
                            } else {
                                log::debug!(
                                    "Pozisyon tarandı: account={}, HF={:.4} (sağlıklı, liquidatable değil)",
                                    position.account_address,
                                    position.health_factor
                                );
                            }
                        }
                Event::PriceUpdate { mint, price, confidence, timestamp } => {
                    log::debug!(
                        "Price update: mint={}, price=${:.4}, confidence=${:.4}, timestamp={}",
                        mint, price, confidence, timestamp
                    );
                }
                Event::SlotUpdate { slot } => {
                    log::debug!("Slot update: slot={}", slot);
                }
                Event::AccountCheckRequest { account_address, reason } => {
                    log::debug!(
                        "Account check requested: {} (reason: {})",
                        account_address, reason
                    );
                }
                Event::PotentiallyLiquidatable(opp) => {
                    metrics.opportunities_found += 1;
                    health_manager.record_opportunity().await;
                    log::info!(
                        "✅ FIRSAT BULUNDU! account={}, tahmini_kâr=${:.2}",
                        opp.account_position.account_address,
                        opp.estimated_profit_usd
                    );
                    log::info!(
                        "   📊 Detaylar: max_liquidatable={}, seizable_collateral={}, liquidation_bonus={:.2}%",
                        opp.max_liquidatable_amount,
                        opp.seizable_collateral,
                        opp.liquidation_bonus * 100.0
                    );
                }
                Event::ExecuteLiquidation(opp) => {
                    metrics.tx_sent += 1;
                    let mode_str = if dry_run { "DRY RUN" } else { "GERÇEK İŞLEM" };
                    log::info!(
                        "🚀 {} BAŞLATILIYOR: account={}, tahmini_kâr=${:.2}",
                        mode_str,
                        opp.account_position.account_address,
                        opp.estimated_profit_usd
                    );
                }
                Event::TxResult {
                    opportunity,
                    success,
                    signature,
                    error,
                } => {
                    health_manager.record_transaction(*success).await;
                    if *success {
                        metrics.tx_success += 1;
                        metrics.total_profit_usd += opportunity.estimated_profit_usd;
                        let mode_str = if dry_run { "DRY RUN" } else { "GERÇEK İŞLEM" };
                        log::info!(
                            "✅ {} BAŞARILI! sig={:?}, kâr=${:.2}",
                            mode_str,
                            signature,
                            opportunity.estimated_profit_usd
                        );
                        log::info!(
                            "   📈 Toplam kâr (bu oturum): ${:.2} ({} işlem)",
                            metrics.total_profit_usd,
                            metrics.tx_success
                        );

                        // Append PnL entry to a persistent CSV log so that we can
                        // track realized PnL across multiple runs without changing
                        // the core liquidation logic.
                        append_pnl_log(
                            dry_run,
                            &opportunity.account_position.account_address,
                            opportunity.estimated_profit_usd,
                            signature,
                            &metrics,
                        );
                    } else {
                        log::error!(
                            "❌ İŞLEM BAŞARISIZ: account={}, hata={:?}",
                            opportunity.account_position.account_address,
                            error
                        );
                    }

                    if metrics.tx_success % 10 == 0 {
                        log::info!(
                                "📊 Özet (her 10 işlemde bir): fırsat={}, gönderilen={}, başarılı={}, toplam_kâr=${:.2}",
                                metrics.opportunities_found,
                                metrics.tx_sent,
                                metrics.tx_success,
                                metrics.total_profit_usd
                            );
                    }
                }
                    },
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        log::warn!("⚠️  Logger lagged, {} event atlandı", skipped);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        log::error!("❌ Event bus kapandı, logger kapanıyor");
                        break;
                    }
                }
            }
            // Periyodik özet log (her 5 dakikada bir)
            _ = summary_interval.tick() => {
                let elapsed = last_summary_time.elapsed().unwrap_or_default();
                let elapsed_min = elapsed.as_secs() / 60;
                
                log::info!("");
                log::info!("═══════════════════════════════════════════════════════════");
                log::info!("📊 BOT DURUM RAPORU (Son {} dakika)", elapsed_min);
                log::info!("═══════════════════════════════════════════════════════════");
                log::info!("   🔍 Taranan pozisyonlar: {}", metrics.accounts_scanned);
                log::info!("   ⚠️  Liquidatable pozisyonlar: {}", metrics.liquidatable_positions);
                log::info!("   ✅ Bulunan fırsatlar: {}", metrics.opportunities_found);
                log::info!("   🚀 Gönderilen işlemler: {}", metrics.tx_sent);
                log::info!("   ✅ Başarılı işlemler: {}", metrics.tx_success);
                log::info!("   💰 Toplam kâr (bu oturum): ${:.2}", metrics.total_profit_usd);
                log::info!("   📝 Mod: {}", if dry_run { "DRY RUN (test modu)" } else { "GERÇEK İŞLEM" });
                log::info!("═══════════════════════════════════════════════════════════");
                log::info!("");
                
                last_summary_time = SystemTime::now();
            }
        }
    }

    Ok(())
}

struct Metrics {
    accounts_scanned: u64,
    liquidatable_positions: u64,
    opportunities_found: u64,
    tx_sent: u64,
    tx_success: u64,
    total_profit_usd: f64,
}

impl Metrics {
    fn new() -> Self {
        Metrics {
            accounts_scanned: 0,
            liquidatable_positions: 0,
            opportunities_found: 0,
            tx_sent: 0,
            tx_success: 0,
            total_profit_usd: 0.0,
        }
    }
}

/// Read DRY_RUN flag from environment so we can tag PnL entries as
/// either DRY_RUN or REAL without plumbing Config into the logger.
fn read_dry_run_flag() -> bool {
    std::env::var("DRY_RUN")
        .ok()
        .and_then(|s| s.parse::<bool>().ok())
        .unwrap_or(true)
}

/// Append a single PnL entry to `logs/pnl_history.csv`.
///
/// Format (CSV):
/// timestamp_unix,mode,profit_usd,cumulative_session_profit,tx_success_count,account,signature
fn append_pnl_log(
    dry_run: bool,
    account: &str,
    profit_usd: f64,
    signature: &Option<String>,
    metrics: &Metrics,
) {
    // Best‑effort: PnL logging MUST NOT break the bot if disk I/O fails.
    if let Err(e) = std::fs::create_dir_all("logs") {
        log::warn!("Failed to create logs directory for PnL logging: {}", e);
        return;
    }

    let path = "logs/pnl_history.csv";
    let existed = Path::new(path).exists();

    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            log::warn!("Failed to open PnL log file {}: {}", path, e);
            return;
        }
    };

    // Write header once for new files to make later analysis easier.
    if !existed {
        let _ = writeln!(
            file,
            "timestamp_unix,mode,profit_usd,cumulative_session_profit,tx_success_count,account,signature"
        );
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mode = if dry_run { "DRY_RUN" } else { "REAL" };
    let sig_str = signature.as_deref().unwrap_or("");

    // Account ve signature base58 olduğu için virgül beklemiyoruz; bu yüzden
    // ekstra CSV escaping'e gerek yok.
    let _ = writeln!(
        file,
        "{},{},{:.6},{:.6},{},\"{}\",\"{}\"",
        ts,
        mode,
        profit_usd,
        metrics.total_profit_usd,
        metrics.tx_success,
        account,
        sig_str,
    );
}
