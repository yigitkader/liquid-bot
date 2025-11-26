//! Direct Obligation Test Helper
//!
//! Bu binary, obligation account bulma için rehberlik sağlar.

use anyhow::{Context, Result};
use clap::Parser;
use std::sync::Arc;

use liquid_bot::solana_client::SolanaClient;
use liquid_bot::protocols::solend::SolendProtocol;
use liquid_bot::protocol::Protocol;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(name = "test_obligation_direct")]
#[command(about = "Helper tool for finding Solend obligation accounts")]
struct Args {
    /// RPC URL (e.g., https://api.mainnet-beta.solana.com)
    #[arg(long, default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();

    println!("🔍 Solend Obligation Account Finder");
    println!("{}", "=".repeat(80));
    println!("RPC URL: {}", args.rpc_url);
    println!();

    let _rpc_client = Arc::new(
        SolanaClient::new(args.rpc_url.clone())
            .context("Failed to create RPC client")?
    );

    let protocol = SolendProtocol::new()
        .context("Failed to create Solend protocol")?;

    println!("📡 Note: Automatic obligation account finding via RPC");
    println!("   is complex and may hit rate limits.");
    println!();
    println!("   For best results, please provide an obligation account address manually.");
    println!();
    
    show_manual_instructions(&args.rpc_url, &protocol);
    
    println!("💡 Alternative: If you have a known obligation account address,");
    println!("   use the find_and_test_obligation tool:");
    println!();
    println!("   cargo run --bin find_and_test_obligation -- \\");
    println!("     --rpc-url {} \\", args.rpc_url);
    println!("     --obligation <OBLIGATION_PUBKEY>");
    println!();
    
    Ok(())
}

fn show_manual_instructions(rpc_url: &str, protocol: &SolendProtocol) {
    println!("📋 Manual Instructions:");
    println!("-------------------------------------------");
    println!();
    println!("To find a Solend obligation account:");
    println!();
    println!("Method 1: Use Solend Dashboard (RECOMMENDED)");
    println!("  1. Visit: https://solend.fi/dashboard");
    println!("  2. Connect a wallet that has a position");
    println!("  3. Find the obligation account address");
    println!();
    println!("Method 2: Use Solana Explorer");
    println!("  1. Visit: https://explorer.solana.com/address/{}", protocol.program_id());
    println!("  2. Look for 'Program Accounts' section");
    println!("  3. Find accounts with data size > 1000 bytes");
    println!();
    println!("Method 3: Use Solscan");
    println!("  1. Visit: https://solscan.io/account/{}", protocol.program_id());
    println!("  2. Look for obligation accounts");
    println!();
    println!("Once you have an obligation account address, test it with:");
    println!();
    println!("  cargo run --bin find_and_test_obligation -- \\");
    println!("    --rpc-url {} \\", rpc_url);
    println!("    --obligation <OBLIGATION_PUBKEY>");
    println!();
}
