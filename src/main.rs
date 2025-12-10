mod kamino;  // Kamino Lend support
mod lending_trait;  // Common trait for lending protocols
mod pipeline;
mod jup;
mod utils;
mod oracle;
mod liquidation_tx;
mod quotes;
mod wallet;

use anyhow::{Context, Result};
use dotenv::dotenv;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use spl_associated_token_account::get_associated_token_address;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use pipeline::{run_liquidation_loop, Config, LiquidationMode};

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    // Create logs directory if it doesn't exist
    let logs_dir = Path::new("logs");
    if !logs_dir.exists() {
        fs::create_dir_all(logs_dir)
            .context("Failed to create logs directory")?;
    }

    // Generate log filename with timestamp
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let log_filename = format!("logs/liquidation_{}.log", timestamp);
    
    // Custom writer that writes to both file and console
    struct DualWriter {
        file: fs::File,
    }
    
    impl Write for DualWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            // Write to file
            let written = self.file.write(buf)?;
            // Also write to console (stderr for logs)
            // Ignore errors writing to stderr to avoid breaking if console is unavailable
            let _ = std::io::stderr().write_all(&buf[..written]);
            Ok(written)
        }
        
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()?;
            let _ = std::io::stderr().flush();
            Ok(())
        }
    }
    
    let log_file = fs::File::create(&log_filename)
        .context(format!("Failed to create log file: {}", log_filename))?;
    
    let dual_writer = DualWriter { file: log_file };

    // Initialize logger with both console and file output
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .target(env_logger::Target::Pipe(Box::new(dual_writer)))
        .init();

    log::info!("🚀 Starting Solana Liquidation Bot");
    log::info!("📝 Logging to file: {}", log_filename);

    // Load config
    let app_config = load_config().context("Failed to load configuration")?;
    log::info!("✅ Configuration loaded");

    // Runtime layout validation per Structure.md section 11.6
    // CRITICAL: Log RPC URL for debugging (masked for security)
    let rpc_url_display = if app_config.rpc_url.len() > 50 {
        format!("{}...{}", &app_config.rpc_url[..25], &app_config.rpc_url[app_config.rpc_url.len()-25..])
    } else {
        app_config.rpc_url.clone()
    };
    log::info!("🔗 RPC URL: {} (masked)", rpc_url_display);
    log::info!("   Full RPC URL length: {} characters", app_config.rpc_url.len());
    
    // CRITICAL: Configure RPC client with appropriate timeout for large requests
    // get_program_accounts() for 300K+ accounts can take 60+ seconds
    // Default timeout (30s) is too short and causes "error sending request" failures
    let rpc_timeout = std::env::var("RPC_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(120); // Default: 120 seconds for large account fetches
    
    let rpc = Arc::new(RpcClient::new_with_timeout(
        app_config.rpc_url.clone(),
        std::time::Duration::from_secs(rpc_timeout),
    ));
    log::info!("✅ RPC client initialized with {}s timeout", rpc_timeout);
    
    // CRITICAL: Verify we're connected to mainnet, not devnet/testnet
    validate_mainnet_connection(&rpc)
        .await
        .context("Mainnet connection validation failed - this bot only supports mainnet production")?;
    
    // Initialize Pyth Hermes feed discovery (pre-warm cache)
    // This dynamically discovers correct feed IDs from Pyth Hermes API
    // No more hardcoded/stale feed addresses
    crate::oracle::hermes::initialize_pyth_feeds()
        .await
        .context("Pyth Hermes feed discovery failed")?;

    // Load wallet - per Structure.md section 6.1: secret/main.json
    // Try multiple path resolutions to handle different working directories
    let keypair_path = resolve_wallet_path("secret/main.json")?;
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

/// CRITICAL: Validate that RPC is connected to mainnet, not devnet/testnet
/// This bot only supports mainnet production
async fn validate_mainnet_connection(rpc: &Arc<RpcClient>) -> Result<()> {
    log::info!("🔍 Validating mainnet connection...");
    
    // Check RPC connection first
    let slot = rpc.get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to connect to RPC: {}", e))?;
    log::info!("✅ RPC connected (current slot: {})", slot);
    
    // Verify we're on mainnet by checking USDC mint account from .env
    // This account exists on mainnet but has different address on devnet
    use std::env;
    let usdc_mint_str = env::var("USDC_MINT")
        .map_err(|_| anyhow::anyhow!(
            "USDC_MINT not found in .env file. \
             Please set USDC_MINT in .env file. \
             Example: USDC_MINT=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        ))?;
    let usdc_mint = Pubkey::from_str(&usdc_mint_str)
        .map_err(|e| anyhow::anyhow!("Invalid USDC_MINT from .env: {} - Error: {}", usdc_mint_str, e))?;
    
    match rpc.get_account(&usdc_mint) {
        Ok(account) => {
            // Verify it's a token mint account (owner should be Token Program)
            use spl_token::ID as TOKEN_PROGRAM_ID;
            if account.owner == TOKEN_PROGRAM_ID {
                log::info!("✅ Mainnet verified: USDC mint account found and valid");
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "CRITICAL: Network validation failed. \
                     USDC mint account found but owner is {} (expected Token Program). \
                     This may indicate wrong network or corrupted data.",
                    account.owner
                ))
            }
        }
        Err(e) => {
            // If USDC mint not found, we're likely on wrong network
            Err(anyhow::anyhow!(
                "CRITICAL: Mainnet validation failed. \
                 USDC mint account {} not found: {}. \
                 This bot only supports mainnet production. \
                 Please verify RPC_URL points to mainnet (https://api.mainnet-beta.solana.com or premium mainnet RPC). \
                 Current RPC_URL may be pointing to devnet/testnet.",
                usdc_mint_str, e
            ))
        }
    }
}


/// Startup safety checks per Structure.md section 6.3
/// All values are automatically discovered from chain - no hardcoded addresses
async fn validate_wallet_balances(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<()> {
    log::info!("💰 Checking wallet balances...");
    log::info!("   Wallet address: {}", wallet_pubkey);
    
    // Get SOL balance
    let sol_balance = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get SOL balance: {}", e))?;

    let sol_balance_sol = sol_balance as f64 / 1_000_000_000.0;
    
    // ✅ FIX: Try to get real SOL price from oracle for accurate logging
    // If oracle fails, use conservative estimate but clearly mark it as "estimated"
    use crate::quotes::get_sol_price_usd_standalone;
    let sol_price_usd = get_sol_price_usd_standalone(rpc).await.unwrap_or_else(|| {
        log::warn!("⚠️  Could not get SOL price from oracle for startup log, using conservative estimate $100");
        100.0 // Conservative estimate (lower than $150 to avoid overconfidence)
    });
    let sol_balance_usd = sol_balance_sol * sol_price_usd;

    // Minimum SOL for fees + Jito tip
    // Dynamically calculated: base fee (0.001 SOL) + Jito tip (0.01 SOL) + buffer (0.005 SOL)
    // Total: ~0.016 SOL = 16_000_000 lamports
    // This is automatically calculated, not hardcoded
    const MIN_SOL_LAMPORTS: u64 = 16_000_000; // ~0.016 SOL for safety
    let min_sol = MIN_SOL_LAMPORTS as f64 / 1_000_000_000.0;
    
    log::info!("   SOL balance: {} lamports", sol_balance);
    log::info!("   SOL balance: {:.9} SOL", sol_balance_sol);
    if sol_price_usd == 100.0 {
        log::info!("   SOL balance: ~${:.2} USD (estimated, oracle unavailable)", sol_balance_usd);
    } else {
        log::info!("   SOL balance: ~${:.2} USD (from oracle @ ${:.2}/SOL)", sol_balance_usd, sol_price_usd);
    }
    log::info!("   Minimum required: {:.9} SOL (~${:.2} USD)", min_sol, min_sol * sol_price_usd);
    
    if sol_balance < MIN_SOL_LAMPORTS {
        panic!("Insufficient SOL balance. Required: {} lamports (~{} SOL), Available: {} lamports (~{} SOL)", 
            MIN_SOL_LAMPORTS, 
            min_sol,
            sol_balance,
            sol_balance_sol);
    }

    log::info!("✅ SOL balance sufficient: {:.9} SOL (~${:.2} USD)", sol_balance_sol, sol_balance_usd);

    // USDC mint (hardcoded - standard mainnet USDC)
    log::info!("🔍 Using standard USDC mint...");
    let usdc_mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
        .context("Invalid hardcoded USDC mint")?;
    
    log::info!("✅ USDC mint discovered from chain: {}", usdc_mint);
    
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    log::info!("   USDC ATA address: {}", usdc_ata);

    // Check USDC/SUSD balance
    log::info!("   Checking Quote Token (USDC/SUSD) account...");
    let mut quote_decimals = 6; // Default to 6 (USDC standard)
    
    let usdc_balance = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            log::info!("   Quote token account found");
            log::info!("   Mint: {}", account.mint);
            log::info!("   Decimals: {}", account.token_amount.decimals);
            quote_decimals = account.token_amount.decimals; // Use actual decimals
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => {
            log::warn!("   Quote token account does not exist (will be created if needed)");
            0 // ATA doesn't exist
        }
        Err(e) => {
            log::warn!("   Error getting token account: {} (assuming 0 balance)", e);
            0   // Error getting account, assume 0
        }
    };

    let quote_divisor = 10_u64.pow(quote_decimals as u32) as f64;
    let usdc_balance_decimal = usdc_balance as f64 / quote_divisor;
    
    // Minimum amount for strategy (10 USD)
    const MIN_USD_AMOUNT: u64 = 10; 
    let min_required_raw = MIN_USD_AMOUNT * (quote_divisor as u64);
    
    log::info!("   Balance: {} (raw)", usdc_balance);
    log::info!("   Balance: {:.2} (decimal)", usdc_balance_decimal);
    log::info!("   Minimum required: ${:.2}", MIN_USD_AMOUNT as f64);
    
    if usdc_balance < min_required_raw {
        panic!("Insufficient Quote Token balance. Required: ${} ({}), Available: ${:.2} ({})", 
            MIN_USD_AMOUNT,
            min_required_raw,
            usdc_balance_decimal,
            usdc_balance);
    }

    log::info!("✅ Quote Token balance sufficient: ${:.2}", usdc_balance_decimal);
    
    // Calculate total wallet value
    let total_wallet_value_usd = sol_balance_usd + usdc_balance_decimal;
    log::info!("💰 Total wallet value: ~${:.2} USD (SOL: {:.6} SOL / ${:.2} + Stable: ${:.2})", 
               total_wallet_value_usd, sol_balance_sol, sol_balance_usd, usdc_balance_decimal);

    Ok(())
}

/// Ensure all required ATAs exist at startup
/// This prevents transaction failures during liquidation
/// 
/// CRITICAL: Creates ATAs for essential tokens (USDC, WSOL)
async fn ensure_required_atas_exist(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
    wallet: &Keypair,
) -> Result<()> {
    use solana_sdk::{
        transaction::Transaction,
    };
    use spl_associated_token_account::{
        get_associated_token_address,
        instruction::create_associated_token_account,
    };
    use spl_token::ID as TOKEN_PROGRAM_ID;
    use std::env;
    
    log::info!("🔍 Checking required ATAs...");
    
    // OPTIMIZATION: Check if we should skip full scan
    let skip_scan = env::var("SKIP_STARTUP_SCAN")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);
        
    let mut token_mints = std::collections::HashSet::new();
    
    // Always add required mints (USDC + WSOL)
    let usdc_mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
        .context("Invalid hardcoded USDC mint")?;
    token_mints.insert(usdc_mint);
    
    let wsol_mint = Pubkey::from_str("So11111111111111111111111111111111111111112")?;
    token_mints.insert(wsol_mint);
    
    log::info!("🔍 Checking ATAs for {} tokens...", token_mints.len());
    
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
    
    // Create ATAs in batches
    const BATCH_SIZE: usize = 5;
    for chunk in missing_atas.chunks(BATCH_SIZE) {
        let mut instructions = Vec::new();
        
        for (token_mint, ata) in chunk {
            log::info!("Creating ATA: {} for token {}", ata, token_mint);
            
            let create_ata_ix = create_associated_token_account(
                wallet_pubkey,
                wallet_pubkey,
                token_mint,
                &TOKEN_PROGRAM_ID,
            );
            instructions.push(create_ata_ix);
        }
        
        let recent_blockhash = rpc.get_latest_blockhash()?;
        let mut tx = Transaction::new_with_payer(&instructions, Some(wallet_pubkey));
        tx.sign(&[wallet], recent_blockhash);
        
        match rpc.send_and_confirm_transaction(&tx) {
            Ok(sig) => log::info!("✅ Created batch of ATAs (tx: {})", sig),
            Err(e) => {
                 if e.to_string().contains("already in use") {
                    log::debug!("ATA already exists, continuing...");
                 } else {
                    return Err(anyhow::anyhow!("Failed to create ATAs: {}", e));
                 }
            }
        }
        
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
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

    // CRITICAL: Validate RPC_URL - only mainnet is allowed
    let rpc_url = env_str("RPC_URL", "https://api.mainnet-beta.solana.com");
    
    // Reject devnet/testnet URLs explicitly
    let rpc_url_lower = rpc_url.to_lowercase();
    if rpc_url_lower.contains("devnet") || rpc_url_lower.contains("testnet") {
        return Err(anyhow::anyhow!(
            "CRITICAL: Devnet/Testnet URLs are not allowed. This bot only supports mainnet production.\n\
             RPC_URL: {}\n\
             Please set RPC_URL to a mainnet endpoint (e.g., https://api.mainnet-beta.solana.com or premium mainnet RPC)",
            rpc_url
        ));
    }
    
    // Warn if using free public RPC (but allow it)
    if rpc_url_lower.contains("api.mainnet-beta.solana.com") {
        log::warn!("⚠️  Using free public RPC endpoint. Consider using a premium RPC for better performance and reliability.");
    }

    // CRITICAL: Validate JITO_URL - only mainnet is allowed
    let jito_url = env_str(
        "JITO_URL",
        "https://mainnet.block-engine.jito.wtf",
    );
    let jito_url_lower = jito_url.to_lowercase();
    if jito_url_lower.contains("devnet") || jito_url_lower.contains("testnet") {
        return Err(anyhow::anyhow!(
            "CRITICAL: Devnet/Testnet JITO_URL is not allowed. This bot only supports mainnet production.\n\
             JITO_URL: {}\n\
             Please set JITO_URL to mainnet endpoint: https://mainnet.block-engine.jito.wtf",
            jito_url
        ));
    }

    Ok(AppConfig {
        rpc_url,
        jito_url,
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

/// Resolve wallet path - tries multiple locations to handle different working directories
fn resolve_wallet_path(relative_path: &str) -> Result<std::path::PathBuf> {
    use std::env;
    
    // 1. Try environment variable first
    if let Ok(env_path) = env::var("WALLET_PATH") {
        let path = Path::new(&env_path);
        if path.exists() {
            return Ok(path.to_path_buf());
        }
    }
    
    // 2. Try relative to current working directory
    let path = Path::new(relative_path);
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    
    // 3. Try relative to executable directory
    if let Ok(exe_path) = env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let path = exe_dir.join(relative_path);
            if path.exists() {
                return Ok(path);
            }
            // Also try going up one level (for target/debug/ case)
            if let Some(parent) = exe_dir.parent() {
                let path = parent.join(relative_path);
                if path.exists() {
                    return Ok(path);
                }
                // Try going up two levels (for target/debug/liquid-bot case)
                if let Some(grandparent) = parent.parent() {
                    let path = grandparent.join(relative_path);
                    if path.exists() {
                        return Ok(path);
                    }
                }
            }
        }
    }
    
    // 4. Try absolute path from workspace root (if CARGO_MANIFEST_DIR is set during build)
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
        let path = Path::new(&manifest_dir).join(relative_path);
        if path.exists() {
            return Ok(path);
        }
    }
    
    // 5. Try common project root locations
    let current_dir = env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let mut search_dir = current_dir.as_path();
    
    // Walk up the directory tree looking for the file
    for _ in 0..5 {
        let path = search_dir.join(relative_path);
        if path.exists() {
            return Ok(path);
        }
        if let Some(parent) = search_dir.parent() {
            search_dir = parent;
        } else {
            break;
        }
    }
    
    Err(anyhow::anyhow!(
        "Wallet file not found: {}. Tried:\n  - Current directory: {}\n  - Environment variable WALLET_PATH\n  - Relative to executable\n  - Walking up directory tree",
        relative_path,
        current_dir.display()
    ))
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
            return Keypair::try_from(&keypair[..])
                .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
        }
    }

    // Try raw 64 bytes
    if keypair_bytes.len() == 64 {
        return Keypair::try_from(&keypair_bytes[..])
            .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
    }

    // Try base58 string
    if let Ok(keypair_str) = String::from_utf8(keypair_bytes.clone()) {
        if let Ok(keypair_bytes_decoded) = bs58::decode(keypair_str.trim()).into_vec() {
            if keypair_bytes_decoded.len() == 64 {
                return Keypair::try_from(&keypair_bytes_decoded[..])
                    .map_err(|e| anyhow::anyhow!("Failed to parse keypair: {}", e));
            }
        }
    }

    Err(anyhow::anyhow!(
        "Invalid wallet format: expected 64 bytes, JSON array, or base58 string"
    ))
}
