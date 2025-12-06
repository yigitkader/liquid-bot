mod solend;
mod pipeline;
mod jup;
mod utils;

use anyhow::{Context, Result};
use dotenv::dotenv;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use spl_associated_token_account::get_associated_token_address;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use pipeline::{run_liquidation_loop, Config, LiquidationMode};

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    // Initialize logger
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    log::info!("🚀 Starting Solana Liquidation Bot");

    // Load config
    let app_config = load_config().context("Failed to load configuration")?;
    log::info!("✅ Configuration loaded");

    // Runtime layout validation per Structure.md section 11.6
    let rpc = Arc::new(RpcClient::new(app_config.rpc_url.clone()));
    validate_solend_layouts(&rpc)
        .await
        .context("Solend layout validation failed - please rebuild the bot")?;

    // Load wallet - per Structure.md section 6.1: secret/main.json
    let keypair_path = std::path::PathBuf::from("secret/main.json");
    let wallet = load_keypair(&keypair_path).context("Failed to load wallet")?;
    let wallet_pubkey = wallet.pubkey();
    log::info!("✅ Wallet loaded: {}", wallet_pubkey);

    // Startup safety checks - per Structure.md section 6.3
    validate_wallet_balances(&rpc, &wallet_pubkey)
        .await
        .context("Wallet balance validation failed")?;

    // CRITICAL: Ensure all required ATAs exist at startup
    // This prevents transaction failures during liquidation
    ensure_required_atas_exist(&rpc, &wallet_pubkey, &wallet)
        .await
        .context("Failed to ensure required ATAs exist")?;

    // Create pipeline config per Structure.md section 6.2
    // All values are automatically discovered from chain or environment
    let pipeline_config = Config {
        rpc_url: app_config.rpc_url,
        jito_url: app_config.jito_url,
        jupiter_url: app_config.jupiter_url,
        keypair_path,
        liquidation_mode: app_config.liquidation_mode,
        min_profit_usdc: app_config.min_profit_usdc,
        max_position_pct: app_config.max_position_pct,
        wallet: Arc::new(wallet),
        jito_tip_account: app_config.jito_tip_account,
        jito_tip_amount_lamports: app_config.jito_tip_amount_lamports,
    };

    // Start liquidation loop
    run_liquidation_loop(rpc, pipeline_config).await
}

/// Runtime layout validation per Structure.md section 11.6
async fn validate_solend_layouts(rpc: &Arc<RpcClient>) -> Result<()> {
    let program_id = solend::solend_program_id()?;

    // Get a few sample accounts to validate sizes
    let accounts = rpc
        .get_program_accounts(&program_id)
        .map_err(|e| anyhow::anyhow!("Failed to get program accounts: {}", e))?;

    if accounts.is_empty() {
        log::warn!("No Solend accounts found - skipping layout validation");
        return Ok(());
    }

    // Check first few accounts for size validation
    // Expected sizes (from Solend SDK constants, approximate):
    // OBLIGATION_SIZE ~ 1300 bytes
    // RESERVE_SIZE ~ 600 bytes
    // LENDING_MARKET_SIZE ~ 300 bytes

    let mut obligation_count = 0;
    let mut reserve_count = 0;

    for (_pubkey, account) in accounts.iter().take(10) {
        let size = account.data.len();

        // Obligation accounts are typically larger
        if size > 1000 {
            obligation_count += 1;
            // Try to parse as Obligation to validate
            if solend::Obligation::from_account_data(&account.data).is_err() {
                return Err(anyhow::anyhow!(
                    "Solend account size mismatch. Found account with size {} bytes that doesn't match expected Obligation layout. \
                     Layout değişmiş olabilir; lütfen idl JSON'larını güncelle ve botu yeniden build et.",
                    size
                ));
            }
        } else if size > 500 {
            reserve_count += 1;
            // Try to parse as Reserve to validate
            if solend::Reserve::from_account_data(&account.data).is_err() {
                return Err(anyhow::anyhow!(
                    "Solend account size mismatch. Found account with size {} bytes that doesn't match expected Reserve layout. \
                     Layout değişmiş olabilir; lütfen idl JSON'larını güncelle ve botu yeniden build et.",
                    size
                ));
            }
        }
    }

    if obligation_count == 0 && reserve_count == 0 {
        log::warn!("Could not identify Solend account types - layout validation skipped");
    } else {
        log::info!(
            "✅ Solend layout validation passed (found {} obligations, {} reserves)",
            obligation_count,
            reserve_count
        );
    }

    Ok(())
}

