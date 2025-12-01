use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use crate::protocol::solend::accounts::get_associated_token_address;
use anyhow::{Context, Result};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::str::FromStr;
use tokio::sync::RwLock;

const ATA_CACHE_FILE: &str = ".cache/atas.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AtaCacheData {
    existing_atas: Vec<String>,
    checked_atas: Vec<String>,
    wallet_pubkey: String,
    last_updated: u64, // Unix timestamp
}

/// Hybrid cache: File-based persistent + Memory cache for fast access
/// Best practice: Persistent cache survives restarts, memory cache for speed
pub struct AtaCache {
    existing_atas: Arc<RwLock<HashSet<Pubkey>>>,
    checked_atas: Arc<RwLock<HashSet<Pubkey>>>,
    wallet_pubkey: Pubkey,
    cache_file_path: String,
}

impl AtaCache {
    /// Create new ATA cache with file-based persistence
    /// Note: Call load_from_file() after creation to load persistent cache
    pub fn new(wallet_pubkey: Pubkey) -> Self {
        AtaCache {
            existing_atas: Arc::new(RwLock::new(HashSet::new())),
            checked_atas: Arc::new(RwLock::new(HashSet::new())),
            wallet_pubkey,
            cache_file_path: ATA_CACHE_FILE.to_string(),
        }
    }

    /// Load cache from persistent file (async)
    pub async fn load_from_file(&self) -> Result<()> {
        let path = Path::new(&self.cache_file_path);
        if !path.exists() {
            return Ok(()); // No cache file yet, that's OK
        }

        let content = fs::read_to_string(path)
            .context("Failed to read ATA cache file")?;
        
        let cache_data: AtaCacheData = serde_json::from_str(&content)
            .context("Failed to parse ATA cache file")?;

        // Verify wallet matches
        let cached_wallet: Pubkey = cache_data.wallet_pubkey.parse()
            .context("Invalid wallet pubkey in cache")?;
        
        if cached_wallet != self.wallet_pubkey {
            log::warn!("ATA cache wallet mismatch ({} vs {}), clearing cache", 
                cached_wallet, self.wallet_pubkey);
            return Ok(()); // Don't load mismatched cache
        }

        // Load into memory
        let mut existing = self.existing_atas.write().await;
        let mut checked = self.checked_atas.write().await;
        
        for ata_str in cache_data.existing_atas {
            if let Ok(ata) = ata_str.parse::<Pubkey>() {
                existing.insert(ata);
                checked.insert(ata);
            }
        }
        
        for ata_str in cache_data.checked_atas {
            if let Ok(ata) = ata_str.parse::<Pubkey>() {
                checked.insert(ata);
            }
        }

        let existing_count = existing.len();
        let checked_count = checked.len();
        drop(existing);
        drop(checked);

        log::info!("✅ Loaded {} existing ATAs and {} checked ATAs from cache", 
            existing_count, checked_count);
        
        Ok(())
    }

    /// Save cache to persistent file
    pub async fn save_to_file(&self) -> Result<()> {
        // Ensure cache directory exists
        if let Some(parent) = Path::new(&self.cache_file_path).parent() {
            fs::create_dir_all(parent)
                .context("Failed to create cache directory")?;
        }

        let existing = self.existing_atas.read().await;
        let checked = self.checked_atas.read().await;

        let cache_data = AtaCacheData {
            existing_atas: existing.iter().map(|p| p.to_string()).collect(),
            checked_atas: checked.iter().map(|p| p.to_string()).collect(),
            wallet_pubkey: self.wallet_pubkey.to_string(),
            last_updated: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| anyhow::anyhow!("Failed to get timestamp: {}", e))?
                .as_secs(),
        };

        let json = serde_json::to_string_pretty(&cache_data)
            .context("Failed to serialize ATA cache")?;
        
        fs::write(&self.cache_file_path, json)
            .context("Failed to write ATA cache file")?;

        log::debug!("💾 Saved ATA cache to file: {} existing, {} checked", 
            existing.len(), checked.len());
        
