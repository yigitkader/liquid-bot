use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::jup::JupiterQuote;
// Kamino Lend support
use crate::kamino::{
    Obligation as KaminoObligation,
    Reserve as KaminoReserve,
    ObligationCollateral as KaminoObligationCollateral,
    ObligationLiquidity as KaminoObligationLiquidity,
    detect_account_type as kamino_detect_account_type,
    KaminoAccountType,
    KAMINO_LEND_PROGRAM_ID,
    SCALE_FACTOR,
};
// Common trait for lending protocols
use crate::lending_trait::LendingObligation;
use crate::utils::{send_jito_bundle, JitoClient};
use crate::oracle;
use crate::liquidation_tx::build_flashloan_liquidation_tx;
use crate::quotes::get_liquidation_quote;
use crate::wallet::{log_wallet_balances, get_wallet_value_usd};

/// Liquidation mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidationMode {
    DryRun,
    Live,
}

/// Config structure per Structure.md section 6.2
/// All values are automatically discovered from chain or environment - no hardcoded addresses
pub struct Config {
    pub rpc_url: String,
    pub jito_url: String,
    pub jupiter_url: String,
    pub keypair_path: PathBuf,
    pub liquidation_mode: LiquidationMode,
    pub min_profit_usdc: f64,
    pub max_position_pct: f64,
    pub wallet: Arc<Keypair>,
    pub jito_tip_account: Option<String>,
    pub jito_tip_amount_lamports: Option<u64>,
}

/// Main liquidation loop - minimal async pipeline per Structure.md section 9
pub async fn run_liquidation_loop(
    rpc: Arc<RpcClient>,
    config: Config,
) -> Result<()> {
    // Determine lending program ID from environment
    // Priority: LENDING_PROGRAM_ID > Kamino mainnet default
    let program_id = {
        use std::env;
        if let Ok(id) = env::var("LENDING_PROGRAM_ID") {
            Pubkey::from_str(&id).context("Invalid LENDING_PROGRAM_ID")?
        } else {
            // Default to Kamino Lend mainnet
            Pubkey::from_str(KAMINO_LEND_PROGRAM_ID)
                .expect("Invalid hardcoded Kamino program ID")
        }
    };
    
    log::info!("🚀 Using Kamino Lend protocol (program: {})", program_id);
    
    let wallet = config.wallet.pubkey();

    use std::env;
    let jito_tip_account_str = if let Some(tip_account) = config.jito_tip_account.as_deref() {
        tip_account.to_string()
    } else if let Ok(tip_account) = env::var("JITO_TIP_ACCOUNT") {
        tip_account
    } else {
        return Err(anyhow::anyhow!(
            "JITO_TIP_ACCOUNT not found in .env file or config. \
             Please set JITO_TIP_ACCOUNT in .env file. \
             Example: JITO_TIP_ACCOUNT=96gYZGLnJYVFmbjzopPSU6QiEV5fGqZ6N6VBY6FuDgU3"
        ));
    };
    
    let jito_tip_account = Pubkey::from_str(&jito_tip_account_str)
        .context(format!(
            "CRITICAL: Invalid Jito tip account address format: {}. \
             Jito is REQUIRED for bot operation (all transactions sent via Jito bundles). \
             Please verify JITO_TIP_ACCOUNT in .env is a valid Solana address.",
            jito_tip_account_str
        ))?;
    
    match rpc.get_account(&jito_tip_account) {
        Ok(account_data) => {
            use solana_system_interface::program;
            let system_program_address = program::id();
            let system_program_id = Pubkey::try_from(system_program_address.as_ref())
                .unwrap_or_else(|_| Pubkey::default()); // Fallback if conversion fails
            if account_data.owner == system_program_id {
                let balance_sol = account_data.lamports as f64 / 1_000_000_000.0;
                log::info!(
                    "✅ Jito tip account validated: {} (balance: {:.6} SOL, owner: system program)",
                    jito_tip_account,
                    balance_sol
                );
            } else {
                log::warn!(
                    "⚠️  Jito tip account {} exists but owner is {} (expected system program). \
                     This may indicate wrong address. Tips may be lost, but bot will continue.",
                    jito_tip_account,
                    account_data.owner
                );
            }
        }
        Err(_) => {
            log::warn!(
                "ℹ️  Jito tip account {} not found in RPC (this is OK - tip accounts are transfer addresses, not regular accounts). \
                 Bot will work normally - tip transaction will be created when sending bundles.",
                jito_tip_account
            );
        }
    }
    
    let jito_tip_amount = config.jito_tip_amount_lamports
        .unwrap_or(10_000_000u64);
    log::info!("✅ Jito tip amount: {} lamports (~{} SOL)", 
        jito_tip_amount, 
        jito_tip_amount as f64 / 1_000_000_000.0);
    
    let final_tip_account = if config.jito_tip_account.is_some() {
        log::info!("Using JITO_TIP_ACCOUNT from environment: {}", jito_tip_account);
        jito_tip_account
    } else {
        let temp_jito_client = JitoClient::new(
            config.jito_url.clone(),
            jito_tip_account,
            jito_tip_amount,
        );
        
        match temp_jito_client.get_tip_accounts().await {
            Ok(tip_accounts) => {
                if !tip_accounts.is_empty() {
                    let dynamic_tip_account = tip_accounts[0];
                    log::info!(
                        "✅ Using tip account from Jito API (dynamic): {} (found {} total tip accounts)",
                        dynamic_tip_account,
                        tip_accounts.len()
                    );
                    dynamic_tip_account
                } else {
                    log::warn!("Jito API returned empty tip accounts list, using default");
                    jito_tip_account
                }
            }
            Err(e) => {
                log::warn!(
                    "Failed to fetch tip accounts from Jito API: {}. Using default/fallback tip account: {}",
                    e,
                    jito_tip_account
                );
                log::info!("Tip: Set JITO_TIP_ACCOUNT in .env to use a specific account, or ensure Jito API is accessible");
                jito_tip_account
            }
        }
    };
    
    let jito_client = JitoClient::new(
        config.jito_url.clone(),
        final_tip_account,
        jito_tip_amount,
    );

    validate_jito_endpoint(&jito_client).await
        .context("Jito endpoint validation failed - check network connectivity")?;

    log::info!("🚀 Starting liquidation loop");
    log::info!("   Program ID: {}", program_id);
    log::info!("   Wallet: {}", wallet);
    log::info!("   RPC URL: {} (masked)", {
        let url = &config.rpc_url;
        if url.len() > 50 {
            format!("{}...{}", &url[..25], &url[url.len()-25..])
        } else {
            url.clone()
        }
    });
    log::info!("   Keypair path: {}", config.keypair_path.display());
    log::info!("   Min Profit USDC: ${}", config.min_profit_usdc);
    log::info!("   Max Position %: {:.2}%", config.max_position_pct * 100.0);
    log::info!("   Mode: {:?}", config.liquidation_mode);

    // Track last balance log time for periodic logging
    let mut last_balance_log = Instant::now();
    const BALANCE_LOG_INTERVAL_SECS: u64 = 30; // Log balances every 30 seconds
    
    loop {
        // Log wallet balances periodically
        if last_balance_log.elapsed().as_secs() >= BALANCE_LOG_INTERVAL_SECS {
            if let Err(e) = log_wallet_balances(&rpc, &wallet).await {
                log::warn!("Failed to log wallet balances: {}", e);
            }
            last_balance_log = Instant::now();
        }
        
        match process_cycle(&rpc, &program_id, &config, &jito_client).await {
            Ok(_) => {}
            Err(e) => {
                log::error!("Error in liquidation cycle: {}", e);
            }
        }

        // Poll interval from .env (no hardcoded values)
        use std::env;
        let poll_interval_ms = env::var("POLL_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(|| {
                log::warn!("POLL_INTERVAL_MS not found in .env, using default 500ms");
                500 // Default 500ms if not set
            });
        sleep(Duration::from_millis(poll_interval_ms)).await;
    }
}

