// src/bin/create_atas.rs
// Kullanım: cargo run --bin create_atas

use anyhow::{Context, Result};
use dotenv::dotenv;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use liquid_bot::blockchain::rpc_client::RpcClient;
use liquid_bot::core::config::Config;
use liquid_bot::protocol::solend::accounts::get_associated_token_address;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::init();

    println!("🔧 Creating ATAs for bot wallet...\n");

    let config = Config::from_env().context("Failed to load config")?;

    let rpc = Arc::new(
        RpcClient::new(config.rpc_http_url.clone()).context("Failed to create RPC client")?,
    );

    let wallet = load_wallet(&config.wallet_path).context("Failed to load wallet")?;
    let wallet_pubkey = wallet.pubkey();

    println!("Wallet: {}\n", wallet_pubkey);

    // Token mint'ler
    let mints = vec![
        ("USDC", config.usdc_mint.as_str()),
        ("SOL", config.sol_mint.as_str()),
        (
            "USDT",
            config.usdt_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
        ),
        (
            "ETH",
            config.eth_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
        ),
        (
            "BTC",
            config.btc_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
        ),
    ];

    let mut instructions = Vec::new();
    let mut total_atas = 0;

    for (name, mint_str) in mints {
        if mint_str.is_empty() {
            continue;
        }

        let mint = Pubkey::from_str(mint_str).with_context(|| format!("Invalid {} mint", name))?;

        let ata = get_associated_token_address(&wallet_pubkey, &mint, Some(&config))
            .with_context(|| format!("Failed to derive ATA for {}", name))?;

        match rpc.get_account(&ata).await {
            Ok(_) => {
                println!("✅ {} ATA already exists: {}", name, ata);
            }
            Err(_) => {
                println!("❌ {} ATA not found: {}, creating...", name, ata);

                let create_ata_ix = create_associated_token_account(
                    &wallet_pubkey,
                    &wallet_pubkey,
                    &mint,
                    &spl_token::id(),
                    Some(&config),
                )?;

                instructions.push(create_ata_ix);
                total_atas += 1;
            }
        }
    }

    if instructions.is_empty() {
        println!("\n🎉 All ATAs already exist, nothing to do!");
        return Ok(());
    }

    println!("\n📝 Creating {} ATA(s)...", total_atas);

    let recent_blockhash = rpc.get_recent_blockhash().await?;
    let mut tx = Transaction::new_with_payer(&instructions, Some(&wallet_pubkey));
    tx.sign(&[&wallet], recent_blockhash);

    match rpc.send_transaction(&tx).await {
        Ok(sig) => {
            println!("✅ Transaction sent: {}", sig);
            println!("\n🎉 All ATAs created successfully!");
        }
        Err(e) => {
            println!("❌ Transaction failed: {}", e);
            println!("\nYou can manually create ATAs using:");
            println!("  spl-token create-account <MINT> --owner <WALLET>");
            return Err(e);
        }
    }

    Ok(())
}

fn load_wallet(path: &str) -> Result<Keypair> {
    let wallet_path = Path::new(path);

    if !wallet_path.exists() {
        return Err(anyhow::anyhow!("Wallet file not found: {}", path));
    }

    let keypair_bytes = fs::read(wallet_path).context("Failed to read wallet file")?;

    if let Ok(keypair) = serde_json::from_slice::<Vec<u8>>(&keypair_bytes) {
        if keypair.len() == 64 {
            return Keypair::from_bytes(&keypair)
                .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
        }
    }

    if keypair_bytes.len() == 64 {
        return Keypair::from_bytes(&keypair_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
    }

    if let Ok(keypair_str) = String::from_utf8(keypair_bytes.clone()) {
        if let Ok(keypair_bytes_decoded) = bs58::decode(keypair_str.trim()).into_vec() {
            if keypair_bytes_decoded.len() == 64 {
                return Keypair::from_bytes(&keypair_bytes_decoded)
                    .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
            }
        }
    }

    Err(anyhow::anyhow!(
        "Invalid wallet format: expected 64 bytes, JSON array, or base58 string"
    ))
}

fn create_associated_token_account(
    payer: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    config: Option<&Config>,
) -> Result<Instruction> {
    use liquid_bot::protocol::solend::accounts::get_associated_token_program_id;

    let associated_token_program = get_associated_token_program_id(config)?;

    let ata = Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &associated_token_program,
    )
    .0;

    Ok(Instruction {
        program_id: associated_token_program,
        accounts: vec![
            solana_sdk::instruction::AccountMeta::new(*payer, true),
            solana_sdk::instruction::AccountMeta::new(ata, false),
            solana_sdk::instruction::AccountMeta::new_readonly(*wallet, false),
            solana_sdk::instruction::AccountMeta::new_readonly(*mint, false),
            solana_sdk::instruction::AccountMeta::new_readonly(
                solana_sdk::system_program::id(),
                false,
            ),
            solana_sdk::instruction::AccountMeta::new_readonly(*token_program, false),
        ],
        data: vec![],
    })
}
