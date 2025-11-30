// Structure.md'ye göre main.rs - Event-driven architecture
use anyhow::{Context, Result};
use dotenv::dotenv;
use log;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::signal;
use tokio::time::{sleep, Duration};

use liquid_bot::core::config::Config;
use liquid_bot::core::events::EventBus;
use liquid_bot::blockchain::rpc_client::RpcClient;
use liquid_bot::blockchain::ws_client::WsClient;
use liquid_bot::engine::scanner::Scanner;
use liquid_bot::engine::analyzer::Analyzer;
use liquid_bot::engine::validator::Validator;
use liquid_bot::engine::executor::Executor;
use liquid_bot::strategy::balance_manager::BalanceManager;
use liquid_bot::utils::cache::AccountCache;
use liquid_bot::utils::metrics::Metrics;
use liquid_bot::protocol::Protocol;
use liquid_bot::protocol::solend::SolendProtocol;

use solana_sdk::signature::{Keypair, Signer};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_secs()
        .init();

    dotenv().ok();

    log::info!("🚀 Starting Solana Liquidation Bot");

    // 1. Load config
    let config = Config::from_env()
        .context("Failed to load configuration")?;
    config.validate()
        .context("Configuration validation failed")?;

    log::info!("✅ Configuration loaded");

    // 2. Initialize components
    let rpc = Arc::new(
        RpcClient::new(config.rpc_http_url.clone())
            .context("Failed to create RPC client")?
    );
    log::info!("✅ RPC client initialized");

    let mut ws = WsClient::new(config.rpc_ws_url.clone());
    ws.connect().await
        .context("Failed to connect WebSocket")?;
    let ws = Arc::new(ws);
    log::info!("✅ WebSocket client initialized");

    // Load wallet
    let wallet_keypair = load_wallet(&config.wallet_path)
        .context("Failed to load wallet")?;
    let wallet = Arc::new(wallet_keypair);
    let wallet_pubkey = wallet.pubkey();
    log::info!("✅ Wallet loaded: {}", wallet_pubkey);

    // 3. Create event bus
    let event_bus = EventBus::new(config.event_bus_buffer_size);
    log::info!("✅ Event bus initialized");

    // 4. Create managers
    let balance_manager = Arc::new(
        BalanceManager::new(Arc::clone(&rpc), wallet_pubkey)
    );
    log::info!("✅ Balance manager initialized");

    let metrics = Arc::new(Metrics::new());
    log::info!("✅ Metrics initialized");

    let cache = Arc::new(AccountCache::new());
    log::info!("✅ Account cache initialized");

    // 5. Initialize protocol
    let protocol: Arc<dyn Protocol> = Arc::new(
        SolendProtocol::new(&config)
            .context("Failed to initialize Solend protocol")?
    );
    log::info!("✅ Protocol initialized: {} (program: {})", protocol.id(), protocol.program_id());

    // 6. Spawn workers (Structure.md'ye göre)
    let scanner = Scanner::new(
        Arc::clone(&rpc),
        Arc::clone(&ws),
        Arc::clone(&protocol),
        event_bus.clone(),
        Arc::clone(&cache),
    );
    let analyzer = Analyzer::new(
        event_bus.clone(),
        Arc::clone(&protocol),
        config.clone(),
    );
    let validator = Validator::new(
        event_bus.clone(),
        Arc::clone(&balance_manager),
        config.clone(),
        Arc::clone(&rpc),
    );
    let executor = Executor::new(
        event_bus.clone(),
        Arc::clone(&rpc),
        Arc::clone(&wallet),
        Arc::clone(&protocol),
        Arc::clone(&balance_manager),
        config.clone(),
    );

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
    tokio::spawn(async move {
        if let Err(e) = executor.run().await {
            log::error!("Executor error: {}", e);
        }
    });
    
    log::info!("✅ All workers started");

    // 6. Metrics logger
    let metrics_clone = Arc::clone(&metrics);
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(60)).await;
            let summary = metrics_clone.get_summary().await;
            log::info!("📊 Metrics: Opportunities: {}, TX Sent: {}, TX Success: {}, Success Rate: {:.2}%, Total Profit: ${:.2}, Avg Latency: {}ms, P95 Latency: {}ms",
                summary.opportunities,
                summary.tx_sent,
                summary.tx_success,
                summary.success_rate * 100.0,
                summary.total_profit,
                summary.avg_latency_ms,
                summary.p95_latency_ms
            );
        }
    });

    log::info!("✅ All components initialized");
    log::info!("⏳ Waiting for shutdown signal (Ctrl+C)...");

    // 7. Wait for shutdown signal
    signal::ctrl_c().await
        .context("Failed to listen for shutdown signal")?;

    log::info!("🛑 Shutting down gracefully...");
    Ok(())
}

fn load_wallet(path: &str) -> Result<Keypair> {
    let wallet_path = Path::new(path);
    
    if !wallet_path.exists() {
        return Err(anyhow::anyhow!("Wallet file not found: {}", path));
    }

    let keypair_bytes = fs::read(wallet_path)
        .context("Failed to read wallet file")?;

    // Try to parse as JSON keypair first (Solana CLI format)
    if let Ok(keypair) = serde_json::from_slice::<Vec<u8>>(&keypair_bytes) {
        if keypair.len() == 64 {
            return Keypair::from_bytes(&keypair)
                .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
        }
    }

    // Try to parse as raw bytes (64 bytes)
    if keypair_bytes.len() == 64 {
        return Keypair::from_bytes(&keypair_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
    }

    // Try to parse as base58 string
    if let Ok(keypair_str) = String::from_utf8(keypair_bytes.clone()) {
        if let Ok(keypair_bytes_decoded) = bs58::decode(keypair_str.trim())
            .into_vec()
        {
            if keypair_bytes_decoded.len() == 64 {
                return Keypair::from_bytes(&keypair_bytes_decoded)
                    .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
            }
        }
    }

    Err(anyhow::anyhow!("Invalid wallet format: expected 64 bytes, JSON array, or base58 string"))
}
