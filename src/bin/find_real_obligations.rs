// Script to find real obligation accounts on Solend mainnet for testing
// Usage: cargo run --bin find_real_obligations

use anyhow::{Context, Result};
use dotenv::dotenv;
use liquid_bot::blockchain::rpc_client::RpcClient;
use liquid_bot::core::config::Config;
use liquid_bot::protocol::solend::types::SolendObligation;
use liquid_bot::protocol::solend::SolendProtocol;
use liquid_bot::protocol::Protocol;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let config = Config::from_env()
        .context("Failed to load config from .env")?;

    let rpc_client = Arc::new(
        RpcClient::new(config.rpc_http_url.clone())
            .context("Failed to create RPC client")?
    );

    let solend_program_id = Pubkey::from_str(&config.solend_program_id)
        .context("Invalid Solend program ID")?;

    let protocol: Arc<dyn Protocol> = Arc::new(
        SolendProtocol::new(&config)
            .context("Failed to initialize Solend protocol")?
    );

    println!("🔍 Searching for real obligation accounts on Solend mainnet...");
    println!("Program ID: {}", solend_program_id);
    println!();

    use solana_client::rpc_filter::RpcFilterType;
    const OBLIGATION_DATA_SIZE: u64 = 1300;
    
    println!("Fetching program accounts with data size filter ({} bytes)...", OBLIGATION_DATA_SIZE);
    
    let accounts_result = rpc_client.get_program_accounts_with_filters(
        &solend_program_id,
        vec![RpcFilterType::DataSize(OBLIGATION_DATA_SIZE)],
    ).await;

    let accounts = match accounts_result {
        Ok(accounts) => {
            println!("✅ Found {} obligation accounts", accounts.len());
            accounts
        }
        Err(e) => {
            let error_str = e.to_string();
            if error_str.contains("scan aborted") || error_str.contains("exceeded the limit") || error_str.contains("timeout") {
                eprintln!("⚠️  RPC limit hit (expected for large programs). Trying to fetch first batch...");
                // Try to get at least some accounts
                return Err(anyhow::anyhow!("RPC limit hit. Please use a premium RPC or try again later."));
            } else {
                return Err(e).context("Failed to fetch program accounts");
            }
        }
    };

    println!();
    println!("Analyzing obligations...");
    println!("{}", "=".repeat(100));

    let mut found_liquidatable = Vec::new();
    let mut found_with_usdc_debt = Vec::new();
    let mut found_with_debt = Vec::new();
    let mut found_healthy = Vec::new();
    let mut parse_errors = 0;

    const MAX_TO_CHECK: usize = 500; // Check first 500 accounts
    let check_count = accounts.len().min(MAX_TO_CHECK);

    for (i, (pubkey, account)) in accounts.iter().take(check_count).enumerate() {
        if (i + 1) % 50 == 0 {
            println!("Processed {}/{} accounts...", i + 1, check_count);
        }

        // Try direct parsing first (more reliable)
        match SolendObligation::from_account_data(&account.data) {
            Ok(obligation) => {
                let health_factor = obligation.calculate_health_factor();
                let deposited = obligation.total_deposited_value_usd();
                let borrowed = obligation.total_borrowed_value_usd();
                let has_borrows = !obligation.borrows.is_empty();
                let has_deposits = !obligation.deposits.is_empty();

                // Check for USDC debt
                let usdc_mint = Pubkey::from_str(&config.usdc_mint).ok();
                let has_usdc_debt = usdc_mint.map(|usdc| {
                    obligation.borrows.iter().any(|b| {
                        // We need to check reserve to determine mint - for now, check if we can parse position
                        false // Will check via protocol parse_position
                    })
                }).unwrap_or(false);

                // Try protocol parsing for better debt/collateral info
                if let Some(position) = protocol.parse_position(account, Some(Arc::clone(&rpc_client))).await {
                    let has_usdc_debt = position.debt_assets.iter().any(|d| d.mint.to_string() == config.usdc_mint);
                    let has_any_debt = !position.debt_assets.is_empty();
                    let has_collateral = !position.collateral_assets.is_empty();

                    if health_factor < config.hf_liquidation_threshold && has_any_debt && has_collateral {
                        found_liquidatable.push((pubkey.clone(), position.clone()));
                    } else if has_usdc_debt {
                        found_with_usdc_debt.push((pubkey.clone(), position.clone()));
                    } else if has_any_debt {
                        found_with_debt.push((pubkey.clone(), position.clone()));
                    } else if health_factor >= 1.0 {
                        found_healthy.push((pubkey.clone(), position.clone()));
                    }
                } else {
                    // Use direct parse results
                    if health_factor < config.hf_liquidation_threshold as f64 && has_borrows && has_deposits {
                        // Store basic info
                        found_liquidatable.push((pubkey.clone(), {
                            // Create a minimal Position for storage
                            use liquid_bot::core::types::Position;
                            Position {
                                address: *pubkey,
                                health_factor,
                                collateral_usd: deposited,
                                debt_usd: borrowed,
                                collateral_assets: vec![],
                                debt_assets: vec![],
                            }
                        }));
                    } else if has_borrows {
                        found_with_debt.push((pubkey.clone(), {
                            use liquid_bot::core::types::Position;
                            Position {
                                address: *pubkey,
                                health_factor,
                                collateral_usd: deposited,
                                debt_usd: borrowed,
                                collateral_assets: vec![],
                                debt_assets: vec![],
                            }
                        }));
                    } else if health_factor >= 1.0 {
                        found_healthy.push((pubkey.clone(), {
                            use liquid_bot::core::types::Position;
                            Position {
                                address: *pubkey,
                                health_factor,
                                collateral_usd: deposited,
                                debt_usd: borrowed,
                                collateral_assets: vec![],
                                debt_assets: vec![],
                            }
                        }));
                    }
                }
            }
            Err(e) => {
                parse_errors += 1;
                if parse_errors <= 5 {
                    log::debug!("Failed to parse obligation {}: {}", pubkey, e);
                }
            }
        }
    }

    println!();
    println!("{}", "=".repeat(100));
    println!("📊 RESULTS");
    println!("{}", "=".repeat(100));
    println!();

    // Print liquidatable obligations
    if !found_liquidatable.is_empty() {
        println!("🎯 LIQUIDATABLE OBLIGATIONS (Health Factor < {}):", config.hf_liquidation_threshold);
        println!("{}", "-".repeat(100));
        for (pubkey, position) in found_liquidatable.iter().take(5) {
            println!("✅ {}", pubkey);
            println!("   Health Factor: {:.6}", position.health_factor);
            println!("   Deposited: ${:.2}", position.collateral_usd);
            println!("   Borrowed: ${:.2}", position.debt_usd);
            println!("   Debt Assets: {}", position.debt_assets.len());
            for (i, debt) in position.debt_assets.iter().enumerate() {
                println!("     [{}] Mint: {}, Amount: {} (${:.2})", i, debt.mint, debt.amount, debt.amount_usd);
            }
            println!("   Collateral Assets: {}", position.collateral_assets.len());
            for (i, coll) in position.collateral_assets.iter().enumerate() {
                println!("     [{}] Mint: {}, Amount: {} (${:.2})", i, coll.mint, coll.amount, coll.amount_usd);
            }
            println!();
        }
        if found_liquidatable.len() > 5 {
            println!("   ... and {} more liquidatable obligations", found_liquidatable.len() - 5);
        }
        println!();
    }

    // Print obligations with USDC debt
    if !found_with_usdc_debt.is_empty() {
        println!("💰 OBLIGATIONS WITH USDC DEBT:");
        println!("{}", "-".repeat(100));
        for (pubkey, position) in found_with_usdc_debt.iter().take(5) {
            println!("✅ {}", pubkey);
            println!("   Health Factor: {:.6}", position.health_factor);
            println!("   USDC Debt: ${:.2}", position.debt_assets.iter()
                .find(|d| d.mint.to_string() == config.usdc_mint)
                .map(|d| d.amount_usd)
                .unwrap_or(0.0));
            println!();
        }
        if found_with_usdc_debt.len() > 5 {
            println!("   ... and {} more obligations with USDC debt", found_with_usdc_debt.len() - 5);
        }
        println!();
    }

    // Print obligations with any debt
    if !found_with_debt.is_empty() && found_liquidatable.is_empty() && found_with_usdc_debt.is_empty() {
        println!("💳 OBLIGATIONS WITH DEBT (non-USDC):");
        println!("{}", "-".repeat(100));
        for (pubkey, position) in found_with_debt.iter().take(3) {
            println!("✅ {}", pubkey);
            println!("   Health Factor: {:.6}", position.health_factor);
            println!("   Debt: ${:.2}", position.debt_usd);
            println!();
        }
        println!();
    }

    // Summary
    println!("{}", "=".repeat(100));
    println!("📈 SUMMARY:");
    println!("   Total accounts checked: {}", check_count);
    println!("   Liquidatable obligations: {}", found_liquidatable.len());
    println!("   Obligations with USDC debt: {}", found_with_usdc_debt.len());
    println!("   Obligations with any debt: {}", found_with_debt.len());
    println!("   Healthy obligations: {}", found_healthy.len());
    println!("   Parse errors: {}", parse_errors);
    println!();

    // Recommendations
    if !found_liquidatable.is_empty() {
        println!("💡 RECOMMENDATION:");
        println!("   Add this to your .env file:");
        println!("   TEST_OBLIGATION_PUBKEY={}", found_liquidatable[0].0);
        println!();
    } else if !found_with_usdc_debt.is_empty() {
        println!("💡 RECOMMENDATION:");
        println!("   Add this to your .env file (for Jupiter API test):");
        println!("   TEST_OBLIGATION_PUBKEY={}", found_with_usdc_debt[0].0);
        println!();
    } else {
        println!("⚠️  No suitable obligations found in first {} accounts.", check_count);
        println!("   Try running with a premium RPC or increase MAX_TO_CHECK.");
        println!();
    }

    Ok(())
}

