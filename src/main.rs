// Modüller artık lib.rs'de tanımlı
use liquid_bot::*;

use anyhow::Result;
use dotenv::dotenv;
use env_logger;
use liquid_bot::protocol::Protocol;
use liquid_bot::shutdown::ShutdownManager;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use tokio;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    // Create logs directory if it doesn't exist
    let logs_dir = Path::new("logs");
    if !logs_dir.exists() {
        fs::create_dir_all(logs_dir)?;
    }

    // Generate log filename with timestamp
    let timestamp = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let log_filename = format!("logs/bot_{}.log", timestamp);
    let log_file = fs::File::create(&log_filename)?;

    // Create a writer that writes to both file and stderr
    let mut logger = env_logger::Builder::from_default_env();
    logger
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_secs()
        .format_module_path(false)
        .format_target(false);

    // Write to both file and stderr
    logger.target(env_logger::Target::Pipe(Box::new(MultiWriter::new(
        log_file,
        std::io::stderr(),
    ))));

    logger.init();

    log::info!("📝 Logging to file: {}", log_filename);

    log::info!("🚀 Starting Solana Liquidation Bot (Production Mode)");
    log::info!("Version: {}", env!("CARGO_PKG_VERSION"));

    let config = match config::Config::from_env() {
        Ok(cfg) => {
            log::info!("✅ Configuration loaded and validated");
            log::info!("   RPC: {}", cfg.rpc_http_url);
            log::info!("   Wallet: {}", cfg.wallet_path);
            log::info!("   Dry Run: {}", cfg.dry_run);
            log::info!("   HF Threshold: {}", cfg.hf_liquidation_threshold);
            log::info!("   Min Profit: ${}", cfg.min_profit_usd);
            cfg
        }
        Err(e) => {
            log::error!("❌ Configuration validation failed: {}", e);
            log::error!("Please check your .env file and configuration values");
            return Err(e);
        }
    };

    let shutdown_manager = Arc::new(ShutdownManager::new());

    let health_manager = Arc::new(health::HealthManager::new(config.health_manager_max_error_age_seconds));
    let balance_reservation = Arc::new(balance_reservation::BalanceReservation::new());

    let shutdown = shutdown_manager.clone();
    tokio::spawn(async move {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to create SIGTERM handler");
        let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt())
            .expect("Failed to create SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                log::info!("📡 Received SIGTERM, initiating graceful shutdown...");
                shutdown.shutdown();
            }
            _ = sigint.recv() => {
                log::info!("📡 Received SIGINT (Ctrl+C), initiating graceful shutdown...");
                shutdown.shutdown();
            }
        }
    });

    log::info!("🔑 Loading wallet from: {}", config.wallet_path);
    let wallet = match wallet::WalletManager::from_file(&config.wallet_path) {
        Ok(w) => {
            log::info!("✅ Wallet loaded: {}", w.pubkey());
            Arc::new(w)
        }
        Err(e) => {
            log::error!("❌ Failed to load wallet: {}", e);
            return Err(e);
        }
    };

    log::info!("🌐 Connecting to RPC: {}", config.rpc_http_url);
    let rpc_client = match solana_client::SolanaClient::new(config.rpc_http_url.clone()) {
        Ok(client) => {
            log::info!("✅ RPC client connected");
            Arc::new(client)
        }
        Err(e) => {
            log::error!("❌ Failed to connect to RPC: {}", e);
            return Err(e);
        }
    };

    let solend_protocol = match protocols::solend::SolendProtocol::new_with_config(&config) {
        Ok(proto) => {
            let protocol_id = proto.id().to_string();
            let program_id = proto.program_id();
            log::info!(
                "✅ Protocol initialized: {} (program: {})",
                protocol_id,
                program_id
            );
            Arc::new(proto) as Arc<dyn protocol::Protocol>
        }
        Err(e) => {
            log::error!("❌ Failed to initialize protocol: {}", e);
            return Err(e);
        }
    };

    let bus = event_bus::EventBus::new(config.event_bus_buffer_size);
    log::info!("✅ Event bus initialized");

    let analyzer_receiver = bus.subscribe();
    let strategist_receiver = bus.subscribe();
    let executor_receiver = bus.subscribe();
    let logger_receiver = bus.subscribe();
    let rpc_worker_receiver = bus.subscribe();
    let performance_tracker = Arc::new(performance::PerformanceTracker::new());

    log::info!("🔧 Starting worker tasks...");

    let wallet_balance_checker = Arc::new(wallet::WalletBalanceChecker::new(
        *wallet.pubkey(),
        Arc::clone(&rpc_client),
        Some(config.clone()),
    ));

    let analyzer_handle = tokio::spawn({
        let bus = bus.clone();
        let config = config.clone();
        let performance_tracker = Arc::clone(&performance_tracker);
        let protocol = Arc::clone(&solend_protocol);
        let rpc_client = Arc::clone(&rpc_client);
        async move {
            log::info!("   ✅ Analyzer worker started");
            if let Err(e) = analyzer::run_analyzer(
                analyzer_receiver,
                bus,
                config,
                performance_tracker,
                protocol,
                Some(rpc_client),
            )
            .await
            {
                log::error!("❌ Analyzer worker error: {}", e);
            }
            log::info!("   ⏹️  Analyzer worker stopped");
        }
    });

    let strategist_handle = tokio::spawn({
        let bus = bus.clone();
        let config = config.clone();
        let wallet_balance_checker = Arc::clone(&wallet_balance_checker);
        let rpc_client = Arc::clone(&rpc_client);
        let protocol = Arc::clone(&solend_protocol);
        let balance_reservation = Arc::clone(&balance_reservation);
        async move {
            log::info!("   ✅ Strategist worker started");
            if let Err(e) = strategist::run_strategist(
                strategist_receiver,
                bus,
                config,
                wallet_balance_checker,
                rpc_client,
                protocol,
                balance_reservation,
            )
            .await
            {
                log::error!("❌ Strategist worker error: {}", e);
            }
            log::info!("   ⏹️  Strategist worker stopped");
        }
    });

    let executor_handle = tokio::spawn({
        let bus = bus.clone();
        let config = config.clone();
        let wallet = Arc::clone(&wallet);
        let protocol = Arc::clone(&solend_protocol);
        let rpc_client = Arc::clone(&rpc_client);
        let performance_tracker = Arc::clone(&performance_tracker);
        let balance_reservation = Arc::clone(&balance_reservation);
        async move {
            log::info!("   ✅ Executor worker started");
            if let Err(e) = executor::run_executor(
                executor_receiver,
                bus,
                config,
                wallet,
                protocol,
                rpc_client,
                performance_tracker,
                balance_reservation,
            )
            .await
            {
                log::error!("❌ Executor worker error: {}", e);
            }
            log::info!("   ⏹️  Executor worker stopped");
        }
    });

    let logger_handle = tokio::spawn({
        let health_manager = Arc::clone(&health_manager);
        async move {
            log::info!("   ✅ Logger worker started");
            if let Err(e) = logger::run_logger(logger_receiver, health_manager).await {
                log::error!("❌ Logger worker error: {}", e);
            }
            log::info!("   ⏹️  Logger worker stopped");
        }
    });

    let rpc_worker_handle = tokio::spawn({
        let bus = bus.clone();
        let config = config.clone();
        let rpc_client = Arc::clone(&rpc_client);
        let protocol = Arc::clone(&solend_protocol);
        async move {
            log::info!("   ✅ RPC worker started");
            if let Err(e) = rpc_worker::run_rpc_worker(
                rpc_worker_receiver,
                bus,
                config,
                rpc_client,
                protocol,
            )
            .await
            {
                log::error!("❌ RPC worker error: {}", e);
            }
            log::info!("   ⏹️  RPC worker stopped");
        }
    });

    let health_check_handle = tokio::spawn({
        let health_manager = Arc::clone(&health_manager);
        let performance_tracker = Arc::clone(&performance_tracker);
        async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let is_healthy = health_manager.check_health().await;
                let status = health_manager.get_status().await;
                performance_tracker.log_metrics().await;

                if !is_healthy {
                    log::warn!(
                        "⚠️  Health check failed: consecutive_errors={}, last_error={:?}",
                        status.consecutive_errors,
                        status.last_error
                    );
                } else {
                    log::debug!(
                        "✅ Health check passed: opportunities={}, tx={}/{}",
                        status.total_opportunities,
                        status.successful_transactions,
                        status.total_transactions
                    );
                }
            }
        }
    });

    log::info!("✅ All workers started");
    log::info!("🎯 Bot is running. Press Ctrl+C to stop gracefully.");

    let data_source_handle = tokio::spawn({
        let bus = bus.clone();
        let config = config.clone();
        let rpc_client = Arc::clone(&rpc_client);
        let protocol = Arc::clone(&solend_protocol);
        let health_manager = Arc::clone(&health_manager);
        async move {
            log::info!("   ✅ Data source worker started");
            if let Err(e) =
                data_source::run_data_source(bus, config, rpc_client, protocol, health_manager)
                    .await
            {
                log::error!("❌ Data source error: {}", e);
            }
            log::info!("   ⏹️  Data source worker stopped");
        }
    });

    let mut shutdown_rx = shutdown_manager.subscribe();
    let _ = shutdown_rx.recv().await;

    log::info!("🛑 Shutdown signal received, waiting for workers to finish...");

    health_check_handle.abort();

    tokio::select! {
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
            log::warn!("⚠️  Shutdown timeout reached, forcing exit");
        }
        _ = analyzer_handle => {}
        _ = strategist_handle => {}
        _ = executor_handle => {}
        _ = logger_handle => {}
        _ = rpc_worker_handle => {}
        _ = data_source_handle => {}
    }

    let final_status = health_manager.get_status().await;
    log::info!(
        "📊 Final stats: opportunities={}, tx={}/{}, healthy={}",
        final_status.total_opportunities,
        final_status.successful_transactions,
        final_status.total_transactions,
        final_status.is_healthy
    );

    log::info!("👋 Shutdown complete. Goodbye!");
    Ok(())
}

/// MultiWriter writes to multiple writers simultaneously
struct MultiWriter {
    file: fs::File,
    stderr: std::io::Stderr,
}

impl MultiWriter {
    fn new(file: fs::File, stderr: std::io::Stderr) -> Self {
        Self { file, stderr }
    }
}

impl Write for MultiWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Write to both file and stderr
        self.file.write_all(buf)?;
        self.stderr.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.stderr.flush()?;
        Ok(())
    }
}
