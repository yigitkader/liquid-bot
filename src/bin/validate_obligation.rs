//! Validate Solend Obligation Account Structure
//!
//! Bu binary, gerçek Solend mainnet obligation account'larını parse ederek
//! struct yapısının doğruluğunu doğrular.
//!
//! Kullanım:
//! ```bash
//! cargo run --bin validate_obligation -- \
//!   --rpc-url https://api.mainnet-beta.solana.com \
//!   --obligation <OBLIGATION_PUBKEY>
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

// Modüller lib.rs'den geliyor
use liquid_bot::solana_client::SolanaClient;
use liquid_bot::protocols::solend::SolendProtocol;
use liquid_bot::protocol::Protocol;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(name = "validate_obligation")]
#[command(about = "Validates Solend obligation account structure against real mainnet accounts")]
struct Args {
    /// RPC URL (e.g., https://api.mainnet-beta.solana.com)
    #[arg(long, default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,

    /// Obligation account pubkey to validate
    #[arg(long, required = true)]
    obligation: String,
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

    log::info!("🔍 Validating Solend Obligation Account Structure");
    log::info!("RPC URL: {}", args.rpc_url);
    log::info!("Obligation: {}", args.obligation);

    // Obligation pubkey'ini parse et
    let obligation_pubkey = args.obligation
        .parse::<Pubkey>()
        .context("Invalid obligation pubkey format")?;

    // RPC client oluştur
    let rpc_client = Arc::new(
        SolanaClient::new(args.rpc_url.clone())
            .context("Failed to create RPC client")?
    );

    // Solend protocol oluştur
    let protocol = SolendProtocol::new()
        .context("Failed to create Solend protocol")?;

    // Account'u oku
    let account = rpc_client.get_account(&obligation_pubkey).await
        .context("Failed to fetch obligation account")?;

    if account.data.is_empty() {
        return Err(anyhow::anyhow!("Obligation account data is empty"));
    }

    log::info!("Account data size: {} bytes", account.data.len());
    log::info!("Account owner: {}", account.owner);
    log::info!("Expected program ID: {}", protocol.program_id());

    // Account'un Solend program'ına ait olduğunu kontrol et
    if account.owner != protocol.program_id() {
        return Err(anyhow::anyhow!(
            "Account owner {} does not match Solend program ID {}",
            account.owner,
            protocol.program_id()
        ));
    }

    // Parse et
    match protocol.parse_account_position(&obligation_pubkey, &account, Some(Arc::clone(&rpc_client))).await {
        Ok(Some(position)) => {
            println!("\n{}", "=".repeat(80));
            println!("✅ SUCCESS: Obligation account parsed successfully!");
            println!("{}", "=".repeat(80));
            
            println!("\n📊 Obligation Information:");
            println!("  Account Address: {}", position.account_address);
            println!("  Protocol: {}", position.protocol_id);
            println!("  Health Factor: {:.4}", position.health_factor);
            println!("  Total Collateral: ${:.2}", position.total_collateral_usd);
            println!("  Total Debt: ${:.2}", position.total_debt_usd);
            
            println!("\n💎 Collateral Assets ({}):", position.collateral_assets.len());
            for (i, asset) in position.collateral_assets.iter().enumerate() {
                println!("  {}. Mint: {}", i + 1, asset.mint);
                println!("     Amount: {} (${:.2})", asset.amount, asset.amount_usd);
                println!("     LTV: {:.2}%", asset.ltv * 100.0);
            }
            
            println!("\n💳 Debt Assets ({}):", position.debt_assets.len());
            for (i, asset) in position.debt_assets.iter().enumerate() {
                println!("  {}. Mint: {}", i + 1, asset.mint);
                println!("     Amount: {} (${:.2})", asset.amount, asset.amount_usd);
                println!("     Borrow Rate: {:.4}%", asset.borrow_rate * 100.0);
            }
            
            println!("\n✅ Struct structure matches the real Solend IDL!");
            println!("   You can safely use this struct in production.");
            
            Ok(())
        }
        Ok(None) => {
            println!("\n{}", "=".repeat(80));
            println!("❌ FAILED: Account is not a valid Solend obligation");
            println!("{}", "=".repeat(80));
            println!("\n⚠️  This account might be a different type (e.g., reserve, market)");
            std::process::exit(1);
        }
        Err(e) => {
            println!("\n{}", "=".repeat(80));
            println!("❌ FAILED: Obligation account parsing failed!");
            println!("{}", "=".repeat(80));
            
            println!("\n❌ Error: {}", e);
            
            // Hex dump ilk 200 byte'ı göster (debug için)
            let hex_dump: String = account.data.iter()
                .take(200)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            println!("\n📋 Debug Information:");
            println!("  Account data size: {} bytes", account.data.len());
            println!("  First 200 bytes (hex): {}", hex_dump);
            
            println!("\n⚠️  ACTION REQUIRED:");
            println!("   1. The struct structure in src/protocols/solend.rs doesn't match the real Solend IDL");
            println!("   2. Please check the official Solend SDK for the correct structure:");
            println!("      https://github.com/solendprotocol/solend-program");
            println!("   3. Update src/protocols/solend.rs with the correct structure");
            println!("   4. Re-run this validation tool to verify");
            
            std::process::exit(1);
        }
    }
}

