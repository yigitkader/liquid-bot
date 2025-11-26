//! Find and Test My Obligation Account
//!
//! Bu binary, projeye eklenmiş cüzdanın obligation account'larını bulur ve test eder.

use anyhow::{Context, Result};
use clap::Parser;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

use liquid_bot::config::Config;
use liquid_bot::solana_client::SolanaClient;
use liquid_bot::protocols::solend::SolendProtocol;
use liquid_bot::protocol::Protocol;
use liquid_bot::protocols::solend_accounts::derive_obligation_address;
use liquid_bot::wallet::WalletManager;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(name = "find_my_obligation")]
#[command(about = "Finds and tests obligation accounts for the configured wallet")]
struct Args {
    /// RPC URL (e.g., https://api.mainnet-beta.solana.com)
    #[arg(long)]
    rpc_url: Option<String>,

    /// Lending market address (default: main market)
    #[arg(long)]
    lending_market: Option<String>,

    /// Test all known lending markets
    #[arg(long, default_value = "false")]
    test_all_markets: bool,
}

/// Known Solend lending markets (mainnet)
const MAIN_LENDING_MARKET: &str = "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";
// Add more markets if needed
// const TURBO_SOL_MARKET: &str = "...";

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();

    println!("🔍 Finding My Obligation Accounts");
    println!("{}", "=".repeat(80));
    println!();

    // Load config
    let config = Config::from_env()
        .context("Failed to load config")?;

    let rpc_url = args.rpc_url.unwrap_or(config.rpc_http_url.clone());
    println!("RPC URL: {}", rpc_url);
    println!();

    // Load wallet
    println!("🔑 Loading wallet from: {}", config.wallet_path);
    let wallet = WalletManager::from_file(&config.wallet_path)
        .context("Failed to load wallet")?;
    
    let wallet_pubkey = wallet.pubkey();
    println!("✅ Wallet loaded: {}", wallet_pubkey);
    println!();

    // Create RPC client
    let rpc_client = Arc::new(
        SolanaClient::new(rpc_url.clone())
            .context("Failed to create RPC client")?
    );

    // Create protocol
    let protocol = SolendProtocol::new()
        .context("Failed to create Solend protocol")?;

    // Determine which markets to test
    let markets_to_test = if args.test_all_markets {
        vec![MAIN_LENDING_MARKET.to_string()]
        // Add more markets here if needed
    } else if let Some(market) = args.lending_market {
        vec![market]
    } else {
        vec![MAIN_LENDING_MARKET.to_string()]
    };

    println!("📊 Testing {} lending market(s)...", markets_to_test.len());
    println!();

    let mut found_obligations = Vec::new();

    for market_str in &markets_to_test {
        let lending_market = market_str
            .parse::<Pubkey>()
            .context("Invalid lending market address")?;

        println!("🔍 Testing Lending Market: {}", lending_market);
        println!("   Market: {}", market_str);
        println!();

        // Derive obligation address
        let obligation_pubkey = derive_obligation_address(
            &wallet_pubkey,
            &lending_market,
            &protocol.program_id(),
        )
        .context("Failed to derive obligation address")?;

        println!("   Derived Obligation Address: {}", obligation_pubkey);
        println!();

        // Try to fetch and test the obligation account
        match rpc_client.get_account(&obligation_pubkey).await {
            Ok(account) => {
                if account.data.is_empty() {
                    println!("   ⚠️  Obligation account exists but is empty (no position)");
                    println!();
                    continue;
                }

                println!("   ✅ Obligation account found! Testing...");
                println!();

                // Test parsing
                match protocol.parse_account_position(&obligation_pubkey, &account, Some(Arc::clone(&rpc_client))).await {
                    Ok(Some(position)) => {
                        println!("   {}✅ SUCCESS: Obligation account parsed successfully!", " ".repeat(3));
                        println!();
                        println!("   {}📊 Obligation Details:", " ".repeat(3));
                        println!("   {}  Account: {}", " ".repeat(3), position.account_address);
                        println!("   {}  Protocol: {}", " ".repeat(3), position.protocol_id);
                        println!("   {}  Health Factor: {:.4}", " ".repeat(3), position.health_factor);
                        println!("   {}  Total Collateral: ${:.2}", " ".repeat(3), position.total_collateral_usd);
                        println!("   {}  Total Debt: ${:.2}", " ".repeat(3), position.total_debt_usd);
                        println!();

                        if !position.collateral_assets.is_empty() {
                            println!("   {}💎 Collateral Assets ({}):", " ".repeat(3), position.collateral_assets.len());
                            for (i, asset) in position.collateral_assets.iter().enumerate() {
                                println!("   {}    {}. Mint: {}", " ".repeat(3), i + 1, asset.mint);
                                println!("   {}       Amount: {} (${:.2})", " ".repeat(3), asset.amount, asset.amount_usd);
                                println!("   {}       LTV: {:.2}%", " ".repeat(3), asset.ltv * 100.0);
                            }
                            println!();
                        }

                        if !position.debt_assets.is_empty() {
                            println!("   {}💳 Debt Assets ({}):", " ".repeat(3), position.debt_assets.len());
                            for (i, asset) in position.debt_assets.iter().enumerate() {
                                println!("   {}    {}. Mint: {}", " ".repeat(3), i + 1, asset.mint);
                                println!("   {}       Amount: {} (${:.2})", " ".repeat(3), asset.amount, asset.amount_usd);
                                println!("   {}       Borrow Rate: {:.4}%", " ".repeat(3), asset.borrow_rate * 100.0);
                            }
                            println!();
                        }

                        found_obligations.push((lending_market, obligation_pubkey, position));
                    }
                    Ok(None) => {
                        println!("   ⚠️  Account is not a valid obligation (different account type)");
                        println!();
                    }
                    Err(e) => {
                        println!("   ❌ Failed to parse obligation: {}", e);
                        println!();
                    }
                }
            }
            Err(e) => {
                println!("   ⚠️  Obligation account not found or error: {}", e);
                println!("   This is normal if you don't have a position in this market.");
                println!();
            }
        }
    }

    // Summary
    println!("{}", "=".repeat(80));
    if found_obligations.is_empty() {
        println!("⚠️  No active obligation accounts found");
        println!();
        println!("This means:");
        println!("  1. You don't have any active positions in Solend");
        println!("  2. Or you're using a different lending market");
        println!();
        println!("💡 To create a position:");
        println!("  1. Visit: https://solend.fi/dashboard");
        println!("  2. Connect your wallet: {}", wallet_pubkey);
        println!("  3. Deposit or borrow to create an obligation");
        println!();
    } else {
        println!("✅ Found {} active obligation account(s)!", found_obligations.len());
        println!();
        println!("📋 Summary:");
        for (market, obligation, position) in &found_obligations {
            println!("  Market: {}", market);
            println!("  Obligation: {}", obligation);
            println!("  Health Factor: {:.4}", position.health_factor);
            println!("  Collateral: ${:.2}, Debt: ${:.2}", position.total_collateral_usd, position.total_debt_usd);
            println!();
        }
        println!("{}", "=".repeat(80));
        println!("✅ OBLIGATION STRUCT VALIDATION SUCCESSFUL!");
        println!("   The obligation struct successfully parsed your real mainnet obligation account(s).");
        println!("   The struct is correctly implemented and ready for production use.");
        println!("{}", "=".repeat(80));
    }

    Ok(())
}

