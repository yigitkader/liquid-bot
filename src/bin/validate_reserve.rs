//! Validate Solend Reserve Account Structure
//!
//! Bu binary, gerçek Solend mainnet reserve account'larını parse ederek
//! struct yapısının doğruluğunu doğrular.
//!
//! Kullanım:
//! ```bash
//! cargo run --bin validate_reserve -- \
//!   --rpc-url https://api.mainnet-beta.solana.com \
//!   --reserve BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

// Modüller lib.rs'den geliyor
use liquid_bot::solana_client::SolanaClient;
use liquid_bot::protocols::reserve_validator;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(name = "validate_reserve")]
#[command(about = "Validates Solend reserve account structure against real mainnet accounts")]
struct Args {
    /// RPC URL (e.g., https://api.mainnet-beta.solana.com)
    #[arg(long, default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,

    /// Reserve account pubkey to validate
    #[arg(long, required = true)]
    reserve: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Logger başlat
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_secs()
        .format_module_path(false)
        .format_target(false)
        .init();

    let args = Args::parse();

    log::info!("🔍 Validating Solend Reserve Account Structure");
    log::info!("RPC URL: {}", args.rpc_url);
    log::info!("Reserve: {}", args.reserve);

    // Reserve pubkey'ini parse et
    let reserve_pubkey = args.reserve
        .parse::<Pubkey>()
        .context("Invalid reserve pubkey format")?;

    // RPC client oluştur
    let rpc_client = Arc::new(
        SolanaClient::new(args.rpc_url.clone())
            .context("Failed to create RPC client")?
    );

    // Reserve account'unu validate et
    let result = reserve_validator::validate_reserve_structure(
        rpc_client,
        &reserve_pubkey,
    )
    .await
    .context("Failed to validate reserve structure")?;

    // Sonuçları göster
    println!("\n{}", "=".repeat(80));
    if result.success {
        println!("✅ SUCCESS: Reserve account parsed successfully!");
        println!("{}", "=".repeat(80));
        
        if let Some(info) = result.reserve_info {
            println!("\n📊 Reserve Information:");
            println!("  Version: {}", info.version);
            println!("  Lending Market: {}", info.lending_market);
            println!("  Liquidity Mint: {}", info.liquidity_mint);
            println!("  Collateral Mint: {}", info.collateral_mint);
            println!("  LTV: {:.2}%", info.ltv * 100.0);
            println!("  Liquidation Bonus: {:.2}%", info.liquidation_bonus * 100.0);
        }
        
        println!("\n✅ Struct structure matches the real Solend IDL!");
        println!("   You can safely use this struct in production.");
    } else {
        println!("❌ FAILED: Reserve account parsing failed!");
        println!("{}", "=".repeat(80));
        
        if let Some(error) = result.error {
            println!("\n❌ Error: {}", error);
        }
        
        println!("\n⚠️  ACTION REQUIRED:");
        println!("   1. The struct structure in src/protocols/solend_reserve.rs doesn't match the real Solend IDL");
        println!("   2. Please check the official Solend SDK for the correct structure:");
        println!("      https://github.com/solendprotocol/solend-program");
        println!("   3. Update src/protocols/solend_reserve.rs with the correct structure");
        println!("   4. Re-run this validation tool to verify");
        
        println!("\n💡 TIP: You can also fetch the IDL using:");
        println!("   ./scripts/fetch_solend_idl.sh");
        
        std::process::exit(1);
    }

    Ok(())
}
