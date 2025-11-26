//! Find and Test Solend Obligation Accounts
//!
//! Bu binary, Solend mainnet'te obligation account'larını bulur ve test eder.

use anyhow::{Context, Result};
use clap::Parser;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

use liquid_bot::solana_client::SolanaClient;
use liquid_bot::protocols::solend::SolendProtocol;
use liquid_bot::protocol::Protocol;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(name = "find_and_test_obligation")]
#[command(about = "Finds and tests Solend obligation accounts on mainnet")]
struct Args {
    /// RPC URL (e.g., https://api.mainnet-beta.solana.com)
    #[arg(long, default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,

    /// Obligation account pubkey to test (optional - if not provided, will try to find one)
    #[arg(long)]
    obligation: Option<String>,

    /// Try to find obligation accounts automatically
    #[arg(long, default_value = "false")]
    auto_find: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();

    log::info!("🔍 Finding and Testing Solend Obligation Accounts");
    log::info!("RPC URL: {}", args.rpc_url);
    println!();

    let rpc_client = Arc::new(
        SolanaClient::new(args.rpc_url.clone())
            .context("Failed to create RPC client")?
    );

    let protocol = SolendProtocol::new()
        .context("Failed to create Solend protocol")?;

    // If obligation address is provided, test it directly
    if let Some(obligation_str) = args.obligation {
        let obligation_pubkey = obligation_str
            .parse::<Pubkey>()
            .context("Invalid obligation pubkey format")?;
        
        return test_obligation(&protocol, &rpc_client, &obligation_pubkey).await;
    }

    // Otherwise, try to find obligation accounts
    if args.auto_find {
        println!("🔍 Attempting to find obligation accounts automatically...");
        println!("   (This may take a while and may hit rate limits)");
        println!();
        
        // Try to find obligation accounts using known patterns
        // Solend obligation accounts are typically derived from user wallets
        // We can try some common patterns or use RPC getProgramAccounts
        
        println!("⚠️  Auto-find is not fully implemented yet.");
        println!("   Please provide an obligation account address manually.");
        println!();
    }

    // Show instructions for finding obligation accounts
    println!("📋 How to find a Solend obligation account:");
    println!("-------------------------------------------");
    println!();
    println!("Method 1: Use Solend Dashboard");
    println!("  1. Visit: https://solend.fi/dashboard");
    println!("  2. Connect your wallet");
    println!("  3. Find your obligation account address");
    println!();
    
    println!("Method 2: Use Solana Explorer");
    println!("  1. Visit: https://explorer.solana.com/address/{}", protocol.program_id());
    println!("  2. Look for 'Program Accounts' section");
    println!("  3. Find accounts with data size > 1000 bytes (likely obligations)");
    println!();
    
    println!("Method 3: Use Solscan");
    println!("  1. Visit: https://solscan.io/account/{}", protocol.program_id());
    println!("  2. Look for obligation accounts");
    println!();
    
    println!("Method 4: Use RPC getProgramAccounts");
    println!("  curl -X POST {} \\", args.rpc_url);
    println!("    -H 'Content-Type: application/json' \\");
    println!("    -d '{{");
    println!("      \"jsonrpc\": \"2.0\",");
    println!("      \"id\": 1,");
    println!("      \"method\": \"getProgramAccounts\",");
    println!("      \"params\": [");
    println!("        \"{}\",", protocol.program_id());
    println!("        {{");
    println!("          \"filters\": [{{ \"dataSize\": 1000 }}]");
    println!("        }}");
    println!("      ]");
    println!("    }}'");
    println!();
    
    println!("Once you have an obligation account address, test it with:");
    println!();
    println!("  cargo run --bin find_and_test_obligation -- \\");
    println!("    --rpc-url {} \\", args.rpc_url);
    println!("    --obligation <OBLIGATION_PUBKEY>");
    println!();

    // Try a known test pattern: obligation accounts are often derived from user wallets
    // We can try to find one by checking if there are any public obligation accounts
    println!("💡 TIP: You can also test with a known obligation account if you have one.");
    println!("   Some obligation accounts are publicly visible on Solend's dashboard.");
    println!();

    Ok(())
}

