use anyhow::{Context, Result};
use dotenv::dotenv;
use fern;
use log;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use tokio::signal;
use tokio::time::{sleep, Duration};

use liquid_bot::blockchain::rpc_client::RpcClient;
use liquid_bot::blockchain::ws_client::WsClient;
use liquid_bot::core::config::Config;
use liquid_bot::core::events::EventBus;
use liquid_bot::engine::analyzer::Analyzer;
use liquid_bot::engine::executor::Executor;
use liquid_bot::engine::scanner::Scanner;
use liquid_bot::engine::validator::Validator;
use liquid_bot::protocol::solend::SolendProtocol;
use liquid_bot::protocol::Protocol;
use liquid_bot::strategy::balance_manager::BalanceManager;
use liquid_bot::utils::cache::AccountCache;
use liquid_bot::utils::metrics::Metrics;

use solana_sdk::signature::{Keypair, Signer};

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    std::fs::create_dir_all("logs").context("Failed to create logs directory")?;

    let log_file_path = format!("logs/bot-{}.log", chrono::Utc::now().format("%Y-%m-%d"));

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {}",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.level(),
                message
            ))
        })
        .level(log::LevelFilter::Debug)
        .chain(std::io::stdout())
        .chain(fern::log_file(&log_file_path)?)
        .apply()
        .context("Failed to initialize logger")?;

    log::info!("🚀 Starting Solana Liquidation Bot");
    log::info!("📝 Logging to: {}", log_file_path);

    let config = Config::from_env().context("Failed to load configuration")?;
    config
        .validate()
        .context("Configuration validation failed")?;

    log::info!("✅ Configuration loaded");

    let rpc = Arc::new(
        RpcClient::new(config.rpc_http_url.clone()).context("Failed to create RPC client")?,
    );
    log::info!("✅ RPC client initialized");

    // ✅ CRITICAL: Start reserve cache FIRST with longer delay
    // This prevents RPC storm on startup when both reserve cache and scanner call get_program_accounts
    liquid_bot::protocol::solend::instructions::start_reserve_cache_refresh(
        Arc::clone(&rpc),
        config.clone(),
    );
    log::info!("✅ Reserve cache background refresh started (30s initial delay)");

    // ✅ CRITICAL: Wait before starting scanner to prevent double RPC call
    // Scanner will call get_program_accounts() immediately, so we delay to avoid hitting rate limits
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // ✅ CRITICAL FIX: ALWAYS wait for reserve cache initialization, regardless of RPC type
    // Both Scanner and Executor need reserve cache to avoid unnecessary get_program_accounts() calls
    // Problem:
    //   - Free RPC: Was waiting ✅
    //   - Premium RPC: Was NOT waiting ❌ → Scanner immediately calls get_program_accounts() → unnecessary RPC call
    // Solution: Wait for cache initialization for BOTH RPC types before starting Scanner
    // This prevents:
    //   1. Scanner calling get_program_accounts() when cache is not ready (unnecessary RPC call)
    //   2. Executor calling get_program_accounts() on first opportunity (rate limit risk)
    log::info!(
        "⏳ Waiting for reserve cache initial population before starting Scanner and Executor..."
    );
    let cache_timeout = tokio::time::Duration::from_secs(40); // 30s delay + 10s buffer for RPC call
    if liquid_bot::protocol::solend::instructions::wait_for_reserve_cache_initialization(
        cache_timeout,
    )
    .await
    {
        log::info!("✅ Reserve cache populated, ready for Scanner and Executor");
    } else {
        log::warn!(
            "⚠️  Reserve cache initialization timeout - Scanner and Executor will start anyway"
        );
        log::warn!(
            "   This may cause unnecessary RPC calls (get_program_accounts) if cache is not ready"
        );
        log::warn!("   Monitor for rate limit errors, especially on first opportunity");
    }

    log::info!("⏳ Starting account discovery...");

    let ws = Arc::new(WsClient::new(config.rpc_ws_url.clone()));
    ws.connect().await.context("Failed to connect WebSocket")?;
    log::info!("✅ WebSocket client initialized");

    let wallet_keypair = load_wallet(&config.wallet_path).context("Failed to load wallet")?;
    let wallet = Arc::new(wallet_keypair);
    let wallet_pubkey = wallet.pubkey();
    log::info!("✅ Wallet loaded: {}", wallet_pubkey);

    // Ensure required ATAs exist (simple on-chain check, no cache needed)
    use liquid_bot::utils::ata_manager;
    ata_manager::ensure_required_atas(Arc::clone(&rpc), Arc::clone(&wallet), &config)
        .await
        .context("Failed to ensure required ATAs exist")?;
    log::info!("✅ ATA setup completed");

    let event_bus = EventBus::new(config.event_bus_buffer_size);
    log::info!("✅ Event bus initialized");

    let balance_manager = Arc::new(
        BalanceManager::new(Arc::clone(&rpc), wallet_pubkey)
            .with_config(config.clone())
            .with_websocket(Arc::clone(&ws)),
    );
    log::info!("✅ Balance manager initialized");

    // Start balance monitoring via WebSocket
    let balance_manager_monitor = Arc::clone(&balance_manager);
    tokio::spawn(async move {
        // Subscribe to ATA accounts
        if let Err(e) = balance_manager_monitor.start_monitoring().await {
            log::error!("BalanceManager: Failed to start monitoring: {}", e);
            log::warn!("BalanceManager: Will fall back to RPC-only mode");
        } else {
            // Start listening for account updates
            if let Err(e) = balance_manager_monitor.listen_account_updates().await {
                log::error!("BalanceManager: Account update listener error: {}", e);
            }
        }
    });
    log::info!("✅ Balance manager monitoring started");

    let metrics = Arc::new(Metrics::new());
    log::info!("✅ Metrics initialized");

    let cache = Arc::new(AccountCache::new());
    log::info!("✅ Account cache initialized");

    let protocol: Arc<dyn Protocol> =
        Arc::new(SolendProtocol::new(&config).context("Failed to initialize Solend protocol")?);
    log::info!(
        "✅ Protocol initialized: {} (program: {})",
        protocol.id(),
        protocol.program_id()
    );

    let scanner = Scanner::new(
        Arc::clone(&rpc),
        Arc::clone(&ws),
        Arc::clone(&protocol),
        event_bus.clone(),
        Arc::clone(&cache),
        config.clone(),
    );
    let analyzer = Analyzer::new(event_bus.clone(), Arc::clone(&protocol), config.clone());
    let validator = Validator::new(
        event_bus.clone(),
        Arc::clone(&balance_manager),
        config.clone(),
        Arc::clone(&rpc),
    );
    let executor = Arc::new(Executor::new(
        event_bus.clone(),
        Arc::clone(&rpc),
        Arc::clone(&wallet),
        Arc::clone(&protocol),
        Arc::clone(&balance_manager),
        config.clone(),
    ));

    // Keep reference to executor for graceful shutdown
    let executor_shutdown = Arc::clone(&executor);

    tokio::spawn(async move {
        if let Err(e) = scanner.run().await {
            log::error!("Scanner error: {}", e);
        }
    });
    tokio::spawn(async move {
        if let Err(e) = analyzer.run().await {
            log::error!("Analyzer error: {}", e);
        }
    });
    tokio::spawn(async move {
        if let Err(e) = validator.run().await {
            log::error!("Validator error: {}", e);
        }
    });
    let executor_run = Arc::clone(&executor);
    tokio::spawn(async move {
        if let Err(e) = executor_run.run().await {
            log::error!("Executor error: {}", e);
        }
    });

    log::info!("✅ All workers started");

    let metrics_clone = Arc::clone(&metrics);
    let balance_manager_clone = Arc::clone(&balance_manager);
    let usdc_mint_str = config.usdc_mint.clone();
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(60)).await;

            // Metrics
            let summary = metrics_clone.get_summary().await;
            log::info!(
                "📊 Metrics: Opportunities: {}, TX Sent: {}, TX Success: {}, Success Rate: {:.2}%, Total Profit: ${:.2}, Avg Latency: {}ms, P95 Latency: {}ms",
                summary.opportunities,
                summary.tx_sent,
                summary.tx_success,
                summary.success_rate * 100.0,
                summary.total_profit,
                summary.avg_latency_ms,
                summary.p95_latency_ms
            );

            // Wallet USDC balance (available, reserved-aware)
            if let Ok(usdc_mint) = solana_sdk::pubkey::Pubkey::from_str(&usdc_mint_str) {
                match balance_manager_clone
                    .get_available_balance(&usdc_mint)
                    .await
                {
                    Ok(available) => {
                        let available_usdc = available as f64 / 1_000_000.0; // USDC: 6 decimals
                        log::info!(
                            "💰 Wallet available USDC balance: {} ({} USDC)",
                            available,
                            available_usdc
                        );
                    }
                    Err(e) => {
                        log::warn!("⚠️ Failed to read available USDC balance for wallet: {}", e);
                    }
                }
            } else {
                log::warn!(
                    "⚠️ Invalid USDC mint in config, cannot log wallet USDC balance: {}",
                    usdc_mint_str
                );
            }
        }
    });

    log::info!("✅ All components initialized");
    log::info!("⏳ Waiting for shutdown signal (Ctrl+C)...");

    signal::ctrl_c()
        .await
        .context("Failed to listen for shutdown signal")?;

    log::info!("🛑 Shutting down gracefully...");

    // ✅ CRITICAL: Stop balance monitoring FIRST (clear subscriptions)
    // This prevents race conditions where account updates arrive after shutdown
    balance_manager
        .stop_monitoring()
        .await
        .context("Failed to stop balance monitoring")?;

    // Then shutdown other components...
    // Note: Executor has its own shutdown() method that cancels background tasks
    executor_shutdown.shutdown();

    log::info!("✅ Graceful shutdown completed");
    Ok(())
}

fn load_wallet(path: &str) -> Result<Keypair> {
    let wallet_path = Path::new(path);

    if !wallet_path.exists() {
        return Err(anyhow::anyhow!("Wallet file not found: {}", path));
    }

    let keypair_bytes = fs::read(wallet_path).context("Failed to read wallet file")?;

    if let Ok(keypair) = serde_json::from_slice::<Vec<u8>>(&keypair_bytes) {
        if keypair.len() == 64 {
            return Keypair::from_bytes(&keypair)
                .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
        }
    }

    if keypair_bytes.len() == 64 {
        return Keypair::from_bytes(&keypair_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
    }

    if let Ok(keypair_str) = String::from_utf8(keypair_bytes.clone()) {
        if let Ok(keypair_bytes_decoded) = bs58::decode(keypair_str.trim()).into_vec() {
            if keypair_bytes_decoded.len() == 64 {
                return Keypair::from_bytes(&keypair_bytes_decoded)
                    .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
            }
        }
    }

    Err(anyhow::anyhow!(
        "Invalid wallet format: expected 64 bytes, JSON array, or base58 string"
    ))
}
