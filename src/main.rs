mod config;
mod domain;
mod event;
mod event_bus;
mod data_source;
mod rpc_poller;
mod ws_listener;
mod analyzer;
mod strategist;
mod executor;
mod logger;
mod solana_client;
mod math;
mod wallet;
mod protocol;
mod tx_lock;
mod rate_limiter;
mod shutdown;
mod health;
mod performance;

mod protocols {
    pub mod solend;
    pub mod solend_accounts;
    pub mod reserve_helper;
    pub mod solend_reserve;
    pub mod oracle_helper;
    pub mod reserve_validator;
}

use anyhow::Result;
use dotenv::dotenv;
use env_logger;
use tokio;
use tokio::signal;
use std::sync::Arc;
use crate::protocol::Protocol;
use crate::shutdown::ShutdownManager;

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
    
    // Protocol Registry oluştur - Trait tabanlı mimari (gelecekte çoklu protokol desteği için)
    // 
    // ŞU AN: Sadece Solend protokolü kullanılıyor (tek protokol - business requirement)
    // GELECEK: Yeni protokol eklemek için sadece registry'ye eklemek yeterli
    // 
    // Mimari tasarım:
    // - Protocol trait: Tüm protokoller için ortak arayüz
    // - ProtocolRegistry: Protokolleri yönetir (şu an 1, gelecekte N)
    // - Worker'lar: Trait üzerinden çalışır (protokole bağımlı değil)
    let mut protocol_registry = protocol::ProtocolRegistry::new();
    
    // Solend protokolünü ekle (şu an tek protokol)
    let solend_protocol = match protocols::solend::SolendProtocol::new() {
        Ok(proto) => {
            let protocol_id = proto.id().to_string();
            let program_id = proto.program_id();
            
            // Registry'ye ekle (gelecekte çoklu protokol için)
            protocol_registry.register(Box::new(proto));
            
            log::info!("✅ Protocol registered: {}", protocol_id);
            log::info!("   Program ID: {}", program_id);
            log::info!("   📌 Current mode: Single protocol (Solend)");
            log::info!("   🔮 Architecture: Multi-protocol ready (trait-based)");
            
            // Aynı protokolü tekrar oluştur (registry'deki Box'tan kullanılamaz çünkü Arc'a dönüştürülemez)
            match protocols::solend::SolendProtocol::new() {
                Ok(proto) => Arc::new(proto) as Arc<dyn protocol::Protocol>,
                Err(e) => {
                    log::error!("Failed to create protocol Arc: {}", e);
                    return Err(e);
                }
            }
        }
        Err(e) => {
            log::error!("❌ Failed to initialize protocol: {}", e);
            return Err(e);
        }
    };
    
    // Protocol registry durumunu logla
    let protocol_count = protocol_registry.count();
    log::info!("📋 Protocol registry: {} protocol(s) registered", protocol_count);
    
    // Gelecekte çoklu protokol desteği için hazır olduğunu göster
    if protocol_count == 1 {
        log::info!("   💡 To add more protocols: Create new protocol struct + implement Protocol trait + register()");
        log::info!("   📚 See: docs/MULTI_PROTOCOL_ARCHITECTURE.md");
    }
    
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
    
    // Worker'ları spawn et - production için task tracking
    log::info!("🔧 Starting worker tasks...");
    
    let bus_clone_1 = bus.clone();
    let config_clone_1 = config.clone();
    let performance_tracker_clone_1 = Arc::clone(&performance_tracker);
    let analyzer_handle = tokio::spawn(async move {
        log::info!("   ✅ Analyzer worker started");
        if let Err(e) = analyzer::run_analyzer(analyzer_receiver, bus_clone_1, config_clone_1, performance_tracker_clone_1).await {
            log::error!("❌ Analyzer worker error: {}", e);
        }
        log::info!("   ⏹️  Analyzer worker stopped");
    });
    
    // Wallet balance checker oluştur
    let wallet_balance_checker = Arc::new(wallet::WalletBalanceChecker::new(
        *wallet.pubkey(),
        Arc::clone(&rpc_client),
    ));
    
    let bus_clone_2 = bus.clone();
    let config_clone_2 = config.clone();
    let wallet_balance_checker_clone = Arc::clone(&wallet_balance_checker);
    let strategist_handle = tokio::spawn(async move {
        log::info!("   ✅ Strategist worker started");
        if let Err(e) = strategist::run_strategist(
            strategist_receiver,
            bus_clone_2,
            config_clone_2,
            wallet_balance_checker_clone,
        ).await {
            log::error!("❌ Strategist worker error: {}", e);
        }
        log::info!("   ⏹️  Strategist worker stopped");
    });
    
    // Executor için wallet, protocol, rpc_client ve performance_tracker clone et
    let bus_clone_3 = bus.clone();
    let config_clone_3 = config.clone();
    let wallet_clone = Arc::clone(&wallet);
    let protocol_clone = Arc::clone(&solend_protocol) as Arc<dyn protocol::Protocol>;
    let rpc_client_clone = Arc::clone(&rpc_client);
    let performance_tracker_clone_2 = Arc::clone(&performance_tracker);
    let executor_handle = tokio::spawn(async move {
        log::info!("   ✅ Executor worker started");
        if let Err(e) = executor::run_executor(
            executor_receiver,
            bus_clone_3,
            config_clone_3,
            wallet_clone,
            protocol_clone,
            rpc_client_clone,
            performance_tracker_clone_2,
        ).await {
            log::error!("❌ Executor worker error: {}", e);
        }
        log::info!("   ⏹️  Executor worker stopped");
    });
    
    let health_manager_for_logger = Arc::clone(&health_manager);
    let logger_handle = tokio::spawn(async move {
        log::info!("   ✅ Logger worker started");
        if let Err(e) = logger::run_logger(logger_receiver, health_manager_for_logger).await {
            log::error!("❌ Logger worker error: {}", e);
        }
        log::info!("   ⏹️  Logger worker stopped");
    });
    
    // Data source için rpc_client, protocol ve health_manager clone'ları
    let rpc_client_for_source = Arc::clone(&rpc_client);
    let protocol_for_source = Arc::clone(&solend_protocol) as Arc<dyn protocol::Protocol>;
    let health_manager_for_source = Arc::clone(&health_manager);
    
    // Health check task - periyodik olarak sistem sağlığını kontrol et
    let health_manager_for_check = Arc::clone(&health_manager);
    let performance_tracker_for_check = Arc::clone(&performance_tracker);
    let health_check_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let is_healthy = health_manager_for_check.check_health().await;
            let status = health_manager_for_check.get_status().await;
            
            // Performance metrics logla
            performance_tracker_for_check.log_metrics().await;
            
            if !is_healthy {
                log::warn!("⚠️  Health check failed: consecutive_errors={}, last_error={:?}", 
                    status.consecutive_errors,
                    status.last_error
                );
            } else {
                log::debug!("✅ Health check passed: opportunities={}, tx={}/{}", 
                    status.total_opportunities,
                    status.successful_transactions,
                    status.total_transactions
                );
            }
        }
    });
    
    log::info!("✅ All workers started");
    log::info!("🎯 Bot is running. Press Ctrl+C to stop gracefully.");
    
    // Data source'u başlat (ana task) - shutdown sinyali ile durdurulabilir
    let data_source_handle = tokio::spawn(async move {
        log::info!("   ✅ Data source worker started");
        if let Err(e) = data_source::run_data_source(bus, config, rpc_client_for_source, protocol_for_source, health_manager_for_source).await {
            log::error!("❌ Data source error: {}", e);
        }
        log::info!("   ⏹️  Data source worker stopped");
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
