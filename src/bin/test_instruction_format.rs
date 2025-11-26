//! Test Solend Liquidation Instruction Format
//!
//! Bu binary, gerçek bir obligation account'u kullanarak liquidation instruction'ı oluşturur
//! ve instruction data format'ını kontrol eder.

use anyhow::{Context, Result};
use clap::Parser;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

use liquid_bot::solana_client::SolanaClient;
use liquid_bot::protocols::solend::SolendProtocol;
use liquid_bot::protocol::Protocol;
use liquid_bot::domain::LiquidationOpportunity;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(name = "test_instruction_format")]
#[command(about = "Tests Solend liquidation instruction format with real accounts")]
struct Args {
    /// RPC URL
    #[arg(long, default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,

    /// Obligation account pubkey (optional - will try to find one if not provided)
    #[arg(long)]
    obligation: Option<String>,

    /// Test liquidator pubkey (for testing only)
    #[arg(long, default_value = "11111111111111111111111111111111")]
    liquidator: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_secs()
        .init();

    let args = Args::parse();

    log::info!("🧪 Testing Solend Liquidation Instruction Format");
    log::info!("RPC URL: {}", args.rpc_url);

    let rpc_client = Arc::new(
        SolanaClient::new(args.rpc_url.clone())
            .context("Failed to create RPC client")?
    );

    let protocol = SolendProtocol::new()
        .context("Failed to create Solend protocol")?;

    // Obligation account is required for testing
    let obligation_pubkey = args.obligation
        .ok_or_else(|| anyhow::anyhow!("--obligation parameter is required. Find one from Solscan or Solend explorer"))?
        .parse::<Pubkey>()
        .context("Invalid obligation pubkey")?;

    log::info!("Using obligation: {}", obligation_pubkey);

    // Get obligation account data
    let account = rpc_client.get_account(&obligation_pubkey).await
        .context("Failed to fetch obligation account")?;

    // Parse as position
    let position = protocol.parse_account_position(&obligation_pubkey, &account, Some(Arc::clone(&rpc_client))).await?
        .ok_or_else(|| anyhow::anyhow!("Account is not a valid obligation"))?;

    log::info!("✅ Obligation parsed successfully");
    log::info!("   Health Factor: {:.4}", position.health_factor);
    log::info!("   Collateral: ${:.2}, Debt: ${:.2}", 
              position.total_collateral_usd, position.total_debt_usd);

    // Create a mock liquidation opportunity
    let opportunity = LiquidationOpportunity {
        account_position: position.clone(),
        max_liquidatable_amount: 1000, // Test amount
        seizable_collateral: 1000,
        liquidation_bonus: 0.05,
        estimated_profit_usd: 10.0,
        target_debt_mint: position.debt_assets.first()
            .map(|d| d.mint.clone())
            .ok_or_else(|| anyhow::anyhow!("No debt assets"))?,
        target_collateral_mint: position.collateral_assets.first()
            .map(|c| c.mint.clone())
            .ok_or_else(|| anyhow::anyhow!("No collateral assets"))?,
    };

    // Parse liquidator pubkey
    let liquidator = args.liquidator.parse::<Pubkey>()
        .context("Invalid liquidator pubkey")?;

    // Build instruction
    log::info!("\n🔨 Building liquidation instruction...");
    let instruction = protocol.build_liquidation_instruction(
        &opportunity,
        &liquidator,
        Some(Arc::clone(&rpc_client)),
    ).await
    .context("Failed to build liquidation instruction")?;

    // Analyze instruction data
    println!("\n{}", "=".repeat(80));
    println!("📊 INSTRUCTION DATA ANALYSIS");
    println!("{}", "=".repeat(80));
    println!("\nInstruction data length: {} bytes", instruction.data.len());
    println!("Instruction data (hex): {}", 
             instruction.data.iter().map(|b| format!("{:02x}", b)).collect::<String>());
    println!("Instruction data (bytes): {:?}", instruction.data);
    
    // Check format
    if instruction.data.len() == 16 {
        let discriminator = &instruction.data[0..8];
        let amount_bytes = &instruction.data[8..];
        let amount = u64::from_le_bytes(amount_bytes.try_into().unwrap());
        
        println!("\n✅ Format: Anchor discriminator (Solend SDK format)");
        println!("   Discriminator: {:02x?}", discriminator);
        println!("   Discriminator hex: {}", discriminator.iter().map(|b| format!("{:02x}", b)).collect::<String>());
        println!("   Amount: {} (0x{:016x})", amount, amount);
        
        // Verify discriminator matches sha256("global:liquidateObligation")[:8]
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(b"global:liquidateObligation");
        let expected_discriminator: [u8; 8] = hasher.finalize()[0..8].try_into().unwrap();
        
        if discriminator == expected_discriminator.as_slice() {
            println!("   ✅ Discriminator matches sha256(\"global:liquidateObligation\")[:8]");
        } else {
            println!("   ⚠️  Discriminator mismatch!");
            println!("      Expected: {:02x?}", expected_discriminator);
            println!("      Got:      {:02x?}", discriminator);
        }
    } else if instruction.data.len() == 9 {
        let instruction_index = instruction.data[0];
        let amount_bytes = &instruction.data[1..];
        let amount = u64::from_le_bytes(amount_bytes.try_into().unwrap());
        
        println!("\n⚠️  Format: SPL token-lending (u8 index + u64 amount) - OLD FORMAT");
        println!("   Instruction index: {} (0x{:02x})", instruction_index, instruction_index);
        println!("   Amount: {} (0x{:016x})", amount, amount);
        println!("   ⚠️  This format is incorrect! Should use Anchor discriminator (16 bytes)");
    } else {
        println!("\n❌ Unknown format: {} bytes (expected 16 bytes for Anchor discriminator)", instruction.data.len());
    }

    // Check accounts
    println!("\n{}", "=".repeat(80));
    println!("📋 ACCOUNTS");
    println!("{}", "=".repeat(80));
    println!("Total accounts: {}", instruction.accounts.len());
    for (i, account_meta) in instruction.accounts.iter().enumerate() {
        println!("  {}. {} (writable: {}, signer: {})", 
                i + 1,
                account_meta.pubkey,
                account_meta.is_writable,
                account_meta.is_signer);
    }

    println!("\n✅ Instruction created successfully!");
    println!("   Program ID: {}", instruction.program_id);
    println!("   Data length: {} bytes", instruction.data.len());
    println!("   Account count: {}", instruction.accounts.len());

    Ok(())
}