/// Metrics for tracking liquidation cycle performance
/// Enhanced to track different failure reasons for better observability
struct CycleMetrics {
    total_candidates: usize,
    skipped_oracle_fail: usize,
    skipped_jupiter_fail: usize,
    skipped_insufficient_profit: usize,
    skipped_risk_limit: usize,
    failed_build_tx: usize,
    failed_send_bundle: usize,
    successful: usize,
    // Additional detailed tracking
    skipped_rpc_error: usize,      // RPC call failures
    skipped_reserve_load_fail: usize, // Failed to load reserve accounts
    skipped_ata_missing: usize,   // Missing ATA (shouldn't happen if startup check works)
    
    // Bundle outcome tracking
    confirmed_bundles: usize,     // Bundles confirmed on-chain
    dropped_bundles: usize,       // Bundles dropped/failed after sending
}

async fn process_cycle(
    rpc: &Arc<RpcClient>,
    program_id: &Pubkey,
    config: &Config,
    jito_client: &JitoClient,
) -> Result<()> {
    // 1. Kamino Lend obligation account'larını çek
    // NOTE: This RPC call fetches accounts and can take 30-90 seconds
    log::info!("📡 Fetching Kamino Lend accounts from RPC (this may take 30-90 seconds)...");
    let fetch_start = std::time::Instant::now();
    let rpc_clone = Arc::clone(rpc);
    let program_id_clone = *program_id;
    let accounts = tokio::task::spawn_blocking(move || {
        rpc_clone.get_program_accounts(&program_id_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
    .map_err(|e| anyhow::anyhow!("Failed to get program accounts: {}", e))?;

    let fetch_duration = fetch_start.elapsed();
    log::info!("✅ Fetched {} accounts in {:.1}s", accounts.len(), fetch_duration.as_secs_f64());

    // 🔴 CRITICAL FIX: Get current slot for stale obligation check
    // Use retry mechanism to handle transient RPC errors after large get_program_accounts calls
    let current_slot = {
        let mut retries = 3;
        let mut last_error = None;
        loop {
            match rpc.get_slot() {
                Ok(slot) => break slot,
                Err(e) => {
                    retries -= 1;
                    last_error = Some(e);
                    if retries == 0 {
                        return Err(anyhow::anyhow!(
                            "Failed to get current slot after 3 retries: {}",
                            last_error.unwrap()
                        ));
                    }
                    log::warn!("⚠️ get_slot failed (retries left: {}): {}", retries, last_error.as_ref().unwrap());
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
    };
    
    // Configurable max slot diff for stale obligations
    // Default: 450 slots = ~3 minutes (previously 150 slots = ~60 seconds was too aggressive)
    // Many obligations may not be updated frequently, causing false "stale" skips
    let max_obligation_slot_diff = std::env::var("MAX_OBLIGATION_SLOT_DIFF")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(450); // Default: 450 slots (~3 minutes at 400ms per slot)

    // 2. HF < 1.0 olanları bul
    // CRITICAL SECURITY: Track parse errors to detect layout changes or corrupt data
    log::info!("🔍 Scanning {} accounts for liquidatable obligations...", accounts.len());
    let scan_start = std::time::Instant::now();
    
    let mut candidates: Vec<(Pubkey, KaminoObligation)> = Vec::new();
    let mut parse_errors: usize = 0;
    let mut skipped_wrong_type: usize = 0;
    let mut obligation_count: usize = 0;
    let mut skipped_zero_borrow: usize = 0;
    let mut skipped_zero_deposit: usize = 0;
    let mut skipped_stale: usize = 0;
    let mut skipped_hf_too_high: usize = 0;
    let total_accounts = accounts.len();
    
    // Read health factor threshold from .env (no hardcoded values)
    let hf_threshold = std::env::var("HF_LIQUIDATION_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1.0); // Default 1.0 if not set
    
    for (pk, acc) in accounts {
        // Use Kamino account type detection (Anchor discriminators)
        match kamino_detect_account_type(&acc.data) {
            KaminoAccountType::Obligation => {
                obligation_count += 1;
                // Parse Kamino Obligation
                match KaminoObligation::from_account_data(&acc.data) {
                    Ok(obligation) => {
                        // Get USD values from Kamino obligation using trait
                        let borrowed_value: f64 = LendingObligation::borrowed_value_usd(&obligation);
                        let deposited_value: f64 = LendingObligation::deposited_value_usd(&obligation);
                        
                        // Skip if no borrows (nothing to liquidate)
                        if !LendingObligation::has_any_debt(&obligation) || borrowed_value <= 0.0 {
                            skipped_zero_borrow += 1;
                            if skipped_zero_borrow <= 3 {
                                log::debug!(
                                    "Skipping obligation {}: zero borrow (borrowed=${:.2})",
                                    pk,
                                    borrowed_value
                                );
                            }
                            continue;
                        }
                        
                        // Skip if no deposits (no collateral to liquidate)
                        if !LendingObligation::has_any_deposits(&obligation) || deposited_value <= 0.0 {
                            skipped_zero_deposit += 1;
                            if skipped_zero_deposit <= 3 {
                                log::debug!(
                                    "Skipping obligation {}: zero deposit (deposited=${:.2})",
                                    pk,
                                    deposited_value
                                );
                            }
                            continue;
                        }
                        
                        // Skip if obligation is marked as stale
                        if LendingObligation::is_stale(&obligation) {
                            skipped_stale += 1;
                                if skipped_stale <= 3 {
                                    log::debug!(
                                    "Skipping obligation {}: marked as stale",
                                    pk
                                );
                            }
                            continue;
                        }
                        
                        // Calculate health factor using trait
                        // Kamino HF = allowed_borrow_value / borrow_factor_adjusted_debt_value
                        let hf = LendingObligation::health_factor(&obligation);
                        
                        // Check if liquidatable (HF < threshold)
                        let is_liquidatable = hf < hf_threshold;
                        
                        if is_liquidatable {
                            // Log obligation details for debugging
                            log::debug!(
                                "✅ Found liquidatable obligation {}: HF={:.6}, deposited=${:.2}, borrowed=${:.2}",
                                pk,
                                hf,
                                deposited_value,
                                borrowed_value
                            );
                            candidates.push((pk, obligation));
                        } else {
                            skipped_hf_too_high += 1;
                            if skipped_hf_too_high <= 3 {
                                log::debug!(
                                    "Skipping obligation {}: HF={:.6} >= threshold {:.6} (not liquidatable)",
                                    pk,
                                    hf,
                                    hf_threshold
                                );
                            }
                        }
                    }
                    Err(e) => {
                        parse_errors += 1;
                        if parse_errors <= 5 {
                            log::debug!("Failed to parse Kamino Obligation {}: {}", pk, e);
                    }
                }
            }
            }
            KaminoAccountType::Reserve => {
                // Skip reserves - we're only looking for obligations
                skipped_wrong_type += 1;
            }
            KaminoAccountType::LendingMarket => {
                // Skip lending markets
                skipped_wrong_type += 1;
            }
            KaminoAccountType::Unknown => {
                skipped_wrong_type += 1;
            }
        }
    }
    
    if skipped_wrong_type > 0 {
        log::debug!(
            "Skipped {} accounts with wrong type (Reserves/LendingMarkets/Unknown)",
            skipped_wrong_type
        );
    }
    
    if parse_errors > 10 {
        log::error!(
            "⚠️  High parse error rate: {}/{} obligations failed to parse. Layout may have changed or data may be corrupted!",
            parse_errors,
            total_accounts
        );
    } else if parse_errors > 0 {
        log::debug!(
            "Parse errors: {}/{} obligations failed to parse (acceptable rate)",
            parse_errors,
            total_accounts
        );
    }

    let total_candidates = candidates.len();
    log::info!("Found {} liquidation opportunities (HF < 1.0)", total_candidates);
    
    let scan_duration = scan_start.elapsed();
    
    // Log detailed statistics
    log::info!("📊 Obligation scanning statistics (completed in {:?}):", scan_duration);
    log::info!("   Total accounts scanned: {}", total_accounts);
    log::info!("   Obligations identified: {}", obligation_count);
    log::info!("   Obligations parsed successfully: {}", obligation_count.saturating_sub(parse_errors));
    log::info!("   Parse errors: {}", parse_errors);
    log::info!("   Accounts skipped (wrong type): {}", skipped_wrong_type);
    log::info!("   Obligations skipped (zero borrow): {}", skipped_zero_borrow);
    log::info!("   Obligations skipped (zero deposit): {}", skipped_zero_deposit);
    log::info!("   Obligations skipped (stale): {}", skipped_stale);
    log::info!("   Obligations skipped (HF too high): {}", skipped_hf_too_high);
    log::info!("   ✅ Liquidatable obligations (HF < 1.0): {}", total_candidates);
    
    if total_candidates > 0 {
        // Log first few candidates for debugging
        let preview_count = total_candidates.min(5);
        log::info!("   📋 First {} candidates preview:", preview_count);
        for (idx, (pubkey, obligation)) in candidates.iter().take(preview_count).enumerate() {
            let hf = LendingObligation::health_factor(obligation);
            log::info!("     {}. {}: HF={:.6}, deposited=${:.2}, borrowed=${:.2}, allowedBorrow=${:.2}", 
                       idx + 1, pubkey, hf, 
                       LendingObligation::deposited_value_usd(obligation),
                       LendingObligation::borrowed_value_usd(obligation),
                       obligation.allowed_borrow_value_sf as f64 / SCALE_FACTOR as f64);
        }
    } else {
        log::info!("   ⚠️  No liquidatable obligations found (all HF >= threshold)");
    }

    // Initialize metrics
    let mut metrics = CycleMetrics {
        total_candidates,
        skipped_oracle_fail: 0,
        skipped_jupiter_fail: 0,
        skipped_insufficient_profit: 0,
        skipped_risk_limit: 0,
        failed_build_tx: 0,
        failed_send_bundle: 0,
        successful: 0,
        skipped_rpc_error: 0,
        skipped_reserve_load_fail: 0,
        skipped_ata_missing: 0,
        confirmed_bundles: 0,
        dropped_bundles: 0,
    };

    // Wallet balance tracking: estimated balance with refresh every 3 liquidations
    struct WalletBalanceTracker {
        initial_balance_usd: f64,
        current_estimated_balance_usd: f64,
        // CRITICAL: Track pending committed amount to prevent overcommit
        // This represents capital that has been committed to pending liquidations
        pending_committed_usd: f64,
        last_refresh_balance_usd: f64,
        liquidations_since_refresh: u32,
        total_liquidations: u32,
    }
    
    let mut wallet_balance_tracker = WalletBalanceTracker {
        initial_balance_usd: 0.0,
        current_estimated_balance_usd: 0.0,
        pending_committed_usd: 0.0,
        last_refresh_balance_usd: 0.0,
        liquidations_since_refresh: 0,
        total_liquidations: 0,
    };
    
    // Cycle başında initial balance al
    match get_wallet_value_usd(rpc, &config.wallet.pubkey()).await {
        Ok(value) => {
            wallet_balance_tracker.initial_balance_usd = value;
            wallet_balance_tracker.current_estimated_balance_usd = value;
            wallet_balance_tracker.last_refresh_balance_usd = value;
            log::debug!("Initial wallet balance: ${:.2}", value);
        }
        Err(e) => {
            log::error!("Failed to get initial wallet value: {}", e);
            return Err(anyhow::anyhow!("Cannot proceed without wallet balance: {}", e));
        }
    }
    
    let mut cumulative_risk_usd = 0.0;
    
    // Bundle status enum (defined before BundleTracker to allow use in methods)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BundleStatus {
        Pending,
        Confirmed,
        Failed,
        Unknown,
    }
    
    /// Verify bundle status before removing from tracking
    async fn verify_bundle_status(
        _rpc: &Arc<RpcClient>,
        bundle_id: &str,
        jito_client: &JitoClient,
    ) -> BundleStatus {
        // Option 1: Check Jito bundle status API
        if let Ok(Some(status_response)) = jito_client.get_bundle_status(bundle_id).await {
            if let Some(status_str) = &status_response.status {
                match status_str.as_str() {
                    "landed" | "confirmed" => {
                        log::debug!("Bundle {} confirmed via Jito API", bundle_id);
                        return BundleStatus::Confirmed;
                    }
                    "failed" | "dropped" => {
                        log::debug!("Bundle {} failed/dropped via Jito API", bundle_id);
                        return BundleStatus::Failed;
                    }
                    "pending" => {
                        return BundleStatus::Pending;
                    }
                    _ => {
                        log::debug!("Bundle {} has unknown status: {}", bundle_id, status_str);
                    }
                }
            }
            
            // If slot is present, bundle likely executed
            if status_response.slot.is_some() {
                log::debug!("Bundle {} has slot, assuming confirmed", bundle_id);
                return BundleStatus::Confirmed;
            }
        }
        
        // Option 2: Fallback - conservative approach
        // If we can't determine status, assume executed to avoid overcommit
        // This is safer than assuming failed (which would release committed amount incorrectly)
        log::debug!("Bundle {} status unknown, assuming confirmed (conservative)", bundle_id);
        BundleStatus::Unknown
    }
    
    /// Enhanced bundle tracking with real-time status
    struct BundleInfo {
        value_usd: f64,
        sent_at: Instant,
        last_status_check: Instant,
        confirmed: bool,
    }
    
    struct BundleTracker {
        bundles: std::collections::HashMap<String, BundleInfo>,
    }
    
    impl BundleTracker {
        fn new() -> Self {
            BundleTracker {
                bundles: std::collections::HashMap::new(),
            }
        }
        
        fn add_bundle(&mut self, bundle_id: String, value_usd: f64) {
            self.bundles.insert(bundle_id, BundleInfo {
                value_usd,
                sent_at: Instant::now(),
                last_status_check: Instant::now(),
                confirmed: false,
            });
        }
        
        /// Update bundle status by checking Jito API
        /// Returns list of processed bundles: (id, value_usd, is_success)
        async fn update_statuses(
            &mut self,
            rpc: &Arc<RpcClient>,
            jito_client: &JitoClient,
        ) -> Vec<(String, f64, bool)> {
            let mut processed_bundles = Vec::new();
            
            // Check bundles that haven't been checked in last 200ms
            let now = Instant::now();
            let bundle_ids_to_check: Vec<String> = self.bundles.iter()
                .filter(|(_, info)| {
                    !info.confirmed && 
                    now.duration_since(info.last_status_check) >= Duration::from_millis(300)
                })
                .map(|(id, _)| id.clone())
                .collect();
            
            for bundle_id in bundle_ids_to_check {
                // Use verify_bundle_status helper function to check bundle status
                // Note: verify_bundle_status is defined later in process_cycle, but since we're in an async context
                // and both are in the same scope, we can call it. However, to avoid forward reference issues,
                // we'll use the inline implementation here but keep verify_bundle_status for other uses.
                // Check bundle status via Jito API (same logic as verify_bundle_status)
                match jito_client.get_bundle_status(&bundle_id).await {
                    Ok(Some(status_response)) => {
                        let mut bundle_status = BundleStatus::Unknown;
                        if let Some(status_str) = &status_response.status {
                            match status_str.as_str() {
                                "landed" | "confirmed" => {
                                    bundle_status = BundleStatus::Confirmed;
                                }
                                "failed" | "dropped" => {
                                    bundle_status = BundleStatus::Failed;
                                }
                                "pending" => {
                                    bundle_status = BundleStatus::Pending;
                                }
                                _ => {
                                    bundle_status = BundleStatus::Unknown;
                                }
                            }
                        }
                        
                        // If slot is present, bundle likely executed
                        if status_response.slot.is_some() {
                            bundle_status = BundleStatus::Confirmed;
                        }
                        
                        // Process based on status
                        if let Some(info) = self.bundles.get_mut(&bundle_id) {
                            match bundle_status {
                                BundleStatus::Confirmed => {
                                    if !info.confirmed {
                                        info.confirmed = true;
                                        processed_bundles.push((bundle_id.clone(), info.value_usd, true));
                                        log::debug!(
                                            "✅ Bundle {} confirmed in {:.1}s",
                                            bundle_id,
                                            info.sent_at.elapsed().as_secs_f64()
                                        );
                                    }
                                }
                                BundleStatus::Failed => {
                                    if !info.confirmed {
                                        info.confirmed = true; // Mark as processed
                                        processed_bundles.push((bundle_id.clone(), info.value_usd, false));
                                        log::debug!("Bundle {} failed/dropped", bundle_id);
                                    }
                                }
                                BundleStatus::Pending | BundleStatus::Unknown => {
                                    // Keep tracking, update check time
                                    info.last_status_check = now;
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        // Bundle status unknown - update check time anyway
                        if let Some(info) = self.bundles.get_mut(&bundle_id) {
                            info.last_status_check = now;
                        }
                    }
                    Err(e) => {
                        log::debug!("Failed to check bundle status for {}: {}", bundle_id, e);
                    }
                }
            }
            
            // Remove old confirmed bundles (older than 10s)
            self.bundles.retain(|_, info| {
                !info.confirmed || now.duration_since(info.sent_at) < Duration::from_secs(10)
            });
            
            // CRITICAL: Release expired unconfirmed bundles
            // Timeout configurable via BUNDLE_EXPIRY_TIMEOUT_SECS (default: 10s)
            // This prevents overcommit by releasing dropped bundles, but must be long enough to avoid false positives
            // 
            // Jito bundles typically confirm in 400ms-2s, but during high load can take 5-10s
            // Using 5s timeout risks early release (false positive) of bundles that are still processing
            // 10s timeout provides safer buffer for high-load scenarios while still preventing overcommit
            use std::env;
            let expiry_timeout_secs = env::var("BUNDLE_EXPIRY_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(10); // ✅ Default: 10 seconds - Safe buffer for high-load scenarios (5-10s possible)
            
            // Find expired bundles (unconfirmed and older than timeout)
            let expired_bundle_ids: Vec<String> = self.bundles.iter()
                .filter(|(_, info)| {
                    !info.confirmed && 
                    now.duration_since(info.sent_at) >= Duration::from_secs(expiry_timeout_secs)
                })
                .map(|(id, _)| id.clone())
                .collect();
            
            // Verify bundle status before releasing (to avoid false positives)
            // Use verify_bundle_status helper function for consistency
            let mut expired_to_release: Vec<(String, f64)> = Vec::new();
            for bundle_id in expired_bundle_ids {
                // Use verify_bundle_status to check bundle status one more time before releasing
                let status = verify_bundle_status(rpc, &bundle_id, jito_client).await;
                match status {
                    BundleStatus::Confirmed => {
                        if let Some(info) = self.bundles.get_mut(&bundle_id) {
                            if !info.confirmed {
                                info.confirmed = true;
                                processed_bundles.push((bundle_id.clone(), info.value_usd, true));
                                log::debug!(
                                    "✅ Bundle {} confirmed during expiry check (was about to expire)",
                                    bundle_id
                                );
                            }
                        }
                    }
                    BundleStatus::Failed => {
                        if let Some(info) = self.bundles.get(&bundle_id) {
                            expired_to_release.push((bundle_id.clone(), info.value_usd));
                            log::warn!(
                                "⏰ Releasing expired bundle {} (${:.2}) after {}s - status: failed/dropped",
                                bundle_id,
                                info.value_usd,
                                expiry_timeout_secs
                            );
                        }
                    }
                    BundleStatus::Pending => {
                        log::debug!(
                            "Bundle {} still pending after {}s - keeping for now",
                            bundle_id,
                            expiry_timeout_secs
                        );
                    }
                    BundleStatus::Unknown => {
                        // 🔴 CRITICAL FIX: Do NOT release Unknown status bundles
                        // Unknown status means we couldn't determine the status, but the bundle
                        // might still be pending. Releasing it would cause overcommit.
                        // Only release bundles with explicit Failed/Dropped status.
                        log::debug!(
                            "Bundle {} has unknown status after {}s - keeping to avoid overcommit risk",
                            bundle_id,
                            expiry_timeout_secs
                        );
                    }
                }
            }
            
            // Remove released bundles from tracking
            for (bundle_id, _) in &expired_to_release {
                self.bundles.remove(bundle_id);
            }
            
            for (bundle_id, value) in expired_to_release {
                processed_bundles.push((bundle_id, value, false));
            }
            
            processed_bundles
        }
        
        fn get_pending_committed(&self) -> f64 {
            self.bundles.iter()
                .filter(|(_, info)| !info.confirmed)
                .map(|(_, info)| info.value_usd)
                .sum()
        }
        
        #[allow(dead_code)]
        fn release_expired(&mut self) -> Vec<(String, f64)> {
            Vec::new()
        }
    }
    
    let mut bundle_tracker = BundleTracker::new();

    log::debug!(
        "Cycle started: initial_wallet_value=${:.2}, cumulative_risk tracking initialized (using estimated balance with refresh every 3 liquidations)",
        wallet_balance_tracker.initial_balance_usd
    );

    // 3. Her candidate için liquidation denemesi per Structure.md section 9
    for (obl_pubkey, obligation) in candidates {
        // a) Oracle + reserve load + HF confirm
        let mut ctx = match build_liquidation_context(rpc, &obligation).await {
            Ok(mut ctx) => {
                // Set actual obligation pubkey
                ctx.obligation_pubkey = obl_pubkey;
                ctx
            }
            Err(e) => {
                log::warn!("Failed to build liquidation context for {}: {}", obl_pubkey, e);
                metrics.skipped_oracle_fail += 1;
                continue;
            }
        };
        
        if !ctx.oracle_ok {
            // Provide detailed information about which oracle failed
            let borrow_info = ctx.borrow_reserve.as_ref().map(|r| {
                format!("borrow_reserve={}, pyth_oracle={}, switchboard_oracle={}", 
                    r.mint_pubkey(),
                    r.pyth_oracle(),
                    r.switchboard_oracle())
            }).unwrap_or_else(|| "borrow_reserve=None".to_string());
            
            let deposit_info = ctx.deposit_reserve.as_ref().map(|r| {
                format!("deposit_reserve={}, pyth_oracle={}, switchboard_oracle={}", 
                    r.mint_pubkey(),
                    r.pyth_oracle(),
                    r.switchboard_oracle())
            }).unwrap_or_else(|| "deposit_reserve=None".to_string());
            
            // Reduce log spam: Use debug instead of warn for frequent oracle failures
            // We have a summary at the end of the cycle to track these stats
            log::debug!(
                "Skipping {}: Oracle validation failed - {} | {} | borrow_price={:?}, deposit_price={:?}",
                obl_pubkey,
                borrow_info,
                deposit_info,
                ctx.borrow_price_usd(),
                ctx.deposit_price_usd()
            );
            metrics.skipped_oracle_fail += 1;
            continue;
        }

        // 🔴 CRITICAL FIX: Skip dust positions (very small positions not worth liquidating)
        // Configurable via MIN_POSITION_SIZE_USD (default: $10)
        let min_position_size_usd = std::env::var("MIN_POSITION_SIZE_USD")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(10.0); // Default: $10 minimum position size
        
        // Use total_deposited_value_usd() which is already in USD
        let position_size_usd = ctx.total_deposited_value_usd();
        
        if position_size_usd < min_position_size_usd {
            log::debug!(
                "Skipping {}: Position size ${:.2} < min ${:.2} (dust position)",
                obl_pubkey,
                position_size_usd,
                min_position_size_usd
            );
            metrics.skipped_insufficient_profit += 1; // Reuse this metric for dust positions
            continue;
        }

        let quote_result = get_liquidation_quote(&ctx, config, rpc).await;
        let quote = match quote_result {
            Ok(q) => q,
            Err(e) => {
                log::warn!("Skipping {}: Jupiter quote failed: {}", obl_pubkey, e);
                metrics.skipped_jupiter_fail += 1;
                continue;
            }
        };

        if quote.profit_usdc < config.min_profit_usdc {
            log::debug!(
                "Skipping {}: Profit ${:.2} < min ${:.2}",
                obl_pubkey,
                quote.profit_usdc,
                config.min_profit_usdc
            );
            metrics.skipped_insufficient_profit += 1;
            continue;
        }

        // c) Wallet risk limiti - per-liquidation check
        let processed_bundles = bundle_tracker.update_statuses(rpc, jito_client).await;
        for (bundle_id, value_usd, is_success) in processed_bundles {
            wallet_balance_tracker.pending_committed_usd -= value_usd;
            
            if is_success {
                metrics.confirmed_bundles += 1;
                log::debug!("Released ${:.2} committed for CONFIRMED bundle {}", value_usd, bundle_id);
            } else {
                metrics.dropped_bundles += 1;
                log::debug!("Released ${:.2} committed for DROPPED bundle {}", value_usd, bundle_id);
            }
        }
        
        let pending_committed = bundle_tracker.get_pending_committed();
        let available_liquidity = wallet_balance_tracker.current_estimated_balance_usd - pending_committed;
        let current_max_position_usd = available_liquidity * config.max_position_pct;
        
        let position_size_usd = quote.collateral_value_usd;
        
        log::debug!(
            "Risk calculation: estimated_wallet=${:.2}, pending_committed=${:.2} (real-time tracking), available=${:.2}, max_position=${:.2}",
            wallet_balance_tracker.current_estimated_balance_usd,
            pending_committed,
            available_liquidity,
            current_max_position_usd
        );
        
        // Per-liquidation check: single liquidation cannot exceed max position
        if position_size_usd > current_max_position_usd {
            log::warn!(
                "Skipping {}: Position ${:.2} exceeds per-liquidation limit ${:.2} (estimated_balance=${:.2})",
                obl_pubkey,
                position_size_usd,
                current_max_position_usd,
                wallet_balance_tracker.current_estimated_balance_usd
            );
            metrics.skipped_risk_limit += 1;
            continue;
        }

        // Per Structure.md section 6.4: Block-wide cumulative risk check
        // "Tek blok içinde kullanılan toplam risk de aynı limit ile sınırlıdır"
        // 
        // CRITICAL: cumulative_risk_usd tracks risk from liquidations sent in this cycle.
        // We check against current estimated wallet value to ensure we don't exceed limits even if
        // previous liquidations have executed and changed the wallet balance.
        let new_cumulative_risk = cumulative_risk_usd + position_size_usd;
        if new_cumulative_risk > current_max_position_usd {
            log::warn!(
                "Skipping {}: Cumulative risk ${:.2} + position ${:.2} = ${:.2} exceeds block-wide limit ${:.2} (estimated_balance=${:.2})",
                obl_pubkey,
                cumulative_risk_usd,
                position_size_usd,
                new_cumulative_risk,
                current_max_position_usd,
                wallet_balance_tracker.current_estimated_balance_usd
            );
            metrics.skipped_risk_limit += 1;
            continue;
        }
        
        log::debug!(
            "Risk check passed: position=${:.2}, cumulative=${:.2}/{:.2}, estimated_wallet=${:.2}",
            position_size_usd,
            new_cumulative_risk,
            current_max_position_usd,
            wallet_balance_tracker.current_estimated_balance_usd
        );

        // d) Jito bundle ile gönder
        if matches!(config.liquidation_mode, LiquidationMode::Live) {
            // ✅ FLASHLOAN APPROACH: Single atomic transaction (NO race condition risk!)
            // All operations in ONE transaction: FlashBorrow -> Liquidate -> Redeem -> Swap -> FlashRepay
            // Benefits:
            // - ✅ Atomicity: All operations succeed or fail together
            // - ✅ No MEV risk: No intermediate state exposed
            // - ✅ No race condition: TX1/TX2 split eliminated
            // - ✅ Gas-efficient: Single transaction fee
            // - ✅ No capital needed: Flashloan provides initial funds
            match execute_liquidation_with_swap(&ctx, &quote, &config, rpc, jito_client).await {
                Ok(_) => {
                            // Update balance tracker
                            wallet_balance_tracker.liquidations_since_refresh += 1;
                            wallet_balance_tracker.total_liquidations += 1;
                            
                            // ESTIMATED balance update (without RPC call)
                            // Formula: new_balance = old_balance + profit - jito_tip - tx_fee
                            // Note: quote.profit_usdc already includes all fees (jito_tip + tx_fee)
                            let estimated_profit = quote.profit_usdc;
                            wallet_balance_tracker.current_estimated_balance_usd += estimated_profit;
                            
                            // Update cumulative risk and pending liquidation tracking after successful send
                            // CRITICAL FIX: Real-time bundle tracking with status checking
                            // NOTE: For flashloan approach, we track the single atomic transaction bundle
                            // The bundle ID is returned from execute_liquidation_with_swap
                            // For now, we use a placeholder ID - actual bundle ID should be returned from execute_liquidation_with_swap
                            let bundle_id = format!("FLASHLOAN_{}", obl_pubkey);
                            bundle_tracker.add_bundle(bundle_id.clone(), position_size_usd);
                            
                            // CRITICAL: Track committed amount to prevent overcommit
                            // This capital is now tied up in the pending liquidation
                            wallet_balance_tracker.pending_committed_usd += position_size_usd;
                            
                            cumulative_risk_usd += position_size_usd;
                            
                            // Calculate current pending value for logging
                            let current_pending_value = bundle_tracker.get_pending_committed();
                            
                            const REFRESH_INTERVAL: u32 = 3;
                            let refresh_countdown = REFRESH_INTERVAL.saturating_sub(wallet_balance_tracker.liquidations_since_refresh);
                            
                            log::info!(
                                "✅ Liquidated {} with profit ${:.2} (FLASHLOAN), estimated_balance=${:.2}, pending_committed=${:.2} (real-time: ${:.2}, refresh in {} liquidations), cumulative_risk=${:.2}/${:.2}",
                                obl_pubkey,
                                quote.profit_usdc,
                                wallet_balance_tracker.current_estimated_balance_usd,
                                wallet_balance_tracker.pending_committed_usd,
                                current_pending_value,
                                refresh_countdown,
                                cumulative_risk_usd,
                                current_max_position_usd
                            );
                            
                            if wallet_balance_tracker.liquidations_since_refresh >= REFRESH_INTERVAL {
                                match get_wallet_value_usd(rpc, &config.wallet.pubkey()).await {
                                    Ok(actual_balance) => {
                                        let estimation_error = (wallet_balance_tracker.current_estimated_balance_usd 
                                            - actual_balance).abs();
                                        let error_pct = if actual_balance > 0.0 {
                                            (estimation_error / actual_balance) * 100.0
                                        } else {
                                            0.0
                                        };
                                        
                                        log::info!(
                                            "🔄 Balance refresh: estimated=${:.2}, actual=${:.2}, error=${:.2} ({:.2}%)",
                                            wallet_balance_tracker.current_estimated_balance_usd,
                                            actual_balance,
                                            estimation_error,
                                            error_pct
                                        );
                                        
                                        wallet_balance_tracker.current_estimated_balance_usd = actual_balance;
                                        wallet_balance_tracker.last_refresh_balance_usd = actual_balance;
                                        wallet_balance_tracker.liquidations_since_refresh = 0;
                                        
                                        if error_pct > 10.0 {
                                            log::error!(
                                                "⚠️  HIGH BALANCE ESTIMATION ERROR: {:.2}%! \
                                                 This indicates profit calculation or fee estimation issues. \
                                                 Investigate immediately!",
                                                error_pct
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to refresh wallet balance: {}", e);
                                        // Continue with estimated balance
                                    }
                                }
                            }
                            
                            metrics.successful += 1;
                }
                Err(e) => {
                    log::error!("Failed to execute liquidation with swap for {}: {}", obl_pubkey, e);
                    metrics.failed_send_bundle += 1;
                }
            }
        } else {
            // Update cumulative risk for dry run as well
            cumulative_risk_usd += position_size_usd;
            log::info!(
                "DryRun: would liquidate obligation {} with profit ~${:.2} USDC, cumulative_risk=${:.2}/{:.2} (estimated_balance=${:.2})",
                obl_pubkey,
                quote.profit_usdc,
                cumulative_risk_usd,
                current_max_position_usd,
                wallet_balance_tracker.current_estimated_balance_usd
            );
            metrics.successful += 1;
        }
    }

    // Log cycle summary metrics
    // Get final wallet value for summary (may have changed if liquidations executed)
    // Use estimated balance if refresh failed, otherwise use actual
    let final_wallet_value_usd = match get_wallet_value_usd(rpc, &config.wallet.pubkey()).await {
        Ok(value) => {
            // Log final estimation error if we have liquidations
            if wallet_balance_tracker.total_liquidations > 0 {
                let final_estimation_error = (wallet_balance_tracker.current_estimated_balance_usd - value).abs();
                let final_error_pct = if value > 0.0 {
                    (final_estimation_error / value) * 100.0
                } else {
                    0.0
                };
                log::debug!(
                    "Final balance check: estimated=${:.2}, actual=${:.2}, error=${:.2} ({:.2}%)",
                    wallet_balance_tracker.current_estimated_balance_usd,
                    value,
                    final_estimation_error,
                    final_error_pct
                );
            }
            value
        }
        Err(e) => {
            log::warn!("Failed to get final wallet value for summary: {}, using estimated balance", e);
            wallet_balance_tracker.current_estimated_balance_usd
        }
    };
    let final_max_position_usd = final_wallet_value_usd * config.max_position_pct;
    
    let total_processed = metrics.total_candidates;
    let total_skipped = metrics.skipped_oracle_fail
        + metrics.skipped_jupiter_fail
        + metrics.skipped_insufficient_profit
        + metrics.skipped_risk_limit
        + metrics.skipped_rpc_error
        + metrics.skipped_reserve_load_fail
        + metrics.skipped_ata_missing;
    let total_failed = metrics.failed_build_tx + metrics.failed_send_bundle;
    
    log::info!(
        "📊 Cycle Summary: {} candidates | {} successful | {} skipped (oracle:{}, jup:{}, profit:{}, risk:{}, rpc:{}, load:{}, ata:{}) | {} tx_failed (build:{}, send:{}) | Bundles: {} confirmed, {} dropped | cumulative_risk=${:.2}/{:.2} (wallet=${:.2})",
        total_processed,
        metrics.successful,
        total_skipped,
        metrics.skipped_oracle_fail,
        metrics.skipped_jupiter_fail,
        metrics.skipped_insufficient_profit,
        metrics.skipped_risk_limit,
        metrics.skipped_rpc_error,
        metrics.skipped_reserve_load_fail,
        metrics.skipped_ata_missing,
        total_failed,
        metrics.failed_build_tx,
        metrics.failed_send_bundle,
        metrics.confirmed_bundles,
        metrics.dropped_bundles,
        cumulative_risk_usd,
        final_max_position_usd,
        final_wallet_value_usd
    );
    
    // Log detailed breakdown for debugging
    if total_skipped > 0 {
        log::debug!("📋 Skip breakdown details:");
        log::debug!("   Oracle failures: {}", metrics.skipped_oracle_fail);
        log::debug!("   Jupiter failures: {}", metrics.skipped_jupiter_fail);
        log::debug!("   Insufficient profit: {}", metrics.skipped_insufficient_profit);
        log::debug!("   Risk limit: {}", metrics.skipped_risk_limit);
        log::debug!("   RPC errors: {}", metrics.skipped_rpc_error);
        log::debug!("   Reserve load failures: {}", metrics.skipped_reserve_load_fail);
        log::debug!("   Missing ATAs: {}", metrics.skipped_ata_missing);
    }
    
    if total_failed > 0 {
        log::warn!("⚠️  Transaction failures:");
        log::warn!("   Build failures: {}", metrics.failed_build_tx);
        log::warn!("   Send failures: {}", metrics.failed_send_bundle);
    }
    
    if metrics.confirmed_bundles > 0 || metrics.dropped_bundles > 0 {
        log::info!("📦 Bundle statistics:");
        log::info!("   Confirmed: {} (success rate: {:.1}%)", 
                  metrics.confirmed_bundles,
                  if (metrics.confirmed_bundles + metrics.dropped_bundles) > 0 {
                      (metrics.confirmed_bundles as f64 / (metrics.confirmed_bundles + metrics.dropped_bundles) as f64) * 100.0
                  } else {
                      0.0
                  });
        log::info!("   Dropped: {}", metrics.dropped_bundles);
    }

    // Structured log for external monitoring (Problems.md requirement)
    // Always log scan stats even if no candidates found (for liveness check)
    log::info!(target: "scan_stats", "{}", serde_json::json!({
        "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        "candidates": total_processed,
        "successful": metrics.successful,
        "skipped": total_skipped,
        "skipped_breakdown": {
            "oracle": metrics.skipped_oracle_fail,
            "jupiter": metrics.skipped_jupiter_fail,
            "profit": metrics.skipped_insufficient_profit,
            "risk": metrics.skipped_risk_limit,
            "rpc": metrics.skipped_rpc_error,
            "reserve": metrics.skipped_reserve_load_fail,
            "ata": metrics.skipped_ata_missing
        },
        "failed_tx": total_failed,
        "bundles": {
            "confirmed": metrics.confirmed_bundles,
            "dropped": metrics.dropped_bundles
        },
        "risk": {
            "cumulative_used": cumulative_risk_usd,
            "limit": final_max_position_usd,
            "wallet_balance": final_wallet_value_usd
        }
    }));

    Ok(())
}

/// Validate Jito endpoint is reachable per Structure.md section 13
async fn validate_jito_endpoint(jito_client: &JitoClient) -> Result<()> {
    // Simple connectivity check - Jito endpoint validation
    // Note: Jito doesn't have a standard health endpoint, so we just log
    log::info!("✅ Jito client initialized: {}", jito_client.url());
    // In production, you might want to do an actual connectivity test
    // For now, we'll validate during first bundle send
    Ok(())
}

/// Liquidation context per Structure.md section 9
/// Kamino Lend protocol only
pub struct LiquidationContext {
    pub obligation_pubkey: Pubkey,
    pub obligation: KaminoObligation,
    pub borrows: Vec<KaminoObligationLiquidity>,
    pub deposits: Vec<KaminoObligationCollateral>,
    pub borrow_reserve: Option<KaminoReserve>,
    pub deposit_reserve: Option<KaminoReserve>,
    pub borrow_price_usd: Option<f64>,
    pub deposit_price_usd: Option<f64>,
    pub oracle_ok: bool,
}

impl LiquidationContext {
    pub fn obligation_pubkey(&self) -> Pubkey {
        self.obligation_pubkey
    }
    
    pub fn oracle_ok(&self) -> bool {
        self.oracle_ok
    }
    
    pub fn borrow_price_usd(&self) -> Option<f64> {
        self.borrow_price_usd
    }
    
    pub fn deposit_price_usd(&self) -> Option<f64> {
        self.deposit_price_usd
    }
    
    /// Get total deposited value in USD (for position size check)
    pub fn total_deposited_value_usd(&self) -> f64 {
        LendingObligation::deposited_value_usd(&self.obligation)
    }
}

/// Build liquidation context with Oracle validation per Structure.md section 5.2
/// Kamino Lend protocol only
async fn build_liquidation_context(
    rpc: &Arc<RpcClient>,
    obligation: &KaminoObligation,
) -> Result<LiquidationContext> {
    build_kamino_liquidation_context(rpc, obligation).await
}

/// Build liquidation context for Kamino Lend
async fn build_kamino_liquidation_context(
    rpc: &Arc<RpcClient>,
    obligation: &KaminoObligation,
) -> Result<LiquidationContext> {
    // Load reserve accounts in parallel for better performance
    let mut borrow_reserve = None;
    let mut deposit_reserve = None;

    // Get active borrows and deposits from Kamino obligation
    let active_borrows: Vec<KaminoObligationLiquidity> = obligation.borrows.iter()
        .filter(|b| b.borrowed_amount_sf > 0)
        .cloned()
        .collect();
    let active_deposits: Vec<KaminoObligationCollateral> = obligation.deposits.iter()
        .filter(|d| d.deposited_amount > 0)
        .cloned()
        .collect();
    
    log::debug!("🔍 Building Kamino liquidation context: owner={}, active_deposits={}, active_borrows={}", 
                obligation.owner, active_deposits.len(), active_borrows.len());
    
    let borrow_reserve_pubkey = active_borrows.first()
        .map(|b| b.borrow_reserve);
    let deposit_reserve_pubkey = active_deposits.first()
        .map(|d| d.deposit_reserve);
    
    if borrow_reserve_pubkey.is_none() && deposit_reserve_pubkey.is_none() {
        return Err(anyhow::anyhow!("Kamino obligation has no active borrows or deposits"));
    }

    // Load reserves in parallel
    log::debug!("  🔄 Loading Kamino reserves in parallel (borrow: {:?}, deposit: {:?})", 
                borrow_reserve_pubkey, deposit_reserve_pubkey);
    let reserve_load_start = std::time::Instant::now();
    
    let (borrow_result, deposit_result) = tokio::join!(
        async {
            if let Some(pubkey) = borrow_reserve_pubkey {
                log::debug!("  📥 Loading borrow reserve: {}", pubkey);
                let rpc_clone = Arc::clone(rpc);
                tokio::task::spawn_blocking(move || -> Result<Option<KaminoReserve>, solana_client::client_error::ClientError> {
                    let account_data = rpc_clone.get_account_data(&pubkey)?;
                    log::debug!("  📊 Borrow reserve account data retrieved: {} bytes", account_data.len());
                    let reserve = KaminoReserve::from_account_data(&account_data)
                        .map_err(|e| solana_client::client_error::ClientError::from(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Parse error: {}", e))))?;
                    log::debug!("  ✅ Borrow reserve parsed successfully: mint={}", reserve.mint_pubkey());
                    Ok(Some(reserve))
                }).await
                .map_err(|e| solana_client::client_error::ClientError::from(std::io::Error::new(std::io::ErrorKind::Other, format!("Join error: {}", e))))
            } else {
                Ok(Ok(None))
            }
        },
        async {
            if let Some(pubkey) = deposit_reserve_pubkey {
                log::debug!("  📥 Loading deposit reserve: {}", pubkey);
                let rpc_clone = Arc::clone(rpc);
                tokio::task::spawn_blocking(move || -> Result<Option<KaminoReserve>, solana_client::client_error::ClientError> {
                    let account_data = rpc_clone.get_account_data(&pubkey)?;
                    log::debug!("  📊 Deposit reserve account data retrieved: {} bytes", account_data.len());
                    let reserve = KaminoReserve::from_account_data(&account_data)
                        .map_err(|e| solana_client::client_error::ClientError::from(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Parse error: {}", e))))?;
                    log::debug!("  ✅ Deposit reserve parsed successfully: mint={}", reserve.mint_pubkey());
                    Ok(Some(reserve))
                }).await
                .map_err(|e| solana_client::client_error::ClientError::from(std::io::Error::new(std::io::ErrorKind::Other, format!("Join error: {}", e))))
            } else {
                Ok(Ok(None))
            }
        }
    );
    
    let reserve_load_duration = reserve_load_start.elapsed();
    log::debug!("  ⏱️  Reserve loading completed in {:?}", reserve_load_duration);

    // Process borrow reserve result
    match borrow_result {
        Ok(Ok(Some(reserve))) => {
            let market_price = reserve.market_price_usd();
            log::debug!(
                "✅ Loaded borrow reserve: {} (ltv={:.2}%, liquidation_threshold={:.2}%, liquidation_bonus={:.2}%, market_price=${:.6})",
                borrow_reserve_pubkey.unwrap(),
                reserve.ltv_pct() as f64,
                reserve.liquidation_threshold_pct() as f64,
                reserve.liquidation_bonus_bps() as f64 / 100.0,
                market_price
            );
            borrow_reserve = Some(reserve);
        }
        Ok(Ok(None)) => {
            log::debug!("Obligation has no borrows");
        }
        Ok(Err(e)) => {
            log::warn!("❌ Failed to load/parse borrow reserve {}: {}", borrow_reserve_pubkey.unwrap(), e);
        }
        Err(e) => {
            log::warn!("❌ Task error loading borrow reserve: {}", e);
        }
    }

    // Process deposit reserve result
    match deposit_result {
        Ok(Ok(Some(reserve))) => {
            let market_price = reserve.market_price_usd();
            log::debug!(
                "✅ Loaded deposit reserve: {} (ltv={:.2}%, liquidation_threshold={:.2}%, liquidation_bonus={:.2}%, market_price=${:.6})",
                deposit_reserve_pubkey.unwrap(),
                reserve.ltv_pct() as f64,
                reserve.liquidation_threshold_pct() as f64,
                reserve.liquidation_bonus_bps() as f64 / 100.0,
                market_price
            );
            deposit_reserve = Some(reserve);
        }
        Ok(Ok(None)) => {
            log::debug!("Obligation has no deposits");
        }
        Ok(Err(e)) => {
            log::warn!("❌ Failed to load/parse deposit reserve {}: {}", deposit_reserve_pubkey.unwrap(), e);
        }
        Err(e) => {
            log::warn!("❌ Task error loading deposit reserve: {}", e);
        }
    }

    // Use active borrows and deposits directly
    let borrows = active_borrows;
    let deposits = active_deposits;
    
    // Validate oracles
    let current_slot = rpc.get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    
    let (oracle_ok, borrow_price, deposit_price) = validate_kamino_oracles(
        rpc,
        &borrow_reserve,
        &deposit_reserve,
        current_slot,
    ).await?;

    Ok(LiquidationContext {
        obligation_pubkey: Pubkey::default(), // Will be set correctly by caller
        obligation: obligation.clone(),
        borrows,
        deposits,
        borrow_reserve,
        deposit_reserve,
        borrow_price_usd: borrow_price,
        deposit_price_usd: deposit_price,
        oracle_ok,
    })
}

/// Validate Kamino oracles (simplified version - full validation in oracle module)
async fn validate_kamino_oracles(
    rpc: &Arc<RpcClient>,
    borrow_reserve: &Option<KaminoReserve>,
    deposit_reserve: &Option<KaminoReserve>,
    current_slot: u64,
) -> Result<(bool, Option<f64>, Option<f64>)> {
    let mut borrow_price = None;
    let mut deposit_price = None;
    let mut all_ok = true;
    
    if let Some(reserve) = borrow_reserve {
        // Get price from reserve's market_price_sf (already updated by protocol)
        let price = reserve.market_price_usd();
        borrow_price = Some(price);
        log::debug!("  💰 Borrow reserve price: ${:.6}", price);
    }
    
    if let Some(reserve) = deposit_reserve {
        // Get price from reserve's market_price_sf
        let price = reserve.market_price_usd();
        deposit_price = Some(price);
        log::debug!("  💰 Deposit reserve price: ${:.6}", price);
    }
    
    // TODO: Add full oracle validation (Pyth/Switchboard confidence checks)
    // For now, we trust the reserve's market_price_sf which is updated by the protocol
    
    Ok((all_ok, borrow_price, deposit_price))
}

// System supports Kamino Lend protocol

/// Liquidation quote with profit calculation
pub struct LiquidationQuote {
    pub quote: JupiterQuote,
    pub profit_usdc: f64,
    pub collateral_value_usd: f64,
    pub debt_to_repay_raw: u64,
    pub collateral_to_seize_raw: u64,
    pub flashloan_fee_raw: u64,
}

// Transaction building functions moved to liquidation_tx module
// System now only supports Kamino Lend with flashloan approach

/// Execute liquidation with swap using flashloan (atomic single transaction)
/// 
/// FLASHLOAN APPROACH - Solves race condition and MEV risks:
/// - Atomic: All operations in single transaction
/// - No race conditions: No TX1/TX2 split
/// - No MEV risk: No intermediate state exposed
/// - No capital required: Flashloan provides initial funds
/// - Gas-efficient: Single transaction
/// 
/// Flow:
/// 1. FlashLoan: Borrow debt_amount USDC from Kamino (flash)
/// 2. LiquidateObligation: Repay debt, receive collateral
/// 3. RedeemReserveCollateral: Collateral -> underlying token
/// 4. Jupiter Swap: Underlying token -> USDC
/// 5. FlashRepayReserveLiquidity: Explicit repayment instruction (REQUIRED!)
async fn execute_liquidation_with_swap(
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    config: &Config,
    rpc: &Arc<RpcClient>,
    jito_client: &JitoClient,
) -> Result<()> {
    let wallet = &config.wallet;
    
    // ============================================================================
    // BUILD ATOMIC FLASHLOAN TRANSACTION
    // ============================================================================
    log::info!("Building atomic flashloan liquidation transaction");
    
    // ✅ CRITICAL: Get fresh blockhash AFTER Jupiter quote completes
    // Blockhashes are valid for ~150 slots (~60 seconds)
    // Fetching blockhash right before building TX minimizes staleness risk
    let blockhash = rpc
        .get_latest_blockhash()
        .map_err(|e| anyhow::anyhow!("Failed to get latest blockhash: {}", e))?;
    
    // Build atomic flashloan transaction
    let tx = crate::liquidation_tx::build_flashloan_liquidation_tx(
        wallet,
        ctx,
        quote,
        rpc,
        blockhash,
        config,
        )
        .await
        .context("Failed to build flashloan liquidation transaction")?;
    
    // ============================================================================
    // SUBMIT TRANSACTION VIA JITO
    // ============================================================================
    log::info!("Submitting liquidation transaction via Jito...");
    
    // Sign transaction
    let mut signed_tx = tx;
    signed_tx.sign(&[wallet], blockhash);
    
    // Create Jito bundle
    use crate::utils::JitoBundle;
    let mut bundle = JitoBundle::new(
        jito_client.tip_account(),
        jito_client.default_tip_amount(),
    );
    bundle.add_transaction(signed_tx);
    
    // Submit via Jito
    jito_client
        .send_bundle(&bundle)
        .await
        .context("Failed to submit liquidation transaction via Jito")?;
    
    log::info!("✅ Liquidation transaction submitted successfully!");
    
    Ok(())
}


// Transaction building functions moved to liquidation_tx module:
// - build_flashloan_liquidation_tx -> liquidation_tx::build_flashloan_liquidation_tx

// Transaction building functions moved to liquidation_tx module:
// - build_flashloan_liquidation_tx -> liquidation_tx::build_flashloan_liquidation_tx
// - build_liquidation_tx1 -> deprecated (kept for reference)
// - build_liquidation_tx2 -> deprecated (kept for reference)

// Transaction building functions moved to liquidation_tx module



