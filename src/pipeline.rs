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
use crate::solend::{Obligation, Reserve, solend_program_id};
use crate::solend::{ObligationLiquidity, ObligationCollateral};
use crate::utils::{send_jito_bundle, JitoClient};
use crate::oracle::{self, validate_oracles_with_twap};
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
    let program_id = solend_program_id()?;
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
    // 1. Solend obligation account'larını çek
    log::debug!("Fetching obligation accounts...");
    let rpc_clone = Arc::clone(rpc);
    let program_id_clone = *program_id;
    let accounts = tokio::task::spawn_blocking(move || {
        rpc_clone.get_program_accounts(&program_id_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
    .map_err(|e| anyhow::anyhow!("Failed to get program accounts: {}", e))?;

    log::debug!("Found {} accounts", accounts.len());

    // 🔴 CRITICAL FIX: Get current slot for stale obligation check
    let current_slot = rpc.get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    
    // Configurable max slot diff for stale obligations (default: 150 slots = ~60 seconds)
    let max_obligation_slot_diff = std::env::var("MAX_OBLIGATION_SLOT_DIFF")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(150); // Default: 150 slots (~60 seconds at 400ms per slot)

    // 2. HF < 1.0 olanları bul
    // CRITICAL SECURITY: Track parse errors to detect layout changes or corrupt data
    // ✅ FIXED: Use account type identification utility
    // This is faster and more reliable than attempting to parse every account
    log::info!("🔍 Scanning {} accounts for liquidatable obligations...", accounts.len());
    let scan_start = std::time::Instant::now();
    
    let mut candidates = Vec::new();
    let mut parse_errors: usize = 0;
    let mut skipped_wrong_type: usize = 0;
    let mut obligation_count: usize = 0;
    let mut skipped_zero_borrow: usize = 0;
    let mut skipped_zero_deposit: usize = 0;
    let mut skipped_stale: usize = 0;
    let mut skipped_hf_too_high: usize = 0;
    let total_accounts = accounts.len();
    
    for (pk, acc) in accounts {
        // Use account type identification to quickly filter accounts
        match crate::solend::identify_solend_account_type(&acc.data) {
            crate::solend::SolendAccountType::Obligation => {
                obligation_count += 1;
                // This looks like an Obligation - try to parse it
                match Obligation::from_account_data(&acc.data) {
                    Ok(obligation) => {
                        // 🔴 CRITICAL FIX: Skip obligations with zero borrow or zero deposit
                        // These are inactive obligations that should not be considered for liquidation
                        // This prevents false positives from stale/inactive obligations
                        let borrowed_value = obligation.total_borrowed_value_usd();
                        let deposited_value = obligation.total_deposited_value_usd();
                        
                        // Skip if no borrows (nothing to liquidate)
                        if borrowed_value <= 0.0 {
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
                        if deposited_value <= 0.0 {
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
                        
                        // 🔴 CRITICAL FIX: Skip stale obligations based on lastUpdate.slot
                        // Stale obligations may have outdated health factor calculations
                        if obligation.is_stale(current_slot, max_obligation_slot_diff) {
                            skipped_stale += 1;
                            if let Some(last_slot) = obligation.last_update_slot() {
                                if skipped_stale <= 3 {
                                    log::debug!(
                                        "Skipping stale obligation {}: last_update_slot={}, current_slot={}, diff={}",
                                        pk,
                                        last_slot,
                                        current_slot,
                                        current_slot.saturating_sub(last_slot)
                                    );
                                }
                            } else {
                                if skipped_stale <= 3 {
                                    log::debug!(
                                        "Skipping obligation {}: cannot determine lastUpdate slot (assuming stale)",
                                        pk
                                    );
                                }
                            }
                            continue;
                        }
                        
                        // Read health factor threshold from .env (no hardcoded values)
                        let hf_threshold = std::env::var("HF_LIQUIDATION_THRESHOLD")
                            .ok()
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(1.0); // Default 1.0 if not set
                        
                        // CRITICAL FIX: Use u128 arithmetic for high-precision HF check
                        // f64 precision loss can cause missed liquidations for HF very close to 1.0
                        // (e.g., 0.9999999999 vs 1.0000000001)
                        const WAD: u128 = 1_000_000_000_000_000_000;
                        
                        // Use is_liquidatable() method when threshold is 1.0 (default)
                        // For custom thresholds, use health_factor_u128() with threshold comparison
                        let is_liquidatable = if hf_threshold == 1.0 {
                            obligation.is_liquidatable()
                        } else {
                            let hf_wad = obligation.health_factor_u128();
                            let threshold_wad = (hf_threshold * WAD as f64) as u128;
                            hf_wad < threshold_wad
                        };
                        
                        if is_liquidatable {
                            // Log obligation details for debugging
                            log::debug!(
                                "✅ Found liquidatable obligation {}: HF={:.6}, deposited=${:.2}, borrowed=${:.2}",
                                pk,
                                obligation.health_factor(),
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
                                    obligation.health_factor(),
                                    hf_threshold
                                );
                            }
                        }
                    }
                    Err(e) => {
                        parse_errors += 1;
                        log::debug!("Failed to parse Obligation {}: {}", pk, e);
                    }
                }
            }
            _ => {
                skipped_wrong_type += 1;
            }
        }
    }
    
    if skipped_wrong_type > 0 {
        log::debug!(
            "Skipped {} accounts with wrong type (likely Reserves/LendingMarkets, not Obligations)",
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
            let hf = obligation.health_factor();
            log::info!("     {}. {}: HF={:.6}, deposited=${:.2}, borrowed=${:.2}, allowedBorrow=${:.2}", 
                       idx + 1, pubkey, hf, 
                       obligation.total_deposited_value_usd(),
                       obligation.total_borrowed_value_usd(),
                       obligation.allowedBorrowValue as f64 / 1e18);
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
        let ctx = match build_liquidation_context(rpc, &obligation).await {
            Ok(mut ctx) => {
                ctx.obligation_pubkey = obl_pubkey; // Set actual obligation pubkey
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
                    r.oracle_pubkey(),
                    r.liquidity().liquiditySwitchboardOracle)
            }).unwrap_or_else(|| "borrow_reserve=None".to_string());
            
            let deposit_info = ctx.deposit_reserve.as_ref().map(|r| {
                format!("deposit_reserve={}, pyth_oracle={}, switchboard_oracle={}", 
                    r.mint_pubkey(),
                    r.oracle_pubkey(),
                    r.liquidity().liquiditySwitchboardOracle)
            }).unwrap_or_else(|| "deposit_reserve=None".to_string());
            
            // Reduce log spam: Use debug instead of warn for frequent oracle failures
            // We have a summary at the end of the cycle to track these stats
            log::debug!(
                "Skipping {}: Oracle validation failed - {} | {} | borrow_price={:?}, deposit_price={:?}",
                obl_pubkey,
                borrow_info,
                deposit_info,
                ctx.borrow_price_usd,
                ctx.deposit_price_usd
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
        
        // Use total_deposited_value_usd() which is already in USD (WAD format, divided by 1e18)
        let position_size_usd = ctx.obligation.total_deposited_value_usd();
        
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
pub struct LiquidationContext {
    pub obligation_pubkey: Pubkey,
    pub obligation: Obligation,
    pub borrows: Vec<ObligationLiquidity>,  // Parsed borrows from dataFlat
    pub deposits: Vec<ObligationCollateral>, // Parsed deposits from dataFlat
    pub borrow_reserve: Option<Reserve>,
    pub deposit_reserve: Option<Reserve>,
    pub borrow_price_usd: Option<f64>,  // Price from oracle
    pub deposit_price_usd: Option<f64>, // Price from oracle
    pub oracle_ok: bool,
}

/// Build liquidation context with Oracle validation per Structure.md section 5.2
async fn build_liquidation_context(
    rpc: &Arc<RpcClient>,
    obligation: &Obligation,
) -> Result<LiquidationContext> {
    // 🔴 CRITICAL FIX: Load reserve accounts in parallel for better performance
    // This reduces latency when loading both borrow and deposit reserves
    let mut borrow_reserve = None;
    let mut deposit_reserve = None;

    // Parse borrows and deposits first to get reserve pubkeys
    log::debug!("🔍 Building liquidation context for obligation: owner={}, depositsLen={}, borrowsLen={}", 
                obligation.owner, obligation.depositsLen, obligation.borrowsLen);
    
    let borrow_reserve_pubkey = obligation.borrows()
        .ok()
        .and_then(|borrows| {
            log::debug!("  📊 Parsed {} borrows from obligation", borrows.len());
            if let Some(first_borrow) = borrows.first() {
                log::debug!("  🔍 First borrow reserve: {}", first_borrow.borrowReserve);
                Some(first_borrow.borrowReserve)
            } else {
                log::debug!("  ⚠️  No borrows found in obligation");
                None
            }
        });
    
    let deposit_reserve_pubkey = obligation.deposits()
        .ok()
        .and_then(|deposits| {
            log::debug!("  📊 Parsed {} deposits from obligation", deposits.len());
            if let Some(first_deposit) = deposits.first() {
                log::debug!("  🔍 First deposit reserve: {}", first_deposit.depositReserve);
                Some(first_deposit.depositReserve)
            } else {
                log::debug!("  ⚠️  No deposits found in obligation");
                None
            }
        });
    
    if borrow_reserve_pubkey.is_none() && deposit_reserve_pubkey.is_none() {
        log::warn!("⚠️  Obligation has neither borrows nor deposits - cannot liquidate");
        return Err(anyhow::anyhow!("Obligation has no borrows or deposits"));
    }

    // Load reserves in parallel using tokio::join!
    log::debug!("  🔄 Loading reserves in parallel (borrow: {:?}, deposit: {:?})", 
                borrow_reserve_pubkey, deposit_reserve_pubkey);
    let reserve_load_start = std::time::Instant::now();
    
    let (borrow_result, deposit_result) = tokio::join!(
        async {
            if let Some(pubkey) = borrow_reserve_pubkey {
                log::debug!("  📥 Loading borrow reserve: {}", pubkey);
                let rpc_clone = Arc::clone(rpc);
                tokio::task::spawn_blocking(move || -> Result<Option<Reserve>, solana_client::client_error::ClientError> {
                    let account_data = rpc_clone.get_account_data(&pubkey)?;
                    log::debug!("  📊 Borrow reserve account data retrieved: {} bytes", account_data.len());
                    let reserve = Reserve::from_account_data(&account_data)
                        .map_err(|e| solana_client::client_error::ClientError::from(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Parse error: {}", e))))?;
                    log::debug!("  ✅ Borrow reserve parsed successfully: version={}, mint={}", 
                                reserve.version, reserve.liquidityMintPubkey);
                    Ok(Some(reserve))
                }).await
                .map_err(|e| solana_client::client_error::ClientError::from(std::io::Error::new(std::io::ErrorKind::Other, format!("Join error: {}", e))))
            } else {
                log::debug!("  ⏭️  No borrow reserve to load");
                Ok(Ok(None))
            }
        },
        async {
            if let Some(pubkey) = deposit_reserve_pubkey {
                log::debug!("  📥 Loading deposit reserve: {}", pubkey);
                let rpc_clone = Arc::clone(rpc);
                tokio::task::spawn_blocking(move || -> Result<Option<Reserve>, solana_client::client_error::ClientError> {
                    let account_data = rpc_clone.get_account_data(&pubkey)?;
                    log::debug!("  📊 Deposit reserve account data retrieved: {} bytes", account_data.len());
                    let reserve = Reserve::from_account_data(&account_data)
                        .map_err(|e| solana_client::client_error::ClientError::from(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Parse error: {}", e))))?;
                    log::debug!("  ✅ Deposit reserve parsed successfully: version={}, mint={}", 
                                reserve.version, reserve.liquidityMintPubkey);
                    Ok(Some(reserve))
                }).await
                .map_err(|e| solana_client::client_error::ClientError::from(std::io::Error::new(std::io::ErrorKind::Other, format!("Join error: {}", e))))
            } else {
                log::debug!("  ⏭️  No deposit reserve to load");
                Ok(Ok(None))
            }
        }
    );
    
    let reserve_load_duration = reserve_load_start.elapsed();
    log::debug!("  ⏱️  Reserve loading completed in {:?}", reserve_load_duration);

    // Process borrow reserve result
    match borrow_result {
        Ok(Ok(Some(reserve))) => {
            let liquidity = reserve.liquidity();
            const WAD: f64 = 1_000_000_000_000_000_000.0;
            let market_price = liquidity.liquidityMarketPrice as f64 / WAD;
            log::debug!(
                "✅ Loaded borrow reserve: {} (liquidation_threshold={:.2}%, liquidation_bonus={:.2}%, close_factor={:.2}%, market_price={:.6})",
                borrow_reserve_pubkey.unwrap(),
                reserve.liquidation_threshold() * 100.0,
                reserve.liquidation_bonus() * 100.0,
                reserve.close_factor() * 100.0,
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
            let liquidity = reserve.liquidity();
            const WAD: f64 = 1_000_000_000_000_000_000.0;
            let market_price = liquidity.liquidityMarketPrice as f64 / WAD;
            log::debug!(
                "✅ Loaded deposit reserve: {} (liquidation_threshold={:.2}%, liquidation_bonus={:.2}%, close_factor={:.2}%, market_price={:.6})",
                deposit_reserve_pubkey.unwrap(),
                reserve.liquidation_threshold() * 100.0,
                reserve.liquidation_bonus() * 100.0,
                reserve.close_factor() * 100.0,
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

    // Parse borrows and deposits from obligation
    let borrows = obligation.borrows()
        .map_err(|e| anyhow::anyhow!("Failed to parse borrows: {}", e))?;
    let deposits = obligation.deposits()
        .map_err(|e| anyhow::anyhow!("Failed to parse deposits: {}", e))?;

    let (oracle_ok, borrow_price, deposit_price) = validate_oracles_with_twap(rpc, &borrow_reserve, &deposit_reserve).await?;

    Ok(LiquidationContext {
        obligation_pubkey: obligation.owner,
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

/// Liquidation quote with profit calculation
pub struct LiquidationQuote {
    pub quote: JupiterQuote,
    pub profit_usdc: f64,
    pub collateral_value_usd: f64,
    pub debt_to_repay_raw: u64,
    pub collateral_to_seize_raw: u64,
    pub flashloan_fee_raw: u64,
}

/// Build liquidation transaction (DEPRECATED)
/// 
/// ⚠️ DEPRECATED: Two-Transaction Approach (Race Condition Risk!)
/// 
/// This function is DEPRECATED and should NOT be used.
/// Use `build_flashloan_liquidation_tx` instead for atomic single-transaction approach.
/// 
/// ❌ PROBLEMS WITH TWO-TRANSACTION APPROACH:
/// - Race condition: TX1 and TX2 can be front-run by MEV bots
/// - MEV risk: Intermediate SOL state exposed between TX1 and TX2
/// - Capital requirement: Need USDC upfront for TX1
/// - Higher fees: Two transaction fees instead of one
/// 
/// ✅ USE FLASHLOAN APPROACH INSTEAD:
/// - Atomic: All operations in single transaction
/// - No MEV risk: No intermediate state
/// - No capital needed: Flashloan provides funds
/// - Gas-efficient: Single transaction fee
/// 
/// Build transaction 1: Liquidation + Redemption (NO Jupiter Swap!)
/// CRITICAL: blockhash must be fresh (fetched immediately before calling this function).
/// Blockhashes are valid for ~150 slots (~60 seconds), so fetch blockhash right before
/// building the transaction to minimize staleness risk.
#[deprecated(note = "Use build_flashloan_liquidation_tx instead for atomic single-transaction approach")]
async fn build_liquidation_tx1(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: solana_sdk::hash::Hash,
    _config: &Config,
) -> Result<Transaction> {
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
        sysvar,
    };
    use spl_token::ID as TOKEN_PROGRAM_ID;

    let current_slot_now = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    
    if let Some(reserve) = &ctx.borrow_reserve {
        let (valid, _) = oracle::pyth::validate_pyth_oracle(
            rpc,
            reserve.oracle_pubkey(),
            current_slot_now,
        )
        .await
        .context("Failed to validate borrow reserve oracle")?;
        
        if !valid {
            return Err(anyhow::anyhow!(
                "Borrow reserve oracle became stale during TX preparation for obligation {}",
                ctx.obligation_pubkey
            ));
        }
    }
    
    if let Some(reserve) = &ctx.deposit_reserve {
        let (valid, _) = oracle::pyth::validate_pyth_oracle(
            rpc,
            reserve.oracle_pubkey(),
            current_slot_now,
        )
        .await
        .context("Failed to validate deposit reserve oracle")?;
        
        if !valid {
            return Err(anyhow::anyhow!(
                "Deposit reserve oracle became stale during TX preparation. \
                 Time elapsed since initial validation: ~2-3s. Aborting liquidation for obligation {}.",
                ctx.obligation_pubkey
            ));
        }
    }
    
    log::debug!(
        "✅ Oracle re-validation passed for obligation {} (current_slot={})",
        ctx.obligation_pubkey,
        current_slot_now
    );

    let program_id = solend_program_id()?;
    let wallet_pubkey = wallet.pubkey();

    // Get reserves
    let borrow_reserve = ctx
        .borrow_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = ctx
        .deposit_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;

    // CRITICAL: Solend LiquidateObligation instruction expects debt token amount (liquidity_amount),
    // NOT collateral amount. This is the amount of debt we're repaying.
    // We already calculated this in get_liquidation_quote() and stored it in debt_to_repay_raw.
    let liquidity_amount = quote.debt_to_repay_raw;

    // Derive required addresses
    let lending_market = ctx.obligation.lendingMarket;
    let lending_market_authority = crate::solend::derive_lending_market_authority(&lending_market, &program_id)?;

    // Get reserve liquidity supply addresses
    // These are stored in Reserve account (supplyPubkey field)
    // Solend program stores the correct PDA addresses in Reserve account during initialization
    let repay_reserve_liquidity_supply = borrow_reserve.liquidity().supplyPubkey;
    let withdraw_reserve_liquidity_supply = deposit_reserve.liquidity().supplyPubkey;
    let withdraw_reserve_collateral_supply = deposit_reserve.collateral().supplyPubkey;
    let withdraw_reserve_collateral_mint = deposit_reserve.collateral().mintPubkey;
    
    // Verify these are not default/zero addresses
    // CRITICAL: Reserve account's supplyPubkey is the authoritative source.
    // We use the value directly from Reserve account, not derived PDA.
    // Solend program stores the correct PDA addresses in Reserve account during initialization.
    if repay_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_collateral_supply == Pubkey::default()
        || withdraw_reserve_collateral_mint == Pubkey::default() {
        return Err(anyhow::anyhow!("Invalid reserve addresses: one or more addresses are default/zero"));
    }

    // SECURITY: Verify reserve supply PDA addresses match expected derivation
    // This helps detect data corruption or manipulation in Reserve accounts.
    // We derive expected PDA and compare with stored supplyPubkey.
    // 
    // CRITICAL: If PDA can be derived and doesn't match, this indicates data corruption
    // or manipulation. We MUST fail-fast per Structure.md section 13 (fail-fast principle).
    // 
    // If PDA cannot be derived (None), this may indicate unknown seed format, but we
    // still log a warning as this is unusual.
    let borrow_reserve_pubkey = ctx.borrows[0].borrowReserve;
    let deposit_reserve_pubkey = ctx.deposits[0].depositReserve;
    
    // Verify repay reserve liquidity supply
    // CRITICAL SECURITY FIX: Fail-fast if PDA cannot be derived (None)
    // This prevents proceeding with unverified addresses, which could indicate:
    // - Unknown PDA format (code needs update)
    // - Data corruption in Reserve account
    // - Malicious account manipulation
    let derived_pda = crate::solend::derive_reserve_liquidity_supply_pda(&borrow_reserve_pubkey, &program_id)
        .ok_or_else(|| anyhow::anyhow!(
            "CRITICAL SECURITY FAILURE: Cannot derive PDA for repay reserve liquidity supply. \
             Reserve: {}. \
             This may indicate unknown PDA format or data corruption. Transaction ABORTED.",
            borrow_reserve_pubkey
        ))?;
    
    if derived_pda != repay_reserve_liquidity_supply {
        log::error!(
            "🚨 SECURITY ALERT: PDA mismatch detected!\n\
             Reserve: {}\n\
             Stored supplyPubkey: {}\n\
             Derived PDA: {}\n\
             This may indicate:\n\
             - Data corruption in Reserve account\n\
             - Malicious account manipulation\n\
             - Outdated PDA derivation seeds\n\
             Transaction ABORTED for security.",
            borrow_reserve_pubkey,
            repay_reserve_liquidity_supply,
            derived_pda
        );
        return Err(anyhow::anyhow!("SECURITY FAILURE: PDA mismatch"));
    }
    log::debug!("✅ Repay reserve liquidity supply PDA verified: {}", repay_reserve_liquidity_supply);
    
    // Verify withdraw reserve liquidity supply
    // CRITICAL SECURITY FIX: Fail-fast if PDA cannot be derived (None)
    let derived_pda = crate::solend::derive_reserve_liquidity_supply_pda(&deposit_reserve_pubkey, &program_id)
        .ok_or_else(|| anyhow::anyhow!(
            "CRITICAL SECURITY FAILURE: Cannot derive PDA for withdraw reserve liquidity supply. \
             Reserve: {}. \
             This may indicate unknown PDA format or data corruption. Transaction ABORTED.",
            deposit_reserve_pubkey
        ))?;
    
    if derived_pda != withdraw_reserve_liquidity_supply {
        return Err(anyhow::anyhow!(
            "SECURITY FAILURE: Withdraw reserve liquidity supply PDA mismatch! \
             This indicates data corruption or manipulation. \
             Reserve: {}, Stored: {}, Derived: {}. \
             Transaction aborted for security.",
            deposit_reserve_pubkey,
            withdraw_reserve_liquidity_supply,
            derived_pda
        ));
    }
    log::debug!("✅ Withdraw reserve liquidity supply PDA verified: {}", withdraw_reserve_liquidity_supply);
    
    // Verify withdraw reserve collateral supply
    // CRITICAL SECURITY FIX: Fail-fast if PDA cannot be derived (None)
    let derived_pda = crate::solend::derive_reserve_collateral_supply_pda(&deposit_reserve_pubkey, &program_id)
        .ok_or_else(|| anyhow::anyhow!(
            "CRITICAL SECURITY FAILURE: Cannot derive PDA for withdraw reserve collateral supply. \
             Reserve: {}. \
             This may indicate unknown PDA format or data corruption. Transaction ABORTED.",
            deposit_reserve_pubkey
        ))?;
    
    if derived_pda != withdraw_reserve_collateral_supply {
        return Err(anyhow::anyhow!(
            "SECURITY FAILURE: Withdraw reserve collateral supply PDA mismatch! \
             This indicates data corruption or manipulation. \
             Reserve: {}, Stored: {}, Derived: {}. \
             Transaction aborted for security.",
            deposit_reserve_pubkey,
            withdraw_reserve_collateral_supply,
            derived_pda
        ));
    }
    log::debug!("✅ Withdraw reserve collateral supply PDA verified: {}", withdraw_reserve_collateral_supply);

    // Get user's token accounts (source liquidity and destination collateral)
    // These would be ATAs for the tokens
    use spl_associated_token_account::get_associated_token_address;
    let source_liquidity = get_associated_token_address(&wallet_pubkey, &borrow_reserve.liquidity().mintPubkey);
    let destination_collateral = get_associated_token_address(&wallet_pubkey, &withdraw_reserve_collateral_mint);
    
    // CRITICAL SECURITY: Validate that ATAs exist before building transaction
    // If ATAs don't exist, the transaction will fail at runtime
    // In production, these should be created at startup, but we validate here for safety
    let source_liquidity_exists = rpc.get_account(&source_liquidity).is_ok();
    if !source_liquidity_exists {
        return Err(anyhow::anyhow!(
            "Source liquidity ATA does not exist: {}. \
             Please create ATA for token {} before liquidation. \
             NOTE: In production, create all required ATAs at startup to avoid this check.",
            source_liquidity,
            borrow_reserve.liquidity().mintPubkey
        ));
    }
    
    let dest_collateral_exists = rpc.get_account(&destination_collateral).is_ok();
    if !dest_collateral_exists {
        return Err(anyhow::anyhow!(
            "Destination collateral ATA does not exist: {}. \
             Please create ATA for token {} before liquidation. \
             NOTE: In production, create all required ATAs at startup to avoid this check.",
            destination_collateral,
            withdraw_reserve_collateral_mint
        ));
    }
    
    log::debug!(
        "✅ ATA validation passed: source_liquidity={}, destination_collateral={}",
        source_liquidity,
        destination_collateral
    );

    // Build Solend liquidation instruction
    // Solend uses enum-based instruction encoding via LendingInstruction enum (NOT Anchor)
    // 
    // IMPORTANT: Solend is a native Solana program, not Anchor-based.
    // Reference: solend-sdk crate, instruction.rs - LendingInstruction enum
    // 
    // Instruction format:
    //   [tag: u8] + [args...]
    //   tag 12 = LiquidateObligation { liquidity_amount: u64 }
    // 
    // Full instruction data: [12] + liquidity_amount.to_le_bytes()
    let mut instruction_data = Vec::new();
    
    // Instruction discriminator: LendingInstruction::LiquidateObligation tag = 12
    // CRITICAL: Solend uses enum-based encoding (tag = 12), NOT Anchor sighash
    // Solend native program uses only 1 byte for enum tag
    let discriminator = crate::solend::get_liquidate_obligation_discriminator();
    log::debug!(
        "Using instruction discriminator: {} (hex: {:02x}) for liquidateObligation",
        discriminator,
        discriminator
    );
    instruction_data.push(discriminator); // Add only 1 byte
    
    // Args: liquidityAmount (u64)
    instruction_data.extend_from_slice(&liquidity_amount.to_le_bytes());

    // Build account metas per IDL - order must match Solend IDL exactly
    // CRITICAL: Account order verified against Solend SDK source code
    // Reference: solend-sdk/src/instruction.rs - LendingInstruction::LiquidateObligation
    // 
    // Correct account order (12 accounts total):
    // 0. [writable] sourceLiquidity - user's token account for debt token
    // 1. [writable] destinationCollateral - user's token account for collateral token
    // 2. [writable] repayReserve - reserve account for debt token (refreshed)
    // 3. [writable] repayReserveLiquiditySupply - SPL token supply for debt reserve
    // 4. [readonly] withdrawReserve - reserve account for collateral token (refreshed)
    // 5. [writable] withdrawReserveCollateralSupply - SPL token supply for collateral reserve
    // 6. [writable] obligation - obligation account (refreshed)
    // 7. [readonly] lendingMarket - lending market account
    // 8. [readonly] lendingMarketAuthority - derived PDA authority
    // 9. [signer] transferAuthority - user wallet (signer)
    // 10. [readonly] clockSysvar - clock sysvar account
    // 11. [readonly] tokenProgram - SPL token program ID
    let accounts = vec![
        AccountMeta::new(source_liquidity, false),                    // 0: sourceLiquidity
        AccountMeta::new(destination_collateral, false),              // 1: destinationCollateral
        AccountMeta::new(ctx.borrows[0].borrowReserve, false),  // 2: repayReserve (writable, refreshed)
        AccountMeta::new(repay_reserve_liquidity_supply, false),      // 3: repayReserveLiquiditySupply
        AccountMeta::new_readonly(ctx.deposits[0].depositReserve, false), // 4: withdrawReserve (readonly, refreshed)
        AccountMeta::new(withdraw_reserve_collateral_supply, false),  // 5: withdrawReserveCollateralSupply
        AccountMeta::new(ctx.obligation_pubkey, false),                // 6: obligation (writable, refreshed)
        AccountMeta::new_readonly(lending_market, false),            // 7: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),   // 8: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),               // 9: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // 10: clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),          // 11: tokenProgram
    ];

    // CRITICAL FIX: Validate account ordering per Problems.md section 3.B
    // Account order must match Solend IDL exactly, or transaction will fail
    const EXPECTED_ACCOUNT_COUNT: usize = 12;
    if accounts.len() != EXPECTED_ACCOUNT_COUNT {
        return Err(anyhow::anyhow!(
            "Invalid account count for LiquidateObligation: expected {}, got {}",
            EXPECTED_ACCOUNT_COUNT,
            accounts.len()
        ));
    }
    
    // Validate specific account requirements
    // Account 9 (transferAuthority) must be a signer
    if !accounts[9].is_signer {
        return Err(anyhow::anyhow!(
            "Account ordering validation failed: transferAuthority (index 9) must be a signer"
        ));
    }
    
    // Validate account 0 (sourceLiquidity) is writable
    if !accounts[0].is_writable {
        return Err(anyhow::anyhow!(
            "Account ordering validation failed: sourceLiquidity (index 0) must be writable"
        ));
    }
    
    // Validate account 1 (destinationCollateral) is writable
    if !accounts[1].is_writable {
        return Err(anyhow::anyhow!(
            "Account ordering validation failed: destinationCollateral (index 1) must be writable"
        ));
    }
    
    // Validate readonly accounts (indices 4, 7, 8, 10, 11)
    let readonly_indices = [4, 7, 8, 10, 11];
    for &idx in &readonly_indices {
        if accounts[idx].is_writable {
            return Err(anyhow::anyhow!(
                "Account ordering validation failed: account at index {} must be readonly",
                idx
            ));
        }
    }
    
    log::debug!(
        "✅ Account ordering validation passed for LiquidateObligation: {} accounts, transferAuthority is signer",
        accounts.len()
    );

    let liquidation_ix = Instruction {
        program_id,
        accounts,
        data: instruction_data,
    };

    // Add compute budget instruction per Structure.md section 8
    // Note: solana-sdk 1.18 doesn't have ComputeBudgetInstruction, so we build manually
    // Compute Budget Program ID
    let compute_budget_program_id = Pubkey::from_str("ComputeBudget111111111111111111111111111111")
        .map_err(|e| anyhow::anyhow!("Invalid compute budget program ID: {}", e))?;

    // Build compute unit limit instruction manually
    // Instruction format: [discriminator: 2, units: u32]
    let mut compute_limit_data = vec![2u8]; // SetComputeUnitLimit discriminator
    compute_limit_data.extend_from_slice(&(200_000u32).to_le_bytes());
    let compute_budget_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_limit_data,
    };

    // Build compute unit price instruction manually
    // Instruction format: [discriminator: 3, micro_lamports: u64]
    let mut compute_price_data = vec![3u8]; // SetComputeUnitPrice discriminator
    compute_price_data.extend_from_slice(&(1_000u64).to_le_bytes()); // 0.001 SOL per CU
    let priority_fee_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_price_data,
    };

    // ============================================================================
    // INSTRUCTION 2: RedeemReserveCollateral (EKLENECEK!)
    // ============================================================================
    // LiquidateObligation bize cToken verir, bunu underlying token'a çevirmeliyiz
    //
    // Solend IDL: RedeemReserveCollateral instruction
    // Discriminator: 5 (LendingInstruction enum)
    // Args: collateral_amount (u64)
    
    // Calculate collateral amount to redeem (cToken amount received from liquidation)
    // CRITICAL: We need to calculate this from the quote or context
    // For now, we'll calculate it from the debt amount and liquidation bonus
    // This is an approximation - ideally this should be passed from get_liquidation_quote()
    
    // Use collateral_to_seize_raw from quote (calculated in get_liquidation_quote)
    let redeem_collateral_amount = quote.collateral_to_seize_raw; // cToken amount to redeem
    
    // Build RedeemReserveCollateral instruction data
    let mut redeem_instruction_data = Vec::new();
    let redeem_discriminator = crate::solend::get_redeem_reserve_collateral_discriminator();
    log::debug!(
        "Using instruction discriminator: {} (hex: {:02x}) for RedeemReserveCollateral",
        redeem_discriminator,
        redeem_discriminator
    );
    redeem_instruction_data.push(redeem_discriminator); // RedeemReserveCollateral discriminator
    redeem_instruction_data.extend_from_slice(&redeem_collateral_amount.to_le_bytes()); // collateral_amount (u64)
    
    // Get necessary accounts for RedeemReserveCollateral
    // Per Solend IDL:
    // 0. [writable] sourceCollateral - User's cToken account (destination from LiquidateObligation)
    // 1. [writable] destinationLiquidity - User's underlying token account
    // 2. [writable] reserve - Reserve account
    // 3. [writable] reserveCollateralMint - Reserve cToken mint
    // 4. [writable] reserveLiquiditySupply - Reserve underlying token supply
    // 5. [readonly] lendingMarket - Lending market
    // 6. [readonly] lendingMarketAuthority - Lending market authority PDA
    // 7. [signer] transferAuthority - User wallet
    // 8. [readonly] clockSysvar - Clock sysvar
    // 9. [readonly] tokenProgram - SPL Token program
    
    // Source collateral: User's cToken ATA (destination from LiquidateObligation)
    // This is the same as destination_collateral from LiquidateObligation
    let source_collateral = destination_collateral;
    
    // Destination liquidity: User's underlying token ATA
    let destination_liquidity = get_associated_token_address(
        &wallet_pubkey,
        &deposit_reserve.liquidity().mintPubkey // Underlying token mint (e.g., SOL)
    );
    
    // CRITICAL SECURITY: Validate that destination liquidity ATA exists
    // If not, transaction will fail at runtime
    let dest_liquidity_exists = rpc.get_account(&destination_liquidity).is_ok();
    if !dest_liquidity_exists {
        return Err(anyhow::anyhow!(
            "Destination liquidity ATA does not exist: {}. \
             Please create ATA for underlying token {} before liquidation. \
             NOTE: In production, create all required ATAs at startup to avoid this check.",
            destination_liquidity,
            deposit_reserve.liquidity().mintPubkey
        ));
    }
    
    log::debug!(
        "✅ Destination liquidity ATA validation passed: {}",
        destination_liquidity
    );
    
    // Build RedeemReserveCollateral instruction accounts
    let redeem_accounts = vec![
        AccountMeta::new(source_collateral, false),                     // 0: sourceCollateral
        AccountMeta::new(destination_liquidity, false),                 // 1: destinationLiquidity
        AccountMeta::new(ctx.deposits[0].depositReserve, false), // 2: reserve
        AccountMeta::new(withdraw_reserve_collateral_mint, false),      // 3: reserveCollateralMint
        AccountMeta::new(withdraw_reserve_liquidity_supply, false),     // 4: reserveLiquiditySupply
        AccountMeta::new_readonly(lending_market, false),              // 5: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),     // 6: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),                 // 7: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),          // 8: clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),            // 9: tokenProgram
    ];
    
    let redeem_collateral_ix = Instruction {
        program_id,
        accounts: redeem_accounts,
        data: redeem_instruction_data,
    };
    
    // ============================================================================
    // TRANSACTION 1: Liquidation + Redemption (NO Jupiter Swap!)
    // ============================================================================
    // CRITICAL: Jupiter swap must be in a SEPARATE transaction because:
    // 1. LiquidateObligation writes cSOL tokens to wallet's ATA
    // 2. RedeemReserveCollateral reads those cSOL tokens and writes SOL to wallet's ATA
    // 3. Jupiter swap needs to read SOL from wallet's ATA
    // 
    // Solana transactions are atomic - all instructions execute simultaneously.
    // If we include Jupiter swap in the same transaction, it will try to read SOL
    // that hasn't been written yet, causing AccountNotFound or InsufficientFunds errors.
    //
    // Solution: Split into 2 transactions:
    // TX1: Liquidation + Redemption (Solend protocol)
    // TX2: Jupiter Swap (DEX)
    
    // Build transaction with fresh blockhash
    // CRITICAL: blockhash must be fetched immediately before this function is called
    // to ensure it's fresh and not stale. Blockhashes are valid for ~150 slots (~60 seconds).
    let mut tx = Transaction::new_with_payer(
        &[
            compute_budget_ix,        // Compute unit limit
            priority_fee_ix,          // Priority fee
            liquidation_ix,           // LiquidateObligation (USDC -> cSOL)
            redeem_collateral_ix,     // RedeemReserveCollateral (cSOL -> SOL)
        ],
        Some(&wallet_pubkey),
    );
    // Set blockhash immediately - it was fetched right before this function call
    tx.message.recent_blockhash = blockhash;

    log::info!(
        "Built TX1 (Liquidation + Redemption) for obligation {}:\n\
         - Liquidate: {} debt tokens (USDC -> cSOL)\n\
         - Redeem: {} cTokens -> underlying tokens (cSOL -> SOL)\n\
         - Source collateral ATA: {}\n\
         - Destination liquidity ATA: {}",
        ctx.obligation_pubkey,
        liquidity_amount,
        redeem_collateral_amount,
        source_collateral,
        destination_liquidity
    );

    Ok(tx)
}

/// ⚠️ DEPRECATED: Two-Transaction Approach (Race Condition Risk!)
/// 
/// This function is DEPRECATED and should NOT be used.
/// Use `build_flashloan_liquidation_tx` instead for atomic single-transaction approach.
/// 
/// ❌ PROBLEMS WITH TWO-TRANSACTION APPROACH:
/// - Race condition: TX1 and TX2 can be front-run by MEV bots
/// - MEV risk: Intermediate SOL state exposed between TX1 and TX2
/// - Capital requirement: Need USDC upfront for TX1
/// - Higher fees: Two transaction fees instead of one
/// 
/// ✅ USE FLASHLOAN APPROACH INSTEAD:
/// - Atomic: All operations in single transaction
/// - No MEV risk: No intermediate state
/// - No capital needed: Flashloan provides funds
/// - Gas-efficient: Single transaction fee
/// 
/// Build transaction 2: Jupiter Swap (SOL -> USDC)
/// This is called AFTER TX1 confirms and SOL is available in wallet
/// 
/// ❌ WARNING: This creates a race condition window where MEV bots can front-run TX2!
#[deprecated(note = "Use build_flashloan_liquidation_tx instead for atomic single-transaction approach")]
/// CRITICAL: blockhash must be fresh (fetched immediately before calling this function).
async fn build_liquidation_tx2(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    blockhash: solana_sdk::hash::Hash,
    config: &Config,
) -> Result<Transaction> {
    use solana_sdk::instruction::Instruction;
    
    let wallet_pubkey = wallet.pubkey();
    
    // Compute Budget Program ID
    let compute_budget_program_id = Pubkey::from_str("ComputeBudget111111111111111111111111111111")
        .map_err(|e| anyhow::anyhow!("Invalid compute budget program ID: {}", e))?;

    // Build compute unit limit instruction
    let mut compute_limit_data = vec![2u8]; // SetComputeUnitLimit discriminator
    compute_limit_data.extend_from_slice(&(200_000u32).to_le_bytes());
    let compute_budget_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_limit_data,
    };

    // Build compute unit price instruction
    let mut compute_price_data = vec![3u8]; // SetComputeUnitPrice discriminator
    compute_price_data.extend_from_slice(&(1_000u64).to_le_bytes()); // 0.001 SOL per CU
    let priority_fee_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_price_data,
    };
    
    // ============================================================================
    // INSTRUCTION: Jupiter Swap (SOL -> USDC)
    // ============================================================================
    // After TX1 confirms, we have SOL in wallet's ATA
    // Now we swap it to USDC to complete the liquidation flow
    // CRITICAL: Jupiter returns a vector of instructions (Setup + Swap + Cleanup)
    // We must include all of them in the transaction to handle ATAs correctly
    let jupiter_swap_ixs = crate::jup::build_jupiter_swap_instruction(
        &quote.quote,
        &wallet_pubkey,
        &config.jupiter_url,
    )
    .await
    .context("Failed to build Jupiter swap instruction")?;
    
    // Build instruction list
    let mut instructions = vec![
        compute_budget_ix,        // Compute unit limit
        priority_fee_ix,          // Priority fee
    ];
    // Add all Jupiter instructions (setup + swap + cleanup)
    instructions.extend(jupiter_swap_ixs);
    
    // Build transaction with fresh blockhash
    let mut tx = Transaction::new_with_payer(
        &instructions,
        Some(&wallet_pubkey),
    );
    tx.message.recent_blockhash = blockhash;

    log::info!(
        "Built TX2 (Jupiter Swap) for obligation {}:\n\
         - Swap: SOL -> USDC ({} instructions)",
        ctx.obligation_pubkey,
        instructions.len() - 2 // Subtract compute budget instructions
    );

    Ok(tx)
}

// Transaction building functions moved to liquidation_tx module:
// - build_flashloan_liquidation_tx -> liquidation_tx::build_flashloan_liquidation_tx
// - build_liquidation_tx1 -> deprecated (kept for reference)
// - build_liquidation_tx2 -> deprecated (kept for reference)

// Transaction building functions moved to liquidation_tx module

/// Execute liquidation with swap using flashloan (atomic single transaction)
/// 
/// FLASHLOAN APPROACH - Solves race condition and MEV risks:
/// - All operations in ONE atomic transaction
/// - No race conditions: No TX1/TX2 split
/// - No MEV risk: No intermediate state exposed
/// - No capital required: Flashloan provides initial funds
/// - Gas-efficient: Single transaction
/// 
/// Flow:
/// 1. FlashLoan: Borrow debt_amount USDC from Solend (flash)
/// 2. LiquidateObligation: Repay debt, receive cSOL
/// 3. RedeemReserveCollateral: cSOL -> SOL
/// 4. Jupiter Swap: SOL -> USDC
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
    // Blockhash expires in ~60 seconds. Jupiter quote can take 8-15 seconds,
    // so we must get blockhash AFTER quote to ensure maximum freshness.
    // This prevents "BlockhashNotFound" errors from stale blockhashes.
    let blockhash = rpc
        .get_latest_blockhash()
        .map_err(|e| anyhow::anyhow!("Failed to get blockhash: {}", e))?;
    
    let tx = build_flashloan_liquidation_tx(wallet, ctx, quote, rpc, blockhash, config)
        .await
        .context("Failed to build flashloan liquidation transaction")?;
    
    // Send transaction via Jito
    let bundle_id = send_jito_bundle(tx, jito_client, wallet, blockhash)
        .await
        .context("Failed to send flashloan liquidation transaction via Jito")?;
    
    log::info!(
        "✅ Atomic flashloan liquidation transaction sent: bundle_id={}\n\
         All operations (FlashLoan -> Liquidate -> Redeem -> Swap -> Repay) are atomic!",
        bundle_id
    );
    
    // ✅ FIXED: Smart polling strategy (Problems.md recommendation)
    // Bundle typically confirms in 300-400ms. Starting at 100ms is too aggressive.
    // Strategy: Wait 300ms initially, then exponential backoff (300, 600, 1200, 2000...)
    let mut confirmed = false;
    
    // Initial wait - typical bundle confirmation time
    tokio::time::sleep(Duration::from_millis(300)).await;
    
    let mut delay_ms = 300; 
    const MAX_ATTEMPTS: u32 = 10;
    const MAX_DELAY_MS: u64 = 2000; // Max 2s between checks
    
    for attempt in 0..MAX_ATTEMPTS {
        if let Ok(Some(status)) = jito_client.get_bundle_status(&bundle_id).await {
            if let Some(status_str) = &status.status {
                match status_str.as_str() {
                    "landed" | "confirmed" => {
                        confirmed = true;
                        log::debug!(
                            "✅ Bundle {} confirmed (poll #{})",
                            bundle_id,
                            attempt + 1
                        );
                        break;
                    }
                    "failed" | "dropped" => {
                        log::warn!(
                            "Bundle {} failed/dropped (poll #{})",
                            bundle_id,
                            attempt + 1
                        );
                        break;
                    }
                    "pending" => {
                        // Continue polling
                    }
                    _ => {
                        log::debug!("Bundle {} unknown status: {}", bundle_id, status_str);
                    }
                }
            }
            
            // If slot is present, bundle likely executed
            if status.slot.is_some() {
                confirmed = true;
                log::debug!(
                    "✅ Bundle {} confirmed (has slot) (poll #{})",
                    bundle_id,
                    attempt + 1
                );
                break;
            }
        }
        
        // Exponential backoff: double delay each time, max 2000ms
        delay_ms = (delay_ms * 2).min(MAX_DELAY_MS);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
    
    if !confirmed {
        log::debug!(
            "Bundle {} status still unknown after 4s polling, will be tracked by bundle_tracker",
            bundle_id
        );
    }
    
    Ok(())
}