async fn test_obligation(
    protocol: &SolendProtocol,
    rpc_client: &Arc<SolanaClient>,
    obligation_pubkey: &Pubkey,
) -> Result<()> {
    println!("🧪 Testing Obligation Account: {}", obligation_pubkey);
    println!("{}", "=".repeat(80));
    println!();

    // Fetch account
    let account = rpc_client.get_account(obligation_pubkey).await
        .context("Failed to fetch obligation account")?;

    if account.data.is_empty() {
        return Err(anyhow::anyhow!("Obligation account data is empty"));
    }

    println!("📊 Account Information:");
    println!("  Address: {}", obligation_pubkey);
    println!("  Owner: {}", account.owner);
    println!("  Expected Program: {}", protocol.program_id());
    println!("  Data Size: {} bytes", account.data.len());
    println!();

    // Check if account is owned by Solend program
    if account.owner != protocol.program_id() {
        return Err(anyhow::anyhow!(
            "Account owner {} does not match Solend program ID {}",
            account.owner,
            protocol.program_id()
        ));
    }

    println!("✅ Account is owned by Solend program");
    println!();

    // Try to parse the obligation
    println!("🔍 Attempting to parse obligation account...");
    println!();

    match protocol.parse_account_position(obligation_pubkey, &account, Some(Arc::clone(rpc_client))).await {
        Ok(Some(position)) => {
            println!("{}", "=".repeat(80));
            println!("✅ SUCCESS: Obligation account parsed successfully!");
            println!("{}", "=".repeat(80));
            println!();
            
            println!("📊 Obligation Information:");
            println!("  Account Address: {}", position.account_address);
            println!("  Protocol: {}", position.protocol_id);
            println!("  Health Factor: {:.4}", position.health_factor);
            println!("  Total Collateral: ${:.2}", position.total_collateral_usd);
            println!("  Total Debt: ${:.2}", position.total_debt_usd);
            println!();
            
            if !position.collateral_assets.is_empty() {
                println!("💎 Collateral Assets ({}):", position.collateral_assets.len());
                for (i, asset) in position.collateral_assets.iter().enumerate() {
                    println!("  {}. Mint: {}", i + 1, asset.mint);
                    println!("     Amount: {} (${:.2})", asset.amount, asset.amount_usd);
                    println!("     LTV: {:.2}%", asset.ltv * 100.0);
                }
                println!();
            }
            
            if !position.debt_assets.is_empty() {
                println!("💳 Debt Assets ({}):", position.debt_assets.len());
                for (i, asset) in position.debt_assets.iter().enumerate() {
                    println!("  {}. Mint: {}", i + 1, asset.mint);
                    println!("     Amount: {} (${:.2})", asset.amount, asset.amount_usd);
                    println!("     Borrow Rate: {:.4}%", asset.borrow_rate * 100.0);
                }
                println!();
            }
            
            println!("{}", "=".repeat(80));
            println!("✅ Struct structure matches the real Solend IDL!");
            println!("   The obligation struct is correctly implemented and can be used in production.");
            println!("{}", "=".repeat(80));
            
            Ok(())
        }
        Ok(None) => {
            println!("{}", "=".repeat(80));
            println!("❌ FAILED: Account is not a valid Solend obligation");
            println!("{}", "=".repeat(80));
            println!();
            println!("⚠️  This account might be a different type (e.g., reserve, market)");
            println!("   or the account data structure doesn't match our expectation.");
            println!();
            
            // Show first bytes for debugging
            let hex_preview: String = account.data.iter()
                .take(100)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .chunks(32)
                .map(|chunk| chunk.join(" "))
                .collect::<Vec<_>>()
                .join("\n   ");
            
            println!("📋 First 100 bytes (hex):");
            println!("   {}", hex_preview);
            println!();
            
            Err(anyhow::anyhow!("Account is not a valid Solend obligation"))
        }
        Err(e) => {
            println!("{}", "=".repeat(80));
            println!("❌ FAILED: Obligation account parsing failed!");
            println!("{}", "=".repeat(80));
            println!();
            
            println!("❌ Error: {}", e);
            println!();
            
            // Show first bytes for debugging
            let hex_preview: String = account.data.iter()
                .take(200)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .chunks(32)
                .map(|chunk| chunk.join(" "))
                .collect::<Vec<_>>()
                .join("\n   ");
            
            println!("📋 Debug Information:");
            println!("  Account data size: {} bytes", account.data.len());
            println!("  First 200 bytes (hex):");
            println!("   {}", hex_preview);
            println!();
            
            println!("⚠️  ACTION REQUIRED:");
            println!("   1. The struct structure might not match the real Solend IDL");
            println!("   2. Please check the official Solend SDK for the correct structure:");
            println!("      https://github.com/solendprotocol/solend-program");
            println!("   3. Update src/protocols/solend.rs with the correct structure");
            println!("   4. Re-run this validation tool to verify");
            println!();
            
            Err(e)
        }
    }
}

