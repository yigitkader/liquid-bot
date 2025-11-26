use anyhow::Result;
use std::{env, str::FromStr, sync::Arc};
use solana_sdk::pubkey::Pubkey;
use crate::solana_client::SolanaClient;
use crate::protocols::reserve_validator;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Vec<String> = env::args().collect();
    let arg = args.get(1).cloned().unwrap_or_else(|| {
        eprintln!("Usage: validate_reserve <RESERVE_PUBKEY> | --usdc | --sol");
        std::process::exit(1);
    });

    // Reserve adresini argümandan ya da known_reserves'den çöz
    let reserve_pubkey = if arg == "--usdc" {
        reserve_validator::known_reserves::usdc_reserve()?
    } else if arg == "--sol" {
        reserve_validator::known_reserves::sol_reserve()?
    } else {
        Pubkey::from_str(&arg)?
    };

    let rpc_url = env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let rpc = Arc::new(SolanaClient::new(rpc_url)?);

    let result = reserve_validator::validate_reserve_structure(rpc, &reserve_pubkey).await?;

    println!("reserve: {}", reserve_pubkey);
    println!("success: {}", result.success);

    if let Some(err) = result.error {
        println!("error: {}", err);
    }

    if let Some(info) = result.reserve_info {
        println!("version: {}", info.version);
        println!("lending_market: {}", info.lending_market);
        println!("liquidity_mint: {}", info.liquidity_mint);
        println!("collateral_mint: {}", info.collateral_mint);
        println!("ltv: {}", info.ltv);
        println!("liquidation_bonus: {}", info.liquidation_bonus);
    }

    Ok(())
}