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

use anyhow::Result;
use dotenv::dotenv;
use env_logger;
use tokio;

#[tokio::main]
async fn main() -> Result<()> {
    // Environment variables yükle
    dotenv().ok();
    
    // Logger başlat
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();
    
    log::info!("Starting Solana Liquidation Bot...");
    
    // Config yükle
    let config = config::Config::from_env()?;
    log::info!("Configuration loaded: dry_run={}", config.dry_run);
    
    // Event Bus oluştur
    let bus = event_bus::EventBus::new(1000); // Buffer size: 1000 events
    
    // Worker'lar için receiver'ları al
    let analyzer_receiver = bus.subscribe();
    let strategist_receiver = bus.subscribe();
    let executor_receiver = bus.subscribe();
    let logger_receiver = bus.subscribe();
    
    // Worker'ları spawn et
    let bus_clone_1 = bus.clone();
    let config_clone_1 = config.clone();
    tokio::spawn(async move {
        if let Err(e) = analyzer::run_analyzer(analyzer_receiver, bus_clone_1, config_clone_1).await {
            log::error!("Analyzer error: {}", e);
        }
    });
    
    let bus_clone_2 = bus.clone();
    let config_clone_2 = config.clone();
    tokio::spawn(async move {
        if let Err(e) = strategist::run_strategist(strategist_receiver, bus_clone_2, config_clone_2).await {
            log::error!("Strategist error: {}", e);
        }
    });
    
    let bus_clone_3 = bus.clone();
    let config_clone_3 = config.clone();
    tokio::spawn(async move {
        if let Err(e) = executor::run_executor(executor_receiver, bus_clone_3, config_clone_3).await {
            log::error!("Executor error: {}", e);
        }
    });
    
    tokio::spawn(async move {
        if let Err(e) = logger::run_logger(logger_receiver).await {
            log::error!("Logger error: {}", e);
        }
    });
    
    // Data source'u başlat (ana task)
    log::info!("Starting data source...");
    data_source::run_data_source(bus, config).await?;
    
    Ok(())
}