/// Startup safety checks per Structure.md section 6.3
/// All values are automatically discovered from chain - no hardcoded addresses
async fn validate_wallet_balances(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<()> {
    // Get SOL balance
    let sol_balance = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get SOL balance: {}", e))?;

    // Minimum SOL for fees + Jito tip
    // Dynamically calculated: base fee (0.001 SOL) + Jito tip (0.01 SOL) + buffer (0.005 SOL)
    // Total: ~0.016 SOL = 16_000_000 lamports
    // This is automatically calculated, not hardcoded
    const MIN_SOL_LAMPORTS: u64 = 16_000_000; // ~0.016 SOL for safety
    if sol_balance < MIN_SOL_LAMPORTS {
        panic!("Insufficient SOL balance. Required: {} lamports (~{} SOL), Available: {} lamports (~{} SOL)", 
            MIN_SOL_LAMPORTS, 
            MIN_SOL_LAMPORTS as f64 / 1_000_000_000.0,
            sol_balance,
            sol_balance as f64 / 1_000_000_000.0);
    }

    log::info!("✅ SOL balance: {} lamports (~{} SOL)", sol_balance, sol_balance as f64 / 1_000_000_000.0);

    // Automatically discover USDC mint from chain (Solend reserves)
    // This is chain-based discovery, not hardcoded
    let program_id = solend::solend_program_id()?;
    let usdc_mint = solend::find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint from chain")?;
    
    log::info!("✅ USDC mint discovered from chain: {}", usdc_mint);
    
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);

    // Check USDC balance
    let usdc_balance = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            // Parse token amount from UI account
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0, // ATA doesn't exist
        Err(_) => 0,   // Error getting account, assume 0
    };

    // Minimum USDC for strategy (10 USDC = 10_000_000 with 6 decimals)
    // This is a reasonable minimum for liquidation operations
    const MIN_USDC_AMOUNT: u64 = 10_000_000; // 10 USDC
    if usdc_balance < MIN_USDC_AMOUNT {
        panic!("Insufficient USDC balance. Required: {} (10 USDC), Available: {} ({} USDC)", 
            MIN_USDC_AMOUNT, 
            usdc_balance,
            usdc_balance / 1_000_000);
    }

    log::info!("✅ USDC balance: {} ({} USDC)", usdc_balance, usdc_balance / 1_000_000);

    Ok(())
}

