/// Binary to validate all external dependencies
/// 
/// This binary checks that:
/// - Solend IDL is valid and parseable
/// - Instruction discriminators match IDL expectations
/// - Account order matches IDL
/// - Oracle SDKs (Pyth, Switchboard) work correctly
/// - Solana SDK functions work correctly

use anyhow::{Context, Result};
use dotenv::dotenv;
use liquid_bot::blockchain::rpc_client::RpcClient;
use liquid_bot::core::config::Config;
use liquid_bot::utils::dependency_validator::validate_all_dependencies;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    println!("🔍 Validating external dependencies...\n");

    let config = Config::from_env().context("Failed to load configuration")?;
    let rpc = Arc::new(
        RpcClient::new(config.rpc_http_url.clone())
            .context("Failed to create RPC client")?,
    );

    let results = validate_all_dependencies(rpc, &config).await;

    println!("📊 Validation Results:\n");
    println!("{:-<80}", "");

    let mut success_count = 0;
    let mut fail_count = 0;

    for result in &results {
        let status = if result.success {
            "✅"
        } else {
            "❌"
        };
        println!("{} {}", status, result.name);
        println!("   {}", result.message);
        if let Some(details) = &result.details {
            println!("   Details: {}", details);
        }
        println!();

        if result.success {
            success_count += 1;
        } else {
            fail_count += 1;
        }
    }

    println!("{:-<80}", "");
    println!(
        "Summary: {} passed, {} failed (out of {} total)",
        success_count,
        fail_count,
        results.len()
    );

    if fail_count > 0 {
        println!("\n⚠️  Some validations failed. Please review the errors above.");
        std::process::exit(1);
    } else {
        println!("\n✅ All dependencies validated successfully!");
        Ok(())
    }
}