        Ok(())
    }

    pub async fn is_ata_checked(&self, ata: &Pubkey) -> bool {
        self.checked_atas.read().await.contains(ata)
    }

    pub async fn is_ata_existing(&self, ata: &Pubkey) -> bool {
        self.existing_atas.read().await.contains(ata)
    }

    pub async fn mark_ata_existing(&self, ata: Pubkey) {
        let mut existing = self.existing_atas.write().await;
        existing.insert(ata);
        let mut checked = self.checked_atas.write().await;
        checked.insert(ata);
        
        // Save to file (best effort, don't fail if it doesn't work)
        if let Err(e) = self.save_to_file().await {
            log::debug!("Failed to save ATA cache to file: {}", e);
        }
    }

    pub async fn mark_ata_checked(&self, ata: Pubkey) {
        let mut checked = self.checked_atas.write().await;
        checked.insert(ata);
        
        // Save to file (best effort)
        if let Err(e) = self.save_to_file().await {
            log::debug!("Failed to save ATA cache to file: {}", e);
        }
    }
}

/// Ensure all required ATAs exist for the bot wallet
pub async fn ensure_required_atas(
    rpc: Arc<RpcClient>,
    wallet: Arc<Keypair>,
    config: &Config,
    ata_cache: Arc<AtaCache>,
) -> Result<()> {
    let wallet_pubkey = wallet.pubkey();
    log::info!("🔍 Checking required ATAs for wallet: {}", wallet_pubkey);

    // Get all required mints from config
    let mints = vec![
        ("USDC", config.usdc_mint.as_str()),
        ("SOL", config.sol_mint.as_str()),
        ("USDT", config.usdt_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
        ("ETH", config.eth_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
        ("BTC", config.btc_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
    ];

    // First pass: Check cache and collect ATAs that need on-chain check
    let mut to_check = Vec::new();
    
    for (name, mint_str) in &mints {
        if mint_str.is_empty() {
            continue;
        }

        let mint = Pubkey::from_str(mint_str)
            .with_context(|| format!("Invalid {} mint", name))?;

        let ata = get_associated_token_address(&wallet_pubkey, &mint, Some(config))
            .with_context(|| format!("Failed to derive ATA for {}", name))?;

        // Check cache first
        if ata_cache.is_ata_existing(&ata).await {
            log::debug!("✅ {} ATA already exists (cached): {}", name, ata);
            continue;
        }

        // If already checked and not existing, skip
        if ata_cache.is_ata_checked(&ata).await {
            log::debug!("⚠️  {} ATA was checked before and doesn't exist: {}", name, ata);
            continue;
        }

        // Need to check on-chain
        to_check.push((*name, mint, ata));
    }

    if to_check.is_empty() {
        log::info!("🎉 All required ATAs already exist (from cache)!");
        return Ok(());
    }

    log::info!("🔍 Checking {} ATA(s) on-chain...", to_check.len());

    // Second pass: Check on-chain in parallel with timeout
    let mut instructions = Vec::new();
    let mut missing_atas = Vec::new();

    // Use join_all for parallel RPC calls with timeout
    let check_tasks: Vec<_> = to_check.into_iter().map(|(name, mint, ata)| {
        let rpc = Arc::clone(&rpc);
        let ata_cache = Arc::clone(&ata_cache);
        let name = name.to_string();
        
        async move {
            // Add timeout to prevent hanging
            let timeout = tokio::time::Duration::from_secs(10);
            let check_result = tokio::time::timeout(timeout, rpc.get_account(&ata)).await;
            
            match check_result {
                Ok(Ok(_)) => {
                    log::info!("✅ {} ATA already exists: {}", name, ata);
                    ata_cache.mark_ata_existing(ata).await;
                    Ok((name, mint, ata, true))
                }
                Ok(Err(e)) => {
                    let error_msg = e.to_string();
                    if error_msg.contains("AccountNotFound") || error_msg.contains("account not found") {
                        log::warn!("❌ {} ATA not found: {}, will create...", name, ata);
                        Ok((name, mint, ata, false))
                    } else {
                        log::warn!("⚠️  Failed to check {} ATA ({}): {}", name, ata, e);
                        ata_cache.mark_ata_checked(ata).await;
                        Err(format!("RPC error for {}: {}", name, e))
                    }
                }
                Err(_) => {
                    log::warn!("⚠️  Timeout checking {} ATA ({}), skipping...", name, ata);
                    ata_cache.mark_ata_checked(ata).await;
                    Err(format!("Timeout checking {} ATA", name))
                }
            }
        }
    }).collect();

    let results = join_all(check_tasks).await;
    
    log::debug!("ATA check completed, processing {} results...", results.len());
    
    for result in results {
        match result {
            Ok((name, mint, ata, exists)) => {
                if !exists {
                    let name_clone = name.clone();
                    missing_atas.push((name, mint, ata));
                    
                    log::debug!("Creating ATA instruction for {} (mint: {})...", name_clone, mint);
                    
                    // Create ATA instruction
                    match create_ata_instruction(
                        &wallet_pubkey,
                        &wallet_pubkey,
                        &mint,
                        Some(config),
                    ) {
                        Ok(ix) => {
                            log::debug!("✅ ATA instruction created for {}", name_clone);
                            instructions.push(ix);
                        }
                        Err(e) => {
                            log::warn!("⚠️  Failed to create ATA instruction for {}: {}", name_clone, e);
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("⚠️  ATA check failed: {}", e);
            }
        }
    }

    log::debug!("Processed all results: {} missing ATAs, {} instructions", missing_atas.len(), instructions.len());

    if instructions.is_empty() {
        log::info!("🎉 All required ATAs already exist!");
        return Ok(());
    }

    log::info!("📝 Creating {} missing ATA(s)...", instructions.len());

    // Send transaction to create missing ATAs
    log::debug!("Getting recent blockhash...");
    let recent_blockhash = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        rpc.get_recent_blockhash()
    ).await
    .context("Timeout getting recent blockhash")??;
    
    log::debug!("Building transaction with {} instructions...", instructions.len());
    let mut tx = Transaction::new_with_payer(&instructions, Some(&wallet_pubkey));
    tx.sign(&[wallet.as_ref()], recent_blockhash);
    
    log::debug!("Sending transaction to create ATAs...");
    let send_result = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        rpc.send_transaction(&tx)
    ).await;

    match send_result {
        Ok(Ok(sig)) => {
            log::info!("✅ ATA creation transaction sent: {}", sig);
            
            // Wait a bit for transaction to confirm
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            
            // Verify ATAs were created and update cache
            for (name, _mint, ata) in missing_atas {
                match rpc.get_account(&ata).await {
                    Ok(_) => {
                        log::info!("✅ {} ATA successfully created: {}", name, ata);
                        ata_cache.mark_ata_existing(ata).await;
                        // Cache is saved automatically in mark_ata_existing
                    }
                    Err(e) => {
                        log::warn!("⚠️  {} ATA creation may have failed ({}): {}", name, ata, e);
                        // Don't mark as existing, but mark as checked so we don't retry immediately
                        ata_cache.mark_ata_checked(ata).await;
                    }
                }
            }
            
            log::info!("🎉 ATA setup completed!");
        }
        Ok(Err(e)) => {
            log::error!("❌ Failed to create ATAs: {}", e);
            log::error!("   You may need to create them manually or ensure wallet has enough SOL");
            // Don't fail - bot can still run, ATAs will be created on-demand during transactions
            log::warn!("   Bot will continue, but may fail during liquidation if ATAs are missing");
        }
        Err(_) => {
            log::error!("❌ Timeout sending ATA creation transaction (30s)");
            log::error!("   Transaction may have been sent but not confirmed yet");
            log::warn!("   Bot will continue, but may fail during liquidation if ATAs are missing");
        }
    }

    Ok(())
}

fn create_ata_instruction(
    payer: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
    config: Option<&Config>,
) -> Result<Instruction> {
    use crate::protocol::solend::accounts::get_associated_token_program_id;
    
    let associated_token_program = get_associated_token_program_id(config)?;
    let token_program = spl_token::id();

    let ata = Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &associated_token_program,
    ).0;
    
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
            solana_sdk::instruction::AccountMeta::new_readonly(token_program, false),
        ],
        data: vec![],
    })
}