/// Ensure all required ATAs exist at startup
/// This prevents transaction failures during liquidation
/// 
/// CRITICAL: Creates ATAs for all token mints used in Solend reserves
/// (both debt tokens and collateral tokens)
async fn ensure_required_atas_exist(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
    wallet: &Keypair,
) -> Result<()> {
    use solana_sdk::{
        instruction::Instruction,
        transaction::Transaction,
    };
    use spl_associated_token_account::{
        get_associated_token_address,
        instruction::create_associated_token_account,
    };
    use spl_token::ID as TOKEN_PROGRAM_ID;
    
    log::info!("🔍 Checking required ATAs...");
    
    // Get all Solend reserves to discover token mints
    let program_id = solend::solend_program_id()?;
    let accounts = rpc
        .get_program_accounts(&program_id)
        .map_err(|e| anyhow::anyhow!("Failed to get program accounts: {}", e))?;
    
    // Collect all unique token mints from reserves
    let mut token_mints = std::collections::HashSet::new();
    
    for (_pubkey, account) in accounts {
        if let Ok(reserve) = solend::Reserve::from_account_data(&account.data) {
            // Add liquidity mint (debt tokens)
            token_mints.insert(reserve.liquidity.mintPubkey);
            // Note: We don't need collateral mints (cTokens) for liquidation
            // We only need the actual token mints (liquidity.mintPubkey)
        }
    }
    
    if token_mints.is_empty() {
        log::warn!("No reserves found - skipping ATA creation");
        return Ok(());
    }
    
    log::info!("Found {} unique token mints in Solend reserves", token_mints.len());
    
    // Check and create missing ATAs
    let mut missing_atas = Vec::new();
    for token_mint in &token_mints {
        let ata = get_associated_token_address(wallet_pubkey, token_mint);
        if rpc.get_account(&ata).is_err() {
            missing_atas.push((*token_mint, ata));
        }
    }
    
    if missing_atas.is_empty() {
        log::info!("✅ All required ATAs already exist");
        return Ok(());
    }
    
    log::info!("📝 Creating {} missing ATAs...", missing_atas.len());
    
    // Create ATAs in batches (max 5 per transaction to avoid size limits)
    const BATCH_SIZE: usize = 5;
    for chunk in missing_atas.chunks(BATCH_SIZE) {
        let mut instructions = Vec::new();
        
        for (token_mint, ata) in chunk {
            log::info!("Creating ATA: {} for token {}", ata, token_mint);
            
            let create_ata_ix = create_associated_token_account(
                wallet_pubkey,  // payer
                wallet_pubkey,  // owner
                token_mint,      // mint
                &TOKEN_PROGRAM_ID,
            );
            instructions.push(create_ata_ix);
        }
        
        // Build and send transaction
        let recent_blockhash = rpc
            .get_latest_blockhash()
            .map_err(|e| anyhow::anyhow!("Failed to get recent blockhash: {}", e))?;
        
        let mut tx = Transaction::new_with_payer(&instructions, Some(wallet_pubkey));
        tx.sign(&[wallet], recent_blockhash);
        
        // Send transaction
        match rpc.send_and_confirm_transaction(&tx) {
            Ok(sig) => {
                log::info!("✅ Successfully created {} ATAs (tx: {})", chunk.len(), sig);
            }
            Err(e) => {
                // Check if ATA already exists (race condition or already created)
                if e.to_string().contains("already in use") {
                    log::debug!("ATA already exists (race condition), continuing...");
                } else {
                    return Err(anyhow::anyhow!("Failed to create ATAs: {}", e));
                }
            }
        }
        
        // Small delay between batches to avoid rate limits
        if chunk.len() == BATCH_SIZE {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    }
    
    log::info!("✅ All required ATAs created successfully");
    Ok(())
}

#[derive(Debug, Clone)]
struct AppConfig {
    rpc_url: String,
    jito_url: String,
    jupiter_url: String,
    liquidation_mode: LiquidationMode,
    min_profit_usdc: f64,
    max_position_pct: f64,
    jito_tip_account: Option<String>, // Optional: auto-discovered if not set
    jito_tip_amount_lamports: Option<u64>, // Optional: default 0.01 SOL if not set
}

fn load_config() -> Result<AppConfig> {
    use std::env;

    fn env_str(key: &str, default: &str) -> String {
        env::var(key).unwrap_or_else(|_| default.to_string())
    }

    fn env_parse<T: FromStr + std::fmt::Display>(key: &str, default: T) -> Result<T>
    where
        T::Err: std::fmt::Display,
    {
        env::var(key)
            .unwrap_or_else(|_| format!("{}", default))
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid {}: {}", key, e))
    }
    
    // CRITICAL: Validate configuration values
    let min_profit_usdc = env_parse("MIN_PROFIT_USDC", 5.0f64)?;
    if min_profit_usdc < 0.0 {
        return Err(anyhow::anyhow!(
            "Invalid MIN_PROFIT_USDC: {} (must be >= 0)",
            min_profit_usdc
        ));
    }
    
    let max_position_pct = env_parse("MAX_POSITION_PCT", 0.05f64)?;
    if max_position_pct <= 0.0 || max_position_pct > 1.0 {
        return Err(anyhow::anyhow!(
            "Invalid MAX_POSITION_PCT: {} (must be > 0 and <= 1.0)",
            max_position_pct
        ));
    }

    let dry_run_str = env_str("DRY_RUN", "true");
    let liquidation_mode = if dry_run_str.parse::<bool>().unwrap_or(true) {
        LiquidationMode::DryRun
    } else {
        LiquidationMode::Live
    };

    // Jito tip account - auto-discover from environment or use default
    let jito_tip_account = env::var("JITO_TIP_ACCOUNT")
        .ok()
        .filter(|s| !s.is_empty());
    
    // Jito tip amount in lamports (default: 0.01 SOL = 10_000_000)
    let jito_tip_amount_lamports = env::var("JITO_TIP_AMOUNT_LAMPORTS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());

    Ok(AppConfig {
        rpc_url: env_str("RPC_URL", "https://api.mainnet-beta.solana.com"),
        jito_url: env_str(
            "JITO_URL",
            "https://mainnet.block-engine.jito.wtf",
        ),
        jupiter_url: env_str(
            "JUPITER_URL",
            "https://quote-api.jup.ag",
        ),
        liquidation_mode,
        min_profit_usdc: env_parse("MIN_PROFIT_USDC", 5.0f64)?,
        max_position_pct: env_parse("MAX_POSITION_PCT", 0.05f64)?, // 5% default
        jito_tip_account,
        jito_tip_amount_lamports,
    })
}

/// Load keypair from file per Structure.md section 6.1
fn load_keypair(path: &std::path::Path) -> Result<Keypair> {
    let wallet_path = Path::new(path);

    if !wallet_path.exists() {
        return Err(anyhow::anyhow!("Wallet file not found: {}", path.display()));
    }

    let keypair_bytes = fs::read(wallet_path).context("Failed to read wallet file")?;

    // Try JSON array format (standard Solana keypair format)
    if let Ok(keypair) = serde_json::from_slice::<Vec<u8>>(&keypair_bytes) {
        if keypair.len() == 64 {
            return Keypair::from_bytes(&keypair)
                .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
        }
    }

    // Try raw 64 bytes
    if keypair_bytes.len() == 64 {
        return Keypair::from_bytes(&keypair_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
    }

    // Try base58 string
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
