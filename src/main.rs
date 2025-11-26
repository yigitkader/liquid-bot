// Modüller artık lib.rs'de tanımlı
use liquid_bot::*;

use anyhow::Result;
use dotenv::dotenv;
use env_logger;
use tokio;
use tokio::signal;
use std::sync::Arc;
use liquid_bot::protocol::Protocol;
use liquid_bot::shutdown::ShutdownManager;

#[tokio::main]
async fn main() -> Result<()> {
    // Environment variables yükle
    dotenv().ok();
    
    // Logger başlat - production için structured logging
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_secs()
        .format_module_path(false)
        .format_target(false)
        .init();
    
    log::info!("🚀 Starting Solana Liquidation Bot (Production Mode)");
    log::info!("Version: {}", env!("CARGO_PKG_VERSION"));
    
    // Config yükle ve validate et
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
    
    // Graceful shutdown manager
    let shutdown_manager = Arc::new(ShutdownManager::new());
    
    // Health check manager
    let health_manager = Arc::new(health::HealthManager::new(300)); // 5 dakika max error age
    
    // Signal handling - graceful shutdown için
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
    
    // Wallet yükle
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
    
    // RPC Client oluştur
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
    
    let solend_protocol = match protocols::solend::SolendProtocol::new() {
        Ok(proto) => {
            let protocol_id = proto.id().to_string();
            let program_id = proto.program_id();
            log::info!("✅ Protocol initialized: {} (program: {})", protocol_id, program_id);
            Arc::new(proto) as Arc<dyn protocol::Protocol>
        }
        Err(e) => {
            log::error!("❌ Failed to initialize protocol: {}", e);
            return Err(e);
        }
    };
    
    // Event Bus oluştur
    let bus = event_bus::EventBus::new(1000); // Buffer size: 1000 events
    log::info!("✅ Event bus initialized");
    
    // Worker'lar için receiver'ları al
    let analyzer_receiver = bus.subscribe();
    let strategist_receiver = bus.subscribe();
    let executor_receiver = bus.subscribe();
    let logger_receiver = bus.subscribe();
    
    // Performance tracker
    let performance_tracker = Arc::new(performance::PerformanceTracker::new());
    
    log::info!("🔧 Starting worker tasks...");
    
    let wallet_balance_checker = Arc::new(wallet::WalletBalanceChecker::new(
        *wallet.pubkey(),
        Arc::clone(&rpc_client),
    ));
    
    let analyzer_handle = tokio::spawn({
        let bus = bus.clone();
        let config = config.clone();
        let performance_tracker = Arc::clone(&performance_tracker);
        let protocol = Arc::clone(&solend_protocol);
        async move {
            log::info!("   ✅ Analyzer worker started");
            if let Err(e) = analyzer::run_analyzer(analyzer_receiver, bus, config, performance_tracker, protocol).await {
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
        async move {
            log::info!("   ✅ Strategist worker started");
            if let Err(e) = strategist::run_strategist(strategist_receiver, bus, config, wallet_balance_checker, rpc_client, protocol).await {
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
        async move {
            log::info!("   ✅ Executor worker started");
            if let Err(e) = executor::run_executor(executor_receiver, bus, config, wallet, protocol, rpc_client, performance_tracker).await {
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
                    log::warn!("⚠️  Health check failed: consecutive_errors={}, last_error={:?}", 
                        status.consecutive_errors, status.last_error);
                } else {
                    log::debug!("✅ Health check passed: opportunities={}, tx={}/{}", 
                        status.total_opportunities, status.successful_transactions, status.total_transactions);
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
            if let Err(e) = data_source::run_data_source(bus, config, rpc_client, protocol, health_manager).await {
                log::error!("❌ Data source error: {}", e);
            }
            log::info!("   ⏹️  Data source worker stopped");
        }
    });
    
    // Graceful shutdown bekle
    let mut shutdown_rx = shutdown_manager.subscribe();
    let _ = shutdown_rx.recv().await;
    
    log::info!("🛑 Shutdown signal received, waiting for workers to finish...");
    
    // Health check task'ı durdur
    health_check_handle.abort();
    
    // Worker'ların tamamlanmasını bekle (timeout ile)
    tokio::select! {
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
            log::warn!("⚠️  Shutdown timeout reached, forcing exit");
        }
        _ = analyzer_handle => {}
        _ = strategist_handle => {}
        _ = executor_handle => {}
        _ = logger_handle => {}
        _ = data_source_handle => {}
    }
    
    // Final health status
    let final_status = health_manager.get_status().await;
    log::info!("📊 Final stats: opportunities={}, tx={}/{}, healthy={}", 
        final_status.total_opportunities,
        final_status.successful_transactions,
        final_status.total_transactions,
        final_status.is_healthy
    );
    
    log::info!("👋 Shutdown complete. Goodbye!");
    Ok(())
}
