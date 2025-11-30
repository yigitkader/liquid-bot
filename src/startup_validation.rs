//! Startup Validation Module
//!
//! This module performs critical validation checks before the bot starts.
//! It ensures all structs, protocols, and configurations are compatible with
//! real-world Solend mainnet data.
//!
//! These validations are MANDATORY and must pass before the bot can run.
//! If any validation fails, the bot will exit with an error.

use crate::config::Config;
use crate::protocols::reserve_validator;
use crate::solana_client::SolanaClient;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

/// Performs all critical startup validations
/// Returns Ok(()) if all validations pass, Err if any critical validation fails
pub async fn validate_startup(config: &Config, rpc_client: &Arc<SolanaClient>) -> Result<()> {
    log::info!("🔍 Running startup validation checks...");
    log::info!("{}", "=".repeat(80));

    // 1. Reserve Struct Validation (CRITICAL)
    log::info!("1️⃣  Validating Reserve Struct (CRITICAL)...");
    validate_reserve_struct(rpc_client, config).await?;
    log::info!("✅ Reserve struct validation passed");

    // 2. RPC Connection Validation
    log::info!("2️⃣  Validating RPC Connection...");
    validate_rpc_connection(rpc_client).await?;
    log::info!("✅ RPC connection validation passed");

    // 3. Protocol Initialization Validation
    log::info!("3️⃣  Validating Protocol Initialization...");
    validate_protocol(config).await?;
    log::info!("✅ Protocol initialization validation passed");

    log::info!("{}", "=".repeat(80));
    log::info!("✅ All startup validations passed! Bot is ready to run.");
    log::info!("{}", "=".repeat(80));
    log::info!("");

    Ok(())
}

/// Validates that the Reserve struct can parse real mainnet reserve accounts
/// This is CRITICAL - if this fails, the bot cannot function correctly
async fn validate_reserve_struct(
    rpc_client: &Arc<SolanaClient>,
    config: &Config,
) -> Result<()> {
    // Use known USDC reserve address (mainnet)
    let usdc_reserve_str = config
        .usdc_reserve_address
        .as_ref()
        .map(|s| s.as_str())
        .unwrap_or("BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw");

    let reserve_pubkey = usdc_reserve_str
        .parse::<Pubkey>()
        .context("Invalid USDC reserve address format")?;

    log::info!("   Testing with USDC reserve: {}", reserve_pubkey);

    // Validate reserve structure
    let result = reserve_validator::validate_reserve_structure(
        Arc::clone(rpc_client),
        &reserve_pubkey,
        Some(config),
    )
    .await
    .context("Failed to validate reserve structure")?;

    if !result.success {
        let error_msg = result.error.unwrap_or_else(|| "Unknown error".to_string());
        log::error!("❌ Reserve struct validation FAILED!");
        log::error!("   Error: {}", error_msg);
        log::error!("");
        log::error!("   ⚠️  CRITICAL: The reserve struct in src/protocols/solend_reserve.rs");
        log::error!("      does not match the real Solend mainnet data!");
        log::error!("");
        log::error!("   ACTION REQUIRED:");
        log::error!("   1. Check src/protocols/solend_reserve.rs struct definition");
        log::error!("   2. Compare with official Solend source code:");
        log::error!("      https://github.com/solendprotocol/solana-program-library/blob/master/token-lending/program/src/state/reserve.rs");
        log::error!("   3. Run: cargo run --bin validate_reserve -- --reserve {}", reserve_pubkey);
        log::error!("   4. Fix the struct definition and re-run validation");
        log::error!("");
        return Err(anyhow::anyhow!(
            "Reserve struct validation failed - struct does not match real mainnet data"
        ));
    }

    if let Some(reserve_info) = result.reserve_info {
        log::info!("   ✅ Reserve parsed successfully:");
        log::info!("      Version: {}", reserve_info.version);
        log::info!("      Liquidity Mint: {}", reserve_info.liquidity_mint);
        log::info!("      Pyth Oracle: {:?}", reserve_info.pyth_oracle);
        log::info!("      Switchboard Oracle: {:?}", reserve_info.switchboard_oracle);
    }

    Ok(())
}

/// Validates RPC connection is working
async fn validate_rpc_connection(rpc_client: &Arc<SolanaClient>) -> Result<()> {
    // Test RPC connection by getting recent blockhash
    let blockhash = rpc_client
        .get_recent_blockhash()
        .await
        .context("Failed to get recent blockhash from RPC")?;

    log::info!("   ✅ RPC connection working (recent blockhash: {})", blockhash);

    Ok(())
}

/// Validates protocol can be initialized
async fn validate_protocol(config: &Config) -> Result<()> {
    use crate::protocol::Protocol;
    use crate::protocols::solend::SolendProtocol;

    let protocol = SolendProtocol::new_with_config(config)
        .context("Failed to initialize Solend protocol")?;

    log::info!("   ✅ Protocol initialized:");
    log::info!("      Protocol ID: {}", protocol.id());
    log::info!("      Program ID: {}", protocol.program_id());

    Ok(())
}

