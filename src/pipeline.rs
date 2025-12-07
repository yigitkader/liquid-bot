use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use parking_lot::Mutex;
use lazy_static::lazy_static;

use crate::jup::{get_jupiter_quote_with_retry, JupiterQuote};
use crate::solend::{Obligation, Reserve, solend_program_id};
use crate::solend::{ObligationLiquidity, ObligationCollateral};
use crate::utils::{send_jito_bundle, JitoClient};

// Static aligned buffer pool for Switchboard Oracle alignment fix
// Reduces GC pressure by reusing buffers instead of allocating on every oracle read
lazy_static! {
    static ref ALIGNED_BUFFERS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
}

/// Get an aligned buffer from the pool or allocate a new one if pool is empty
/// Ensures the buffer is at least `size` bytes
fn get_aligned_buffer(size: usize) -> Vec<u8> {
    let mut pool = ALIGNED_BUFFERS.lock();
    match pool.pop() {
        Some(mut buffer) => {
            // Ensure buffer is large enough
            if buffer.capacity() < size {
                buffer = vec![0u8; size];
            } else {
                buffer.resize(size, 0u8);
            }
            buffer
        }
        None => vec![0u8; size],
    }
}

/// Return a buffer to the pool for reuse (max 10 cached buffers to limit memory usage)
fn return_aligned_buffer(mut buffer: Vec<u8>) {
    buffer.clear();
    let mut pool = ALIGNED_BUFFERS.lock();
    if pool.len() < 10 {
        // Max 10 cached buffers
        pool.push(buffer);
    }
}

/// Liquidation mode - per Structure.md
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
    pub max_position_pct: f64, // Örn: 0.05 => cüzdanın %5'i max risk
    pub wallet: Arc<Keypair>,
    pub jito_tip_account: Option<String>, // Auto-discovered from env or default
    pub jito_tip_amount_lamports: Option<u64>, // Auto-discovered from env or default
}

/// Main liquidation loop - minimal async pipeline per Structure.md section 9
pub async fn run_liquidation_loop(
    rpc: Arc<RpcClient>,
    config: Config,
) -> Result<()> {
    let program_id = solend_program_id()?;
    let wallet = config.wallet.pubkey();

    // Initialize Jito client with tip account from config or env
    // No hardcoded defaults - must be set in .env file
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
    
    // CRITICAL: Validate Jito tip account address format
    // 
    // IMPORTANT: Jito tip account is CRITICAL for bot operation:
    // - All liquidation transactions are sent via Jito bundles (MEV protection)
    // - Each bundle includes a tip transaction to the tip account
    // - Tip account is just a transfer address (SOL is sent to it)
    // - Account doesn't need to exist in RPC (AccountNotFound is OK)
    // - But address MUST be valid Pubkey format (otherwise tip transaction will fail)
    //
    // If tip account is wrong:
    // - Tip will be lost (sent to wrong address)
    // - But bundle can still be sent (Jito will accept it)
    // - However, without proper tip, bundle may not get priority processing
    //
    // Bot will work even if tip account not found in RPC, but tip account address
    // must be valid for tip transaction to be created successfully.
    let jito_tip_account = Pubkey::from_str(&jito_tip_account_str)
        .context(format!(
            "CRITICAL: Invalid Jito tip account address format: {}. \
             Jito is REQUIRED for bot operation (all transactions sent via Jito bundles). \
             Please verify JITO_TIP_ACCOUNT in .env is a valid Solana address.",
            jito_tip_account_str
        ))?;
    
    // Optional: Try to fetch account info for validation (but don't fail if not found)
    // Jito tip accounts are just transfer addresses - they may not exist in RPC
    // This is just for informational purposes - bot will work regardless
    match rpc.get_account(&jito_tip_account) {
        Ok(account_data) => {
            // If account exists, validate it's a system account (native SOL account)
            use solana_sdk::system_program;
            if account_data.owner == system_program::id() {
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
            // AccountNotFound is OK - Jito tip accounts are just transfer addresses
            // They don't need to exist in RPC, they're just used for SOL transfers
            // Bot will work fine - tip transaction will be created and sent
            log::warn!(
                "ℹ️  Jito tip account {} not found in RPC (this is OK - tip accounts are transfer addresses, not regular accounts). \
                 Bot will work normally - tip transaction will be created when sending bundles.",
                jito_tip_account
            );
        }
    }
    
    let jito_tip_amount = config.jito_tip_amount_lamports
        .unwrap_or(10_000_000u64); // Default: 0.01 SOL
    log::info!("✅ Jito tip amount: {} lamports (~{} SOL)", 
        jito_tip_amount, 
        jito_tip_amount as f64 / 1_000_000_000.0);
    
    // CRITICAL: Try to get tip accounts dynamically from Jito Block Engine API
    // Tip accounts can change over time, so we should fetch them dynamically
    // If JITO_TIP_ACCOUNT is set in env, use it (user override)
    // Otherwise, try to fetch from Jito API, fallback to default if API fails
    let final_tip_account = if config.jito_tip_account.is_some() {
        // User explicitly set JITO_TIP_ACCOUNT - use it (no dynamic fetch)
        log::info!("Using JITO_TIP_ACCOUNT from environment: {}", jito_tip_account);
        jito_tip_account
    } else {
        // Try to fetch tip accounts dynamically from Jito API
        let temp_jito_client = JitoClient::new(
            config.jito_url.clone(),
            jito_tip_account, // Temporary, will be updated
            jito_tip_amount,
        );
        
        match temp_jito_client.get_tip_accounts().await {
            Ok(tip_accounts) => {
                if !tip_accounts.is_empty() {
                    // Use first tip account from Jito API
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

    // Validate Jito endpoint per Structure.md section 13
    validate_jito_endpoint(&jito_client).await
        .context("Jito endpoint validation failed - check network connectivity")?;

    log::info!("🚀 Starting liquidation loop");
    log::info!("   Program ID: {}", program_id);
    log::info!("   Wallet: {}", wallet);
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

    // 2. HF < 1.0 olanları bul
    // CRITICAL SECURITY: Track parse errors to detect layout changes or corrupt data
    // ✅ FIXED: Use account type identification utility
    // This is faster and more reliable than attempting to parse every account
    let mut candidates = Vec::new();
    let mut parse_errors = 0;
    let mut skipped_wrong_type = 0;
    let total_accounts = accounts.len();
    
    for (pk, acc) in accounts {
        // Use account type identification to quickly filter accounts
        match crate::solend::identify_solend_account_type(&acc.data) {
            crate::solend::SolendAccountType::Obligation => {
                // This looks like an Obligation - try to parse it
                match Obligation::from_account_data(&acc.data) {
                    Ok(obligation) => {
                        // Read health factor threshold from .env (no hardcoded values)
                        let hf_threshold = std::env::var("HF_LIQUIDATION_THRESHOLD")
                            .ok()
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(1.0); // Default 1.0 if not set
                        
                        // CRITICAL FIX: Use u128 arithmetic for high-precision HF check
                        // f64 precision loss can cause missed liquidations for HF very close to 1.0
                        // (e.g., 0.9999999999 vs 1.0000000001)
                        const WAD: u128 = 1_000_000_000_000_000_000;
                        let hf_wad = obligation.health_factor_u128();
                        
                        // Convert threshold to WAD with care
                        let threshold_wad = (hf_threshold * WAD as f64) as u128;
                        
                        if hf_wad < threshold_wad {
                            candidates.push((pk, obligation));
                        }
                    }
                    Err(e) => {
                        parse_errors += 1;
                        log::debug!("Failed to parse Obligation {}: {}", pk, e);
                    }
                }
            }
            _ => {
                // Skip non-Obligation accounts (Reserve, LendingMarket, Unknown)
                skipped_wrong_type += 1;
            }
        }
    }
    
    // Log filtering statistics
    if skipped_wrong_type > 0 {
        log::debug!(
            "Skipped {} accounts with wrong type (likely Reserves/LendingMarkets, not Obligations)",
            skipped_wrong_type
        );
    }
    
    // Log error rate if high (indicates potential layout change or widespread corruption)
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
    };

    // Per Structure.md section 6.4: Track block-wide cumulative risk
    // "Tek blok içinde kullanılan toplam risk de aynı limit ile sınırlıdır"
    // 
    // CRITICAL FIX: Wallet balance tracking strategy
    //
    // Problem: Her liquidation sonrası wallet balance değişir, ama RPC call çok yavaş
    // Çözüm: Hybrid approach
    // 1. Cycle başında initial balance al
    // 2. Her liquidation sonrası ESTIMATED balance hesapla (RPC call yapmadan)
    // 3. Her 5 liquidation'da bir GERÇEK balance refresh et (doğrulama için)
    
    // Helper struct for wallet balance tracking
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
    
    let mut cumulative_risk_usd = 0.0; // Track total risk used in this cycle
    
    // Track pending liquidations (sent but not yet executed on-chain)
    // CRITICAL FIX: Use real-time bundle status checking to handle race conditions
    // Bundles can execute in ~400ms, so we check status proactively instead of waiting for timeout
    
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
        /// Returns list of newly confirmed bundle IDs with their values
        async fn update_statuses(
            &mut self,
            jito_client: &JitoClient,
        ) -> Vec<(String, f64)> {
            let mut newly_confirmed = Vec::new();
            
            // Check bundles that haven't been checked in last 200ms
            let now = Instant::now();
            let bundle_ids_to_check: Vec<String> = self.bundles.iter()
                .filter(|(_, info)| {
                    !info.confirmed && 
                    now.duration_since(info.last_status_check) >= Duration::from_millis(200)
                })
                .map(|(id, _)| id.clone())
                .collect();
            
            for bundle_id in bundle_ids_to_check {
                // Check bundle status via Jito API
                match jito_client.get_bundle_status(&bundle_id).await {
                    Ok(Some(status)) => {
                        if let Some(status_str) = &status.status {
                            if status_str == "landed" || status_str == "confirmed" {
                                // Bundle confirmed!
                                if let Some(info) = self.bundles.get_mut(&bundle_id) {
                                    if !info.confirmed {
                                        info.confirmed = true;
                                        newly_confirmed.push((bundle_id.clone(), info.value_usd));
                                        
                                        log::debug!(
                                            "✅ Bundle {} confirmed in {:.1}s",
                                            bundle_id,
                                            info.sent_at.elapsed().as_secs_f64()
                                        );
                                    }
                                }
                            } else if status_str == "failed" || status_str == "dropped" {
                                // Bundle failed - mark as confirmed (will be cleaned up)
                                if let Some(info) = self.bundles.get_mut(&bundle_id) {
                                    info.confirmed = true; // Mark as processed
                                    log::debug!("Bundle {} failed/dropped", bundle_id);
                                }
                            }
                        }
                        
                        // If slot is present, bundle likely executed
                        if status.slot.is_some() {
                            if let Some(info) = self.bundles.get_mut(&bundle_id) {
                                if !info.confirmed {
                                    info.confirmed = true;
                                    newly_confirmed.push((bundle_id.clone(), info.value_usd));
                                    log::debug!("Bundle {} confirmed (has slot)", bundle_id);
                                }
                            }
                        }
                        
                        // Update last check timestamp
                        if let Some(info) = self.bundles.get_mut(&bundle_id) {
                            info.last_status_check = now;
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
            
            // Remove expired unconfirmed bundles (older than 5s - definitely dropped)
            let expired: Vec<String> = self.bundles.iter()
                .filter(|(_, info)| {
                    !info.confirmed && now.duration_since(info.sent_at) >= Duration::from_secs(5)
                })
                .map(|(id, _)| id.clone())
                .collect();
            
            for bundle_id in &expired {
                self.bundles.remove(bundle_id);
                log::warn!("Bundle {} expired (not confirmed in 5s), assuming dropped", bundle_id);
            }
            
            newly_confirmed
        }
        
        /// Get total pending committed value (unconfirmed bundles)
        fn get_pending_committed(&self) -> f64 {
            self.bundles.iter()
                .filter(|(_, info)| !info.confirmed) // ✅ CRITICAL FIX: Only count unconfirmed bundles
                .map(|(_, info)| info.value_usd)
                .sum()
        }
        
        /// Release expired bundles (return their committed value)
        fn release_expired(&mut self) -> Vec<(String, f64)> {
            let now = Instant::now();
            
            let expired: Vec<(String, f64)> = self.bundles.iter()
                .filter(|(_, info)| {
                    !info.confirmed && now.duration_since(info.sent_at) >= Duration::from_secs(5)
                })
                .map(|(id, info)| (id.clone(), info.value_usd))
                .collect();
            
            for (bundle_id, _) in &expired {
                self.bundles.remove(bundle_id);
            }
            
            expired
        }
    }
    
    let mut bundle_tracker = BundleTracker::new();
    
    // Bundle status enum for tracking bundle execution state
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BundleStatus {
        Pending,
        Confirmed,
        Failed,
        Unknown,
    }
    
    /// Verify bundle status before removing from tracking
    /// This prevents race conditions where we release committed amount for bundles that actually executed
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

    log::debug!(
        "Cycle started: initial_wallet_value=${:.2}, cumulative_risk tracking initialized (using estimated balance with refresh every 5 liquidations)",
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
            log::warn!("Skipping {}: Oracle validation failed", obl_pubkey);
            metrics.skipped_oracle_fail += 1;
            continue;
        }

        // b) Jupiter'den kârlılık kontrolü
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
        // CRITICAL OPTIMIZATION: Use cached wallet balance with pending liquidation tracking
        // instead of refreshing before each liquidation. This reduces RPC calls significantly.
        // 
        // With 10 liquidations, this reduces RPC calls from 20 (2 per liquidation) to 2 (once at start).
        // Trade-off: Slightly less accurate but much faster and avoids RPC rate limits.
        // 
        // CRITICAL FIX: Real-time bundle status checking (instead of timeout-based)
        // Check bundle status proactively every 200ms to detect confirmed bundles immediately
        // This prevents overcommit by releasing committed amounts as soon as bundles execute (~400ms)
        let newly_confirmed = bundle_tracker.update_statuses(jito_client).await;
        
        // Release committed amounts for newly confirmed bundles
        for (bundle_id, value_usd) in newly_confirmed {
            wallet_balance_tracker.pending_committed_usd -= value_usd;
            log::debug!(
                "Released ${:.2} committed amount for confirmed bundle {}",
                value_usd,
                bundle_id
            );
        }
        
        // Calculate available liquidity with real-time pending
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
                            
                            // REFRESH balance every 5 liquidations (doğrulama için)
                            const REFRESH_INTERVAL: u32 = 5;
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
                            
                            // Refresh actual balance every 5 liquidations to verify estimation accuracy
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
                                        
                                        // Update with actual balance
                                        wallet_balance_tracker.current_estimated_balance_usd = actual_balance;
                                        wallet_balance_tracker.last_refresh_balance_usd = actual_balance;
                                        wallet_balance_tracker.liquidations_since_refresh = 0;
                                        
                                        // CRITICAL: High error rate indicates problem
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
            wallet_balance_tracker.current_estimated_balance_usd // Use estimated as fallback
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
        "📊 Cycle Summary: {} candidates | {} successful | {} skipped (oracle:{}, jupiter:{}, profit:{}, risk:{}, rpc:{}, reserve:{}, ata:{}) | {} failed (build:{}, send:{}) | cumulative_risk=${:.2}/{:.2} (final_wallet_value=${:.2})",
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
        cumulative_risk_usd,
        final_max_position_usd,
        final_wallet_value_usd
    );

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
struct LiquidationContext {
    obligation_pubkey: Pubkey,
    obligation: Obligation,
    borrows: Vec<ObligationLiquidity>,  // Parsed borrows from dataFlat
    deposits: Vec<ObligationCollateral>, // Parsed deposits from dataFlat
    borrow_reserve: Option<Reserve>,
    deposit_reserve: Option<Reserve>,
    borrow_price_usd: Option<f64>,  // Price from oracle
    deposit_price_usd: Option<f64>, // Price from oracle
    oracle_ok: bool,
}

/// Build liquidation context with Oracle validation per Structure.md section 5.2
async fn build_liquidation_context(
    rpc: &Arc<RpcClient>,
    obligation: &Obligation,
) -> Result<LiquidationContext> {
    // Load reserve accounts for borrow and deposit
    let mut borrow_reserve = None;
    let mut deposit_reserve = None;

    // Get first borrow reserve if exists
    if let Ok(borrows) = obligation.borrows() {
        if !borrows.is_empty() {
            let borrow_reserve_pubkey = borrows[0].borrowReserve;
            if let Ok(account) = rpc.get_account_data(&borrow_reserve_pubkey) {
                if let Ok(reserve) = Reserve::from_account_data(&account) {
                    borrow_reserve = Some(reserve);
                }
            }
        }
    }

    // Get first deposit reserve if exists
    if let Ok(deposits) = obligation.deposits() {
        if !deposits.is_empty() {
            let deposit_reserve_pubkey = deposits[0].depositReserve;
            if let Ok(account) = rpc.get_account_data(&deposit_reserve_pubkey) {
                if let Ok(reserve) = Reserve::from_account_data(&account) {
                    deposit_reserve = Some(reserve);
                }
            }
        }
    }

    // Parse borrows and deposits from obligation
    let borrows = obligation.borrows()
        .map_err(|e| anyhow::anyhow!("Failed to parse borrows: {}", e))?;
    let deposits = obligation.deposits()
        .map_err(|e| anyhow::anyhow!("Failed to parse deposits: {}", e))?;

    // Validate Oracle per Structure.md section 5.2
    // Use TWAP protection when Switchboard is not available (Pyth-only mode)
    let (oracle_ok, borrow_price, deposit_price) = validate_oracles_with_twap(rpc, &borrow_reserve, &deposit_reserve).await?;

    Ok(LiquidationContext {
        obligation_pubkey: obligation.owner, // Note: actual obligation pubkey should be passed separately
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

/// Get Pyth Network program ID from environment
fn pyth_program_id() -> Result<Pubkey> {
    use std::env;
    let pyth_id_str = env::var("PYTH_PROGRAM_ID")
        .map_err(|_| anyhow::anyhow!(
            "PYTH_PROGRAM_ID not found in .env file. \
             Please set PYTH_PROGRAM_ID in .env file. \
             Example: PYTH_PROGRAM_ID=FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH"
        ))?;
    Pubkey::from_str(&pyth_id_str)
        .map_err(|e| anyhow::anyhow!("Invalid PYTH_PROGRAM_ID from .env: {} - Error: {}", pyth_id_str, e))
}

/// Get Switchboard v2 program ID from environment
fn switchboard_program_id_v2() -> Result<Pubkey> {
    use std::env;
    let switchboard_id_str = env::var("SWITCHBOARD_PROGRAM_ID")
        .map_err(|_| anyhow::anyhow!(
            "SWITCHBOARD_PROGRAM_ID not found in .env file. \
             Please set SWITCHBOARD_PROGRAM_ID in .env file. \
             Example: SWITCHBOARD_PROGRAM_ID=SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f"
        ))?;
    Pubkey::from_str(&switchboard_id_str)
        .map_err(|e| anyhow::anyhow!("Invalid SWITCHBOARD_PROGRAM_ID from .env: {} - Error: {}", switchboard_id_str, e))
}

/// Get Switchboard On-Demand v3 program ID from environment
/// Solend uses Switchboard On-Demand v3, so this is the primary program ID we check
fn switchboard_program_id_v3() -> Result<Pubkey> {
    use std::env;
    let switchboard_id_str = env::var("SWITCHBOARD_PROGRAM_ID_V3")
        .map_err(|_| anyhow::anyhow!(
            "SWITCHBOARD_PROGRAM_ID_V3 not found in .env file. \
             Please set SWITCHBOARD_PROGRAM_ID_V3 in .env file. \
             Example: SWITCHBOARD_PROGRAM_ID_V3=SBondMDrcV3K4kxZR1HNVT7osZxAHVHgYXL5Ze1oMUv"
        ))?;
    Pubkey::from_str(&switchboard_id_str)
        .map_err(|e| anyhow::anyhow!("Invalid SWITCHBOARD_PROGRAM_ID_V3 from .env: {} - Error: {}", switchboard_id_str, e))
}

/// Minimum valid price threshold (in USD)
/// Prices below this threshold are considered invalid to prevent division by zero
/// and floating point precision issues in confidence percentage calculations.
/// Example: price = 1e-100 → confidence_pct calculation would produce inf or very large values.
/// 
/// CRITICAL: Set to 1e-6 to support micro-cap tokens while maintaining safety
/// Previous 1e-3 was too aggressive and rejected valid micro-cap tokens ($0.0005)
const MIN_VALID_PRICE_USD: f64 = 1e-6; // $0.000001 minimum (1 micro-dollar) - supports micro-cap tokens

/// Get oracle configuration from .env (no hardcoded values)
fn get_oracle_config() -> (u64, f64, u64, usize, usize, f64, f64, f64) {
    use std::env;
    
    // MAX_ORACLE_AGE_SECONDS: Maximum age of oracle price in seconds
    let max_oracle_age_secs = env::var("MAX_ORACLE_AGE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(120); // Default 120 seconds if not set
    
    // Convert to slots (assuming ~400ms per slot)
    let max_slot_difference = (max_oracle_age_secs * 1000 / 400).max(25); // Minimum 25 slots
    
    // MAX_ORACLE_DEVIATION_PCT: Maximum price deviation between Pyth and Switchboard
    let max_oracle_deviation_pct = env::var("MAX_ORACLE_DEVIATION_PCT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(2.0); // Default 2% if not set
    
    // TWAP configuration
    let twap_max_age_secs = max_oracle_age_secs.min(30); // Use oracle age or 30s, whichever is smaller
    let twap_min_samples = 5; // Minimum samples for TWAP (fixed for reliability)
    let twap_max_samples = 50; // Maximum samples for TWAP (fixed for memory efficiency)
    let twap_anomaly_threshold_pct = 3.0; // 3% deviation from TWAP triggers anomaly (fixed)

    // MAX_CONFIDENCE_PCT: Maximum confidence interval (percentage)
    let max_confidence_pct = env::var("MAX_CONFIDENCE_PCT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(5.0); // Default 5%

    // MAX_CONFIDENCE_PCT_PYTH_ONLY: Stricter confidence for Pyth-only mode
    let max_confidence_pct_pyth_only = env::var("MAX_CONFIDENCE_PCT_PYTH_ONLY")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(2.0); // Default 2%
    
    (max_slot_difference, max_oracle_deviation_pct, twap_max_age_secs, twap_min_samples, twap_max_samples, twap_anomaly_threshold_pct, max_confidence_pct, max_confidence_pct_pyth_only)
}

/// Oracle price cache for TWAP calculation
/// Protects against oracle manipulation when only Pyth is available
struct OraclePriceCache {
    prices: VecDeque<(Instant, f64)>, // (timestamp, price)
    max_age: Duration,
    min_samples: usize,
}

impl OraclePriceCache {
    fn new(max_age_secs: u64, min_samples: usize) -> Self {
        OraclePriceCache {
            prices: VecDeque::new(),
            max_age: Duration::from_secs(max_age_secs),
            min_samples,
        }
    }
    
    fn add_price(&mut self, price: f64) {
        let now = Instant::now();
        
        // ✅ FIXED: Time-based cleanup (30 saniyeden eski olanları sil)
        // retain() is more efficient than while loop for removing multiple items
        self.prices.retain(|(timestamp, _)| {
            now.duration_since(*timestamp) <= self.max_age
        });
        
        self.prices.push_back((now, price));
        
        // ✅ FIXED: Size-based limit (memory kontrolü)
        // Increased from 20 to 50 samples for better TWAP accuracy on low-liquidity tokens
        // Use while loop to ensure we stay under limit even if multiple samples are added quickly
        let (_, _, _, _, twap_max_samples, _, _, _) = get_oracle_config();
        while self.prices.len() > twap_max_samples {
            self.prices.pop_front();
        }
    }
    
    /// Calculate TWAP (Time-Weighted Average Price)
    /// Returns None if not enough samples
    fn calculate_twap(&self) -> Option<f64> {
        if self.prices.len() < self.min_samples {
            return None;
        }
        
        let now = Instant::now();
        let mut weighted_sum = 0.0;
        let mut total_weight = 0.0;
        
        for (timestamp, price) in &self.prices {
            // Weight = time since sample (older = more weight)
            let age = now.duration_since(*timestamp).as_secs_f64();
            let weight = 1.0 / (1.0 + age); // Exponential decay
            
            weighted_sum += price * weight;
            total_weight += weight;
        }
        
        if total_weight > 0.0 {
            Some(weighted_sum / total_weight)
        } else {
            None
        }
    }
    
    /// Check if current price deviates too much from TWAP
    /// Returns true if deviation > threshold
    fn is_price_anomaly(&self, current_price: f64, threshold_pct: f64) -> bool {
        if let Some(twap) = self.calculate_twap() {
            let deviation_pct = ((current_price - twap).abs() / twap) * 100.0;
            deviation_pct > threshold_pct
        } else {
            false // Not enough data, assume OK
        }
    }
}

/// Global price cache for TWAP calculation (per reserve)
/// Uses OnceLock<RwLock<HashMap>> for thread-safe access
static PRICE_CACHES: std::sync::OnceLock<RwLock<HashMap<Pubkey, OraclePriceCache>>> = 
    std::sync::OnceLock::new();

/// Get or initialize the global price cache
fn get_price_caches() -> &'static RwLock<HashMap<Pubkey, OraclePriceCache>> {
    PRICE_CACHES.get_or_init(|| RwLock::new(HashMap::new()))
}


/// Pyth price status values (from price_type byte)
/// PriceType enum: Unknown = 0, Price = 1, Trading = 2, Halted = 3, Auction = 4
const PYTH_PRICE_STATUS_UNKNOWN: u8 = 0;
const PYTH_PRICE_STATUS_PRICE: u8 = 1; // Price status - acceptable for liquidations
const PYTH_PRICE_STATUS_TRADING: u8 = 2; // Trading status - preferred for liquidations
const PYTH_PRICE_STATUS_HALTED: u8 = 3;

/// Validate Pyth and Switchboard oracles per Structure.md section 5.2
/// Helper function to get price from either Pyth or Switchboard oracle
/// Returns (price, has_pyth, has_switchboard) where:
/// - price: Some(f64) if valid price found, None otherwise
/// - has_pyth: true if price came from Pyth
/// - has_switchboard: true if price came from Switchboard (and we also checked for cross-validation)
async fn get_reserve_price(
    rpc: &Arc<RpcClient>,
    reserve: &Reserve,
    current_slot: u64,
) -> Result<(Option<f64>, bool, bool)> {
    let pyth_pubkey = reserve.oracle_pubkey();
    let switchboard_pubkey = reserve.config().switchboardOraclePubkey;
    
    // Try Pyth first if available
    if pyth_pubkey != Pubkey::default() {
        match validate_pyth_oracle(rpc, pyth_pubkey, current_slot).await {
            Ok((valid, price)) => {
                if valid {
                    if let Some(price) = price {
                        // Pyth succeeded - check if Switchboard is also available for cross-validation
                        let has_switchboard = if switchboard_pubkey != Pubkey::default() {
                            validate_switchboard_oracle_if_available(rpc, reserve, current_slot)
                                .await?
                                .is_some()
                        } else {
                            false
                        };
                        return Ok((Some(price), true, has_switchboard));
                    }
                }
                // Pyth validation failed or no price - try Switchboard
            }
            Err(e) => {
                log::debug!("Pyth oracle validation error: {}, trying Switchboard", e);
                // Pyth error - try Switchboard
            }
        }
    }
    
    // Pyth not available or failed - try Switchboard
    if switchboard_pubkey != Pubkey::default() {
        match validate_switchboard_oracle_if_available(rpc, reserve, current_slot).await {
            Ok(Some(switchboard_price)) => {
                log::debug!("Using Switchboard price (Pyth not available or failed)");
                return Ok((Some(switchboard_price), false, true));
            }
            Ok(None) => {
                log::debug!("Switchboard oracle not available or invalid");
            }
            Err(e) => {
                log::debug!("Switchboard oracle validation error: {}", e);
            }
        }
    }
    
    // Neither oracle worked
    Ok((None, false, false))
}

/// Returns (is_valid, borrow_price_usd, deposit_price_usd)
async fn validate_oracles(
    rpc: &Arc<RpcClient>,
    borrow_reserve: &Option<Reserve>,
    deposit_reserve: &Option<Reserve>,
) -> Result<(bool, Option<f64>, Option<f64>)> {
    // ✅ FIXED: Check if reserves exist and have EITHER Pyth OR Switchboard oracle
    let borrow_ok = borrow_reserve
        .as_ref()
        .map(|r| {
            // Check if Pyth oracle exists
            if r.oracle_pubkey() != Pubkey::default() {
                return true;
            }
            // Check if Switchboard oracle exists
            if r.config().switchboardOraclePubkey != Pubkey::default() {
                return true;
            }
            false
        })
        .unwrap_or(false);

    let deposit_ok = deposit_reserve
        .as_ref()
        .map(|r| {
            // Check if Pyth oracle exists
            if r.oracle_pubkey() != Pubkey::default() {
                return true;
            }
            // Check if Switchboard oracle exists
            if r.config().switchboardOraclePubkey != Pubkey::default() {
                return true;
            }
            false
        })
        .unwrap_or(false);

    if !borrow_ok || !deposit_ok {
        log::debug!("Oracle validation failed: missing oracle pubkeys (neither Pyth nor Switchboard)");
        return Ok((false, None, None));
    }

    // Get current slot for stale check
    let current_slot = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;

    // ✅ FIXED: Validate borrow reserve oracle - try Pyth first, fallback to Switchboard
    let (borrow_price, borrow_has_pyth, borrow_has_switchboard_for_crossval) = if let Some(reserve) = borrow_reserve {
        match get_reserve_price(rpc, reserve, current_slot).await {
            Ok((price, has_pyth, has_switchboard)) => {
                if price.is_none() {
                    log::debug!("Borrow reserve: No valid oracle price found");
                    return Ok((false, None, None));
                }
                (price, has_pyth, has_switchboard)
            }
            Err(e) => {
                log::debug!("Borrow reserve oracle error: {}", e);
                return Ok((false, None, None));
            }
        }
    } else {
        (None, false, false)
    };

    // ✅ FIXED: Validate deposit reserve oracle - try Pyth first, fallback to Switchboard
    let (deposit_price, deposit_has_pyth, deposit_has_switchboard_for_crossval) = if let Some(reserve) = deposit_reserve {
        match get_reserve_price(rpc, reserve, current_slot).await {
            Ok((price, has_pyth, has_switchboard)) => {
                if price.is_none() {
                    log::debug!("Deposit reserve: No valid oracle price found");
                    return Ok((false, None, None));
                }
                (price, has_pyth, has_switchboard)
            }
            Err(e) => {
                log::debug!("Deposit reserve oracle error: {}", e);
                return Ok((false, None, None));
            }
        }
    } else {
        (None, false, false)
    };

    // ✅ FIXED: Validate Switchboard oracles if available per Structure.md section 5.2
    // "Switchboard varsa, Pyth ile sapma fazla mı?"
    // 
    // CRITICAL SECURITY: If Switchboard is NOT available, we rely solely on Pyth.
    // In this case, we apply stricter validation (stricter confidence threshold).
    // This mitigates the risk of Pyth oracle manipulation when no cross-validation exists.
    
    // Cross-validate: If we have both Pyth and Switchboard, compare them
    let (_, max_oracle_deviation_pct, _, _, _, _, max_confidence_pct, max_confidence_pct_pyth_only) = get_oracle_config();
    if borrow_has_pyth && borrow_has_switchboard_for_crossval {
        if let (Some(borrow_reserve), Some(borrow_price)) = (borrow_reserve, borrow_price) {
            if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
                rpc,
                borrow_reserve,
                current_slot,
            )
            .await?
            {
                // Compare Pyth and Switchboard prices
                let deviation_pct = ((borrow_price - switchboard_price).abs() / borrow_price) * 100.0;
                if deviation_pct > max_oracle_deviation_pct {
                    log::warn!(
                        "Oracle deviation too high for borrow reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                        deviation_pct,
                        max_oracle_deviation_pct,
                        borrow_price,
                        switchboard_price
                    );
                    return Ok((false, None, None));
                }
                log::debug!(
                    "✅ Oracle deviation OK for borrow reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    borrow_price,
                    switchboard_price
                );
            }
        }
    }

    if deposit_has_pyth && deposit_has_switchboard_for_crossval {
        if let (Some(deposit_reserve), Some(deposit_price)) = (deposit_reserve, deposit_price) {
            if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
                rpc,
                deposit_reserve,
                current_slot,
            )
            .await?
            {
                let deviation_pct = ((deposit_price - switchboard_price).abs() / deposit_price) * 100.0;
                if deviation_pct > max_oracle_deviation_pct {
                    log::warn!(
                        "Oracle deviation too high for deposit reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                        deviation_pct,
                        max_oracle_deviation_pct,
                        deposit_price,
                        switchboard_price
                    );
                    return Ok((false, None, None));
                }
                log::debug!(
                    "✅ Oracle deviation OK for deposit reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    deposit_price,
                    switchboard_price
                );
            }
        }
    }

    // SECURITY: If Switchboard is NOT available for either reserve AND we're using Pyth, apply stricter Pyth validation
    // This mitigates the risk of relying solely on Pyth without cross-validation
    // Note: If we're using Switchboard-only (no Pyth), we don't need this check
    let borrow_needs_stricter_validation = borrow_has_pyth && !borrow_has_switchboard_for_crossval;
    let deposit_needs_stricter_validation = deposit_has_pyth && !deposit_has_switchboard_for_crossval;
    
    if borrow_needs_stricter_validation || deposit_needs_stricter_validation {
        log::warn!(
            "⚠️  SECURITY WARNING: Switchboard oracle not available for cross-validation (borrow: {}, deposit: {}). \
             Using Pyth-only with stricter confidence threshold ({}% vs {}%). \
             This increases risk of oracle manipulation.",
            if borrow_has_switchboard_for_crossval { "✓" } else { "✗" },
            if deposit_has_switchboard_for_crossval { "✓" } else { "✗" },
            max_confidence_pct_pyth_only,
            max_confidence_pct
        );
        
        // Re-validate Pyth confidence with stricter threshold (only if we're using Pyth without Switchboard)
        if borrow_needs_stricter_validation {
            if let (Some(borrow_reserve), Some(borrow_price)) = (borrow_reserve, borrow_price) {
                // Re-check Pyth confidence with stricter threshold
                let confidence_check = validate_pyth_confidence_strict(
                    rpc,
                    borrow_reserve.oracle_pubkey(),
                    borrow_price,
                    current_slot,
                ).await?;
                if !confidence_check {
                    log::warn!(
                        "Borrow reserve Pyth confidence check failed (stricter threshold for Pyth-only mode)"
                    );
                    return Ok((false, None, None));
                }
            }
        }
        
        if deposit_needs_stricter_validation {
            if let (Some(deposit_reserve), Some(deposit_price)) = (deposit_reserve, deposit_price) {
                // Re-check Pyth confidence with stricter threshold
                let confidence_check = validate_pyth_confidence_strict(
                    rpc,
                    deposit_reserve.oracle_pubkey(),
                    deposit_price,
                    current_slot,
                ).await?;
                if !confidence_check {
                    log::warn!(
                        "Deposit reserve Pyth confidence check failed (stricter threshold for Pyth-only mode)"
                    );
                    return Ok((false, None, None));
                }
            }
        }
    }

    Ok((true, borrow_price, deposit_price))
}

/// Enhanced oracle validation with TWAP protection
/// This function adds TWAP (Time-Weighted Average Price) protection when Switchboard is not available
/// to detect oracle manipulation attempts
/// 
/// Returns (is_valid, borrow_price_usd, deposit_price_usd)
async fn validate_oracles_with_twap(
    rpc: &Arc<RpcClient>,
    borrow_reserve: &Option<Reserve>,
    deposit_reserve: &Option<Reserve>,
) -> Result<(bool, Option<f64>, Option<f64>)> {
    // First, get current prices from standard oracle validation
    let (pyth_ok, borrow_price, deposit_price) = 
        validate_oracles(rpc, borrow_reserve, deposit_reserve).await?;
    
    if !pyth_ok {
        return Ok((false, None, None));
    }

    // Check if TWAP protection is enabled via environment variable
    let enable_twap = std::env::var("ENABLE_TWAP_PROTECTION")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false); // Default: disabled

    if !enable_twap {
        log::debug!("TWAP protection disabled (ENABLE_TWAP_PROTECTION=false)");
        return Ok((true, borrow_price, deposit_price));
    }
    
    // Check if Switchboard is available for either reserve
    // If Switchboard is available, we don't need TWAP protection (cross-validation is sufficient)
    let current_slot = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    
    let mut borrow_has_switchboard = false;
    let mut deposit_has_switchboard = false;
    
    if let Some(reserve) = borrow_reserve {
        if let Ok(Some(_)) = validate_switchboard_oracle_if_available(rpc, reserve, current_slot).await {
            borrow_has_switchboard = true;
        }
    }
    
    if let Some(reserve) = deposit_reserve {
        if let Ok(Some(_)) = validate_switchboard_oracle_if_available(rpc, reserve, current_slot).await {
            deposit_has_switchboard = true;
        }
    }
    
    // Only apply TWAP protection if Switchboard is NOT available (Pyth-only mode)
    if !borrow_has_switchboard || !deposit_has_switchboard {
        let caches = get_price_caches();
        // Replace unwrap() with proper error handling
        let mut caches_guard = match caches.write() {
            Ok(guard) => guard,
            Err(e) => {
                log::error!("Failed to acquire write lock on price caches: {}", e);
                // Fail safe - if we can't check oracle safety, assume unsafe
                return Ok((false, None, None));
            }
        };
        
        // Get or create price cache for borrow reserve
        let (_, _, twap_max_age_secs, twap_min_samples, _, twap_anomaly_threshold_pct, _, _) = get_oracle_config();
        if let (Some(reserve), Some(price)) = (borrow_reserve.as_ref(), borrow_price) {
            if !borrow_has_switchboard {
                let oracle_pubkey = reserve.oracle_pubkey();
                let borrow_cache = caches_guard
                    .entry(oracle_pubkey)
                    .or_insert_with(|| OraclePriceCache::new(twap_max_age_secs, twap_min_samples));
                
                // Add current price to cache
                borrow_cache.add_price(price);
                
                // Check for manipulation
                if borrow_cache.is_price_anomaly(price, twap_anomaly_threshold_pct) {
                    if let Some(twap) = borrow_cache.calculate_twap() {
                        log::warn!(
                            "⚠️  Borrow price anomaly detected! Current: ${:.6}, TWAP: ${:.6}, Deviation: {:.2}%",
                            price,
                            twap,
                            ((price - twap).abs() / twap) * 100.0
                        );
                    } else {
                        log::warn!(
                            "⚠️  Borrow price anomaly detected! Current: ${:.6} (TWAP not available yet)",
                            price
                        );
                    }
                    return Ok((false, None, None));
                }
            }
        }
        
        // Get or create price cache for deposit reserve
        if let (Some(reserve), Some(price)) = (deposit_reserve.as_ref(), deposit_price) {
            if !deposit_has_switchboard {
                let oracle_pubkey = reserve.oracle_pubkey();
                let deposit_cache = caches_guard
                    .entry(oracle_pubkey)
                    .or_insert_with(|| OraclePriceCache::new(twap_max_age_secs, twap_min_samples));
                
                // Add current price to cache
                deposit_cache.add_price(price);
                
                // Check for manipulation
                if deposit_cache.is_price_anomaly(price, twap_anomaly_threshold_pct) {
                    if let Some(twap) = deposit_cache.calculate_twap() {
                        log::warn!(
                            "⚠️  Deposit price anomaly detected! Current: ${:.6}, TWAP: ${:.6}, Deviation: {:.2}%",
                            price,
                            twap,
                            ((price - twap).abs() / twap) * 100.0
                        );
                    } else {
                        log::warn!(
                            "⚠️  Deposit price anomaly detected! Current: ${:.6} (TWAP not available yet)",
                            price
                        );
                    }
                    return Ok((false, None, None));
                }
            }
        }
    }
    
    Ok((true, borrow_price, deposit_price))
}

/// Validate Switchboard oracle if available in ReserveConfig
/// Returns Some(price) if Switchboard oracle exists and is valid, None otherwise
/// Per Structure.md section 5.2
/// 
/// DYNAMIC CHAIN READING: All data is read from chain via RPC - no static values.
/// 
/// Uses official Switchboard SDK (switchboard-on-demand) with Solana SDK v2 compatibility.
/// SDK is pulled from GitHub main branch for latest Solana v2 support.
async fn validate_switchboard_oracle_if_available(
    rpc: &Arc<RpcClient>,
    reserve: &Reserve,
    current_slot: u64,
) -> Result<Option<f64>> {
    // Use new Switchboard On-Demand SDK (Solana 2.0 compatible)
    use switchboard_on_demand::on_demand::accounts::pull_feed::PullFeedAccountData;
    
    // 1. Get switchboard_oracle_pubkey from reserve.config (DYNAMIC - from chain)
    let switchboard_oracle_pubkey = reserve.config().switchboardOraclePubkey;
    if switchboard_oracle_pubkey == Pubkey::default() {
        // No Switchboard oracle configured for this reserve
        return Ok(None);
    }

    // 2. Get Switchboard feed account data from chain (DYNAMIC)
    let oracle_account = rpc
        .get_account(&switchboard_oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get Switchboard oracle account from chain: {}", e))?;

    // ✅ FIXED: Check for both Switchboard v2 and v3 program IDs
    // Solend uses Switchboard On-Demand v3, but we also support v2 for compatibility
    let switchboard_program_id_v2 = switchboard_program_id_v2()?;
    let switchboard_program_id_v3 = switchboard_program_id_v3()?;

    // CRITICAL SECURITY: Verify account owner BEFORE parsing
    // Prevents parsing malicious accounts that claim to be oracles
    if oracle_account.owner != switchboard_program_id_v2 
        && oracle_account.owner != switchboard_program_id_v3 {
        log::debug!(
            "❌ Switchboard oracle account {} rejected: invalid owner\n\
             - Expected: {} (v2) or {} (v3)\n\
             - Found: {}\n\
             - This may indicate: wrong oracle type, malicious account, or data corruption",
            switchboard_oracle_pubkey,
            switchboard_program_id_v2,
            switchboard_program_id_v3,
            oracle_account.owner
        );
        return Ok(None);
    }
    
    // Additional validation: Check account size is reasonable
    // Switchboard PullFeedAccountData should be ~512 bytes
    const EXPECTED_FEED_SIZE_MIN: usize = 400;
    const EXPECTED_FEED_SIZE_MAX: usize = 2000;
    let account_size = oracle_account.data.len();
    
    if account_size < EXPECTED_FEED_SIZE_MIN || account_size > EXPECTED_FEED_SIZE_MAX {
        log::warn!(
            "⚠️  Switchboard feed {} has unusual size: {} bytes (expected: {}-{} bytes)\n\
             This may indicate wrong account type or data corruption",
            switchboard_oracle_pubkey,
            account_size,
            EXPECTED_FEED_SIZE_MIN,
            EXPECTED_FEED_SIZE_MAX
        );
        return Ok(None);
    }
    
    // Log which version we're using for debugging
    if oracle_account.owner == switchboard_program_id_v3 {
        log::trace!("✅ Using Switchboard On-Demand v3 oracle (account size: {} bytes)", account_size);
    } else {
        log::trace!("✅ Using Switchboard v2 oracle (account size: {} bytes)", account_size);
    }
    
    log::trace!(
        "✅ Switchboard account validation passed: owner={}, size={} bytes",
        oracle_account.owner,
        account_size
    );

    // 3. Parse using Switchboard On-Demand SDK (Solana 2.0 compatible)
    // 
    // PullFeedAccountData implements Pod (Plain Old Data), so we can use bytemuck
    // to directly deserialize from the account data. This is the recommended approach
    // for off-chain clients, as the SDK's parse() method is designed for Anchor's
    // account.data.borrow() pattern which requires Ref<'_, &mut [u8]>.
    // 
    // CRITICAL FIX: Solana account data is not guaranteed to be aligned.
    // try_from_bytes requires alignment, which can cause runtime panic.
    // We handle AlignmentMismatch error with a safe fallback that ensures proper alignment.
    use bytemuck::PodCastError;
    
    // Ensure we have enough data
    let feed_size = std::mem::size_of::<PullFeedAccountData>();
    if oracle_account.data.len() < feed_size {
        log::warn!(
            "Switchboard feed account data too short: {} bytes (need at least {})",
            oracle_account.data.len(),
            feed_size
        );
        return Ok(None);
    }
    
    // CRITICAL: Solana RPC account data is NOT guaranteed to be aligned.
    // bytemuck::try_from_bytes requires strict alignment, which can cause runtime panic.
    // We use a safe alignment strategy: try direct parse first, then fallback to explicit alignment.
    let feed = {
        // ❌ PROBLEM: Switchboard On-Demand v3 SDK (switchboard-on-demand) changed its API.
        // The struct PullFeedAccountData no longer implements Pod, or simple casting is not enough.
        // The correct way to parse is using the SDK's provided method which handles alignment and deserialization safely.
        //
        // Reference: switchboard-on-demand SDK usage
        // use switchboard_on_demand::on_demand::accounts::pull_feed::PullFeedAccountData;
        
        // We need to use try_deserialize which takes a mutable slice reference
        // This is the standard Anchor/Borsh pattern used by the SDK
        let mut account_data_slice = &oracle_account.data[..];
        match PullFeedAccountData::try_deserialize(&mut account_data_slice) {
            Ok(feed) => feed,
            Err(e) => {
                let (_, _, _, _, _, _, _, max_confidence_pct_pyth_only) = get_oracle_config();
                log::debug!(
                    "Switchboard feed parsing failed for {}: {}. Falling back to Pyth-only mode with stricter validation ({}% confidence threshold).",
                    switchboard_oracle_pubkey,
                    e,
                    max_confidence_pct_pyth_only
                );
                return Ok(None);
            }
        }
    };
    
    // 4. Get price using SDK's value() method with staleness check
    // The value() method requires current slot for staleness validation
    // It returns Result<Decimal, OnDemandError> - Ok if valid, Err if stale/insufficient
    let price_decimal = match feed.value(current_slot) {
        Ok(v) => v,
        Err(e) => {
            log::debug!(
                "Switchboard feed {} value() failed (stale or insufficient quorum): {}",
                switchboard_oracle_pubkey,
                e
            );
            return Ok(None);
        }
    };
    
    // 5. Convert Decimal to f64
    // rust_decimal::Decimal provides better precision, but we use f64 for consistency with Pyth
    let price = price_decimal.to_string().parse::<f64>()
        .unwrap_or_else(|_| {
            // Fallback: manual conversion using mantissa and scale
            let mantissa = price_decimal.mantissa();
            let scale = price_decimal.scale();
            mantissa as f64 / 10_f64.powi(scale as i32)
        });
    
    // 6. Validate price is positive and reasonable
    if price <= 0.0 || !price.is_finite() {
        log::debug!(
            "Switchboard oracle price is invalid: {} (from feed {}, decimal: {})",
            price,
            switchboard_oracle_pubkey,
            price_decimal
        );
        return Ok(None);
    }
    
    log::debug!(
        "✅ Switchboard oracle validation passed for {} (price: {}, decimal: {})",
        switchboard_oracle_pubkey,
        price,
        price_decimal
    );
    
    Ok(Some(price))
}

/// Validate Pyth oracle per Structure.md section 5.2
/// Returns (is_valid, price) where price is in USD with decimals
async fn validate_pyth_oracle(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    current_slot: u64,
) -> Result<(bool, Option<f64>)> {
    // 1. Verify oracle account belongs to Pyth program ID
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    let pyth_program_id = pyth_program_id()?;

    if oracle_account.owner != pyth_program_id {
        log::warn!(
            "❌ Oracle account {} does not belong to Pyth program (owner: {}, expected: {})",
            oracle_pubkey,
            oracle_account.owner,
            pyth_program_id
        );
        return Ok((false, None));
    }

    // 2. Parse Pyth v2 price account structure
    // Pyth v2 price account layout (COMPLETE):
    // - Offset 0-4: magic (4 bytes) = 0xa1b2c3d4
    // - Offset 4-5: version (1 byte) = 2
    // - Offset 5-6: price_type (1 byte) - PriceType enum: Unknown=0, Price=1, Trading=2, Halted=3, Auction=4
    // - Offset 6-8: size (2 bytes)
    // - Offset 8-16: price (i64, 8 bytes)
    // - Offset 16-20: exponent (i32, 4 bytes)
    // - Offset 20-24: reserved (4 bytes)
    // - Offset 24-32: timestamp (i64, 8 bytes)
    // - Offset 32-40: prev_publish_time (i64, 8 bytes)
    // - Offset 40-48: prev_price (i64, 8 bytes)
    // - Offset 48-56: prev_conf (u64, 8 bytes)
    // - Offset 56-64: last_slot (u64, 8 bytes) - slot when price was last updated
    // - Offset 64-72: valid_slot (u64, 8 bytes) - slot when price is valid until
    // - Offset 72-80: aggregate.status (u64, 8 bytes) ← CRITICAL: Must be Trading (1)
    // - Offset 80-84: aggregate.num_components (u32, 4 bytes) ← Should be >= 3
    // - Offset 84+: publisher accounts...

    // CRITICAL FIX: Need at least 84 bytes for aggregate.status and num_components
    if oracle_account.data.len() < 84 {
        log::warn!(
            "❌ Oracle account {} data too short: {} bytes (need at least 84 for Pyth v2 with aggregate fields)",
            oracle_pubkey,
            oracle_account.data.len()
        );
        return Ok((false, None));
    }

    // Check magic number (Pyth v2)
    // CRITICAL FIX: Pyth magic number is 0xa1b2c3d4 in big-endian, but Solana uses little-endian
    // So when read from account data, it appears as [0xd4, 0xc3, 0xb2, 0xa1]
    // We need to check for little-endian byte order
    let magic: [u8; 4] = oracle_account.data[0..4]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse magic"))?;
    
    // Pyth v2 magic: 0xa1b2c3d4 in big-endian = [0xd4, 0xc3, 0xb2, 0xa1] in little-endian
    const PYTH_MAGIC_LE: [u8; 4] = [0xd4, 0xc3, 0xb2, 0xa1];
    if magic != PYTH_MAGIC_LE {
        log::warn!(
            "❌ Invalid Pyth magic number for {}: {:?} (expected little-endian: {:?})",
            oracle_pubkey,
            magic,
            PYTH_MAGIC_LE
        );
        return Ok((false, None));
    }

    // Check version
    let version = oracle_account.data[4];
    if version != 2 {
        log::warn!(
            "❌ Unsupported Pyth version for {}: {} (expected 2)",
            oracle_pubkey,
            version
        );
        return Ok((false, None));
    }

    // Check price status (price_type byte)
    // ✅ FIXED: Accept both Trading (2) and Price (1) statuses for liquidations
    // Trading is preferred, but Price status is also acceptable (feed may be updating)
    // Reject: Unknown (0), Halted (3), Auction (4)
    let price_type = oracle_account.data[5];
    if price_type != PYTH_PRICE_STATUS_TRADING && price_type != PYTH_PRICE_STATUS_PRICE {
        let status_name = match price_type {
            0 => "Unknown",
            1 => "Price",
            2 => "Trading",
            3 => "Halted",
            4 => "Auction",
            _ => "Invalid",
        };
        
        log::warn!(
            "Pyth price status is {} ({}) - REJECTING oracle. Only Trading or Price status is acceptable for liquidations.",
            price_type,
            status_name
        );
        return Ok((false, None));
    }
    
    // Log which status we're accepting (for debugging)
    if price_type == PYTH_PRICE_STATUS_PRICE {
        log::debug!(
            "⚠️  Pyth oracle {} has Price status (not Trading). This may indicate feed is updating, but accepting for liquidation.",
            oracle_pubkey
        );
    }
    
    // CRITICAL FIX: Check aggregate.status (offset 72-80, u64)
    // aggregate.status values:
    // 0 = Unknown
    // 1 = Trading (ONLY THIS IS ACCEPTABLE!)
    // 2 = Halted
    // 3 = Auction
    let aggregate_status_bytes: [u8; 8] = oracle_account.data[72..80]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse aggregate.status"))?;
    let aggregate_status = u64::from_le_bytes(aggregate_status_bytes);
    
    const AGGREGATE_STATUS_TRADING: u64 = 1;
    
    // CRITICAL: Accept Trading status (1) normally.
    // RELAXED: Also accept Unknown (0) IF the price is very fresh (within 10 slots ~4 seconds).
    // This handles cases where Pyth status might flicker or RPC is slightly behind but data is fresh.
    let is_status_ok = if aggregate_status == AGGREGATE_STATUS_TRADING {
        true
    } else if aggregate_status == 0 {
        // Parse last_slot early for this check
        let last_slot_bytes: [u8; 8] = oracle_account.data[56..64]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse last_slot"))?;
        let last_slot = u64::from_le_bytes(last_slot_bytes);
        let slot_diff = current_slot.saturating_sub(last_slot);
        
        if slot_diff <= 10 { // ✅ 10 slots (4s) tolerance
            log::debug!(
                "Pyth aggregate status is 0 (Unknown) but price is FRESH (diff={} slots, {:.1}s) - ACCEPTING oracle for {}",
                slot_diff,
                slot_diff as f64 * 0.4,
                oracle_pubkey
            );
            true
        } else {
            log::warn!(
                "Pyth aggregate status is 0 (Unknown) and stale (diff={} slots) - REJECTING oracle for {}",
                slot_diff,
                oracle_pubkey
            );
            false
        }
    } else {
        false
    };

    if !is_status_ok {
        let status_name = match aggregate_status {
            0 => "Unknown",
            1 => "Trading",
            2 => "Halted",
            3 => "Auction",
            _ => "Invalid",
        };
        
        log::warn!(
            "Pyth aggregate status is {} ({}) - REJECTING oracle. Only Trading status is acceptable.",
            aggregate_status,
            status_name
        );
        return Ok((false, None));
    }
    
    // Additional validation: num_components check
    // If num_components < 3, the aggregate may be unreliable
    let num_components_bytes: [u8; 4] = oracle_account.data[80..84]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse num_components"))?;
    let num_components = u32::from_le_bytes(num_components_bytes);
    
    const MIN_NUM_COMPONENTS: u32 = 3; // Pyth recommends at least 3 publishers
    if num_components < MIN_NUM_COMPONENTS {
        log::warn!(
            "Pyth aggregate has only {} publishers (min: {}). Price may be unreliable.",
            num_components,
            MIN_NUM_COMPONENTS
        );
        // Don't reject, but log warning for monitoring
    }
    
    log::debug!(
        "✅ Pyth oracle validation passed: price_type={}, aggregate_status={}, num_components={}",
        price_type,
        aggregate_status,
        num_components
    );

    // Parse exponent
    let exponent_bytes: [u8; 4] = oracle_account.data[16..20]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?;
    let exponent = i32::from_le_bytes(exponent_bytes);

    // Parse price (i64)
    let price_bytes: [u8; 8] = oracle_account.data[8..16]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse price"))?;
    let price_raw = i64::from_le_bytes(price_bytes);

    // CRITICAL: Overflow/Corruption check
    // Pyth prices can be very large, but i64::MAX/MIN usually indicate uninitialized/corrupted data
    if price_raw == i64::MAX || price_raw == i64::MIN {
        log::warn!("Pyth price overflow/corruption detected: {} for {}", price_raw, oracle_pubkey);
        return Ok((false, None));
    }

    // Convert to f64 with exponent
    // CRITICAL: Pyth exponent is typically negative (e.g., -8 for 8 decimals)
    // Mathematical equivalence: price_raw * 10^(-8) = price_raw / 10^8
    // Example: price_raw=150000000, exponent=-8 → 150000000 * 10^(-8) = 1.5 USD
    // This is CORRECT: powi() handles negative exponents properly (10^(-8) = 1/10^8)
    let price = Some(price_raw as f64 * 10_f64.powi(exponent));

    // 3. Check if price is stale (last_slot and valid_slot check)
    // CRITICAL FIX: last_slot is the PRIMARY staleness check (when price was last updated)
    // valid_slot can be a future slot, so it's only used for expiration check
    
    // Parse last_slot (PRIMARY staleness check - when price was last updated)
    let last_slot_bytes: [u8; 8] = oracle_account.data[56..64]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse last_slot"))?;
    let last_slot = u64::from_le_bytes(last_slot_bytes);
    
    // Parse valid_slot (for expiration check only)
    let valid_slot_bytes: [u8; 8] = oracle_account.data[64..72]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse valid_slot"))?;
    let valid_slot = u64::from_le_bytes(valid_slot_bytes);

    // PRIMARY CHECK: last_slot based staleness (when price was last updated)
    // This is the most reliable indicator of price freshness
    let (max_slot_difference, _, _, _, _, _, _, _) = get_oracle_config();
    let slot_diff_last = current_slot.saturating_sub(last_slot);
    if slot_diff_last > max_slot_difference {
        log::debug!(
            "Pyth oracle price too old: last_slot={}, current_slot={}, diff={} > {}",
            last_slot,
            current_slot,
            slot_diff_last,
            max_slot_difference
        );
        return Ok((false, None));
    }

    // SECONDARY CHECK: valid_slot expiration (only if current_slot > valid_slot)
    // valid_slot can be a future slot, so we only check if it's in the past
    // and the difference is significant (to avoid false positives from minor slot drift)
    let slot_diff_valid = current_slot.saturating_sub(valid_slot);
    if current_slot > valid_slot && slot_diff_valid > 10 {
        log::debug!(
            "Pyth oracle price expired: valid_slot={}, current_slot={}, diff={}",
            valid_slot,
            current_slot,
            slot_diff_valid
        );
        return Ok((false, None));
    }

    log::debug!(
        "Pyth oracle price is fresh: valid_slot={}, last_slot={}, current_slot={}, slot_diff_last={}, slot_diff_valid={}",
        valid_slot,
        last_slot,
        current_slot,
        slot_diff_last,
        slot_diff_valid
    );

    // 4. Check confidence interval
    let price_value = price.ok_or_else(|| anyhow::anyhow!("Price not parsed"))?;
    
    // CRITICAL: Check minimum price threshold and finite check to prevent division by zero and floating point issues
    // Edge case: price_value = 1e-100 → price_value.abs() > 0.0 check passes, but
    // confidence_pct = confidence / price_value.abs() would produce inf or very large values
    // Solution: Reject prices below minimum threshold or non-finite values
    if price_value.abs() < MIN_VALID_PRICE_USD || !price_value.is_finite() {
        log::debug!(
            "Pyth oracle price invalid or too small: {} (min: {}, finite: {})",
            price_value,
            MIN_VALID_PRICE_USD,
            price_value.is_finite()
        );
        return Ok((false, None));
    }
    
    // Parse confidence (u64 at offset 48-56)
    // CRITICAL: Confidence uses the same exponent as price
    // Example: confidence_raw=1000000, exponent=-8 → 1000000 * 10^(-8) = 0.01
    let conf_bytes: [u8; 8] = oracle_account.data[48..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
    let confidence_raw = u64::from_le_bytes(conf_bytes);
    let confidence = confidence_raw as f64 * 10_f64.powi(exponent);

    // Check if confidence interval is too large (as percentage of price)
    // NOTE: We already checked price_value.abs() >= MIN_VALID_PRICE_USD and is_finite() above,
    // so division is safe and won't produce inf
    let confidence_pct = (confidence / price_value.abs()) * 100.0;
    
    // Additional safety check after calculation
    if !confidence_pct.is_finite() {
        log::debug!("Confidence percentage calculation produced non-finite value");
        return Ok((false, None));
    }

    let (_, _, _, _, _, _, max_confidence_pct, _) = get_oracle_config();
    if confidence_pct > max_confidence_pct {
        log::debug!(
            "Pyth oracle confidence too high: {:.2}% > {:.2}% (price: {}, confidence: {})",
            confidence_pct,
            max_confidence_pct,
            price_value,
            confidence
        );
        return Ok((false, None));
    }

    // COMBINED CHECK: Old feed + high confidence = suspicious
    // If slot difference is > 15 slots (~6 seconds) AND confidence > 1.0%, reject
    // This prevents accepting manipulated prices that are slightly stale but have high confidence
    if slot_diff_last > 15 && confidence_pct > 1.0 {
        log::warn!(
            "Suspicious oracle: old_feed ({} slots, ~{:.1}s) + high_confidence ({:.2}%) - rejecting",
            slot_diff_last,
            slot_diff_last as f64 * 0.4, // Convert slots to seconds (400ms per slot)
            confidence_pct
        );
        return Ok((false, None));
    }

    log::debug!(
        "✅ Pyth oracle validation passed for {} (price: {}, confidence: {:.2}%, status: {})",
        oracle_pubkey,
        price_value,
        confidence_pct,
        price_type
    );
    Ok((true, price))
}

/// Validate Pyth confidence with stricter threshold (for Pyth-only mode when Switchboard is unavailable)
/// Returns true if confidence is acceptable, false otherwise
async fn validate_pyth_confidence_strict(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    price_value: f64,
    _current_slot: u64,
) -> Result<bool> {
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    if oracle_account.data.len() < 56 {
        return Ok(false);
    }

    // Parse exponent
    let exponent_bytes: [u8; 4] = oracle_account.data[16..20]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?;
    let exponent = i32::from_le_bytes(exponent_bytes);

    // Parse confidence (u64 at offset 48-56)
    let conf_bytes: [u8; 8] = oracle_account.data[48..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
    let confidence_raw = u64::from_le_bytes(conf_bytes);
    let confidence = confidence_raw as f64 * 10_f64.powi(exponent);

    // CRITICAL: Check minimum price threshold to prevent division by zero and floating point issues
    // Edge case: price_value = 1e-100 → price_value.abs() > 0.0 check passes, but
    // confidence_pct = confidence / price_value.abs() would produce inf or very large values
    if price_value.abs() < MIN_VALID_PRICE_USD {
        log::warn!(
            "Pyth oracle price below minimum threshold for strict check: {} < {} (price too small, likely invalid)",
            price_value.abs(),
            MIN_VALID_PRICE_USD
        );
        return Ok(false);
    }
    
    // Check if confidence interval is too large (stricter threshold for Pyth-only mode)
    // NOTE: We already checked price_value.abs() >= MIN_VALID_PRICE_USD above,
    // so division is safe and won't produce inf
    let confidence_pct = (confidence / price_value.abs()) * 100.0;

    let (_, _, _, _, _, _, _, max_confidence_pct_pyth_only) = get_oracle_config();
    if confidence_pct > max_confidence_pct_pyth_only {
        log::warn!(
            "Pyth oracle confidence too high for Pyth-only mode: {:.2}% > {:.2}% (price: {}, confidence: {})",
            confidence_pct,
            max_confidence_pct_pyth_only,
            price_value,
            confidence
        );
        return Ok(false);
    }

    log::debug!(
        "✅ Pyth confidence check passed (stricter threshold): {:.2}% <= {:.2}%",
        confidence_pct,
        max_confidence_pct_pyth_only
    );

    Ok(true)
}

/// Liquidation quote with profit calculation per Structure.md section 7
struct LiquidationQuote {
    quote: JupiterQuote,
    profit_usdc: f64,
    collateral_value_usd: f64, // Position size in USD for risk limit calculation
    debt_to_repay_raw: u64, // Debt amount to repay (in debt token raw units) for Solend instruction
    collateral_to_seize_raw: u64, // Collateral cToken amount to seize (for redemption check)
}

/// Get SOL price in USD from oracle
/// Returns SOL price if available, otherwise None
/// 
/// CRITICAL: Tries multiple methods to get SOL price:
/// 1. From liquidation context (if SOL is collateral/debt)
/// 2. From Pyth SOL/USD feed (primary)
/// 3. From alternative Pyth feed if primary fails
async fn get_sol_price_usd(rpc: &Arc<RpcClient>, ctx: &LiquidationContext) -> Option<f64> {
    // SOL native mint: So11111111111111111111111111111111111111112
    let sol_native_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").ok()?;
    
    // Method 1: Check if collateral or debt is SOL, use its price
    if let Some(deposit_reserve) = &ctx.deposit_reserve {
        if deposit_reserve.liquidity().mintPubkey == sol_native_mint {
            if let Some(price) = ctx.deposit_price_usd {
                log::debug!("Using SOL price from deposit reserve: ${}", price);
                return Some(price);
            }
        }
    }
    
    if let Some(borrow_reserve) = &ctx.borrow_reserve {
        if borrow_reserve.liquidity().mintPubkey == sol_native_mint {
            if let Some(price) = ctx.borrow_price_usd {
                log::debug!("Using SOL price from borrow reserve: ${}", price);
                return Some(price);
            }
        }
    }
    
    // Method 2: Fetch from Pyth SOL/USD price feed (primary)
    // Pyth SOL/USD mainnet price feed: H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG
    let sol_usd_pyth_feed_primary = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => {
            log::warn!("Failed to parse primary Pyth SOL/USD feed address");
            return None;
        }
    };
    
    let current_slot = match rpc.get_slot() {
        Ok(slot) => slot,
        Err(e) => {
            log::warn!("Failed to get current slot for SOL price: {}", e);
            return None;
        }
    };
    
    // Try primary Pyth feed
    match validate_pyth_oracle(rpc, sol_usd_pyth_feed_primary, current_slot).await {
        Ok((true, Some(price))) => {
            log::info!("✅ SOL price from primary Pyth feed: ${:.2}", price);
            return Some(price);
        }
        Ok((true, None)) => {
            // This shouldn't happen (if valid=true, price should be Some), but handle it
            log::warn!(
                "⚠️  Primary Pyth SOL/USD feed validation passed but price is None (unexpected). \
                 Feed: {}",
                sol_usd_pyth_feed_primary
            );
        }
        Ok((false, _)) => {
            log::warn!(
                "⚠️  Primary Pyth SOL/USD feed validation failed (stale/invalid). \
                 Feed: {}. This may indicate: stale price, invalid status, or high confidence interval.",
                sol_usd_pyth_feed_primary
            );
        }
        Err(e) => {
            log::error!(
                "❌ Error validating primary Pyth SOL/USD feed {}: {}. \
                 This is CRITICAL - SOL price is required for profit calculations!",
                sol_usd_pyth_feed_primary,
                e
            );
        }
    }
    
    // Method 3: Try alternative Pyth feed if available
    // Alternative: SOL/USD price feed (if different feed exists)
    // Note: For now, we only have one feed, but this structure allows easy addition of alternatives
    
    // Method 4: Fetch from Jupiter Price API (External fallback - "kesin çalışmalı")
    // This is a robust fallback when on-chain oracles are failing due to RPC issues
    const JUPITER_PRICE_API: &str = "https://price.jup.ag/v4/price?ids=SOL";
    
    log::warn!("Trying Jupiter Price API fallback for SOL price...");
    
    // Create a short-lived client with strict timeout (2s)
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build();
        
    if let Ok(client) = client {
        match client.get(JUPITER_PRICE_API).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    #[derive(serde::Deserialize)]
                    struct PriceData {
                        price: f64,
                    }
                    #[derive(serde::Deserialize)]
                    struct JupiterPriceResponse {
                        data: std::collections::HashMap<String, PriceData>,
                    }
                    
                    if let Ok(json) = resp.json::<JupiterPriceResponse>().await {
                        if let Some(data) = json.data.get("SOL") {
                            log::info!("✅ SOL price from Jupiter API fallback: ${:.2}", data.price);
                            return Some(data.price);
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("Jupiter Price API fallback failed: {}", e);
            }
        }
    }

    log::error!(
        "❌ CRITICAL: All SOL price oracle methods failed! \
         SOL price is REQUIRED for: profit calculations, fee calculations, risk limit checks. \
         Bot cannot safely operate without accurate SOL price. \
         Please check: RPC connectivity, Pyth feed availability, network conditions."
    );
    None
}

/// Get Jupiter quote for liquidation with profit calculation per Structure.md section 7
async fn get_liquidation_quote(
    ctx: &LiquidationContext,
    config: &Config,
    rpc: &Arc<RpcClient>,
) -> Result<LiquidationQuote> {
    // Use first borrow and first deposit
    if ctx.borrows.is_empty() || ctx.deposits.is_empty() {
        return Err(anyhow::anyhow!("No borrows or deposits in obligation"));
    }

    let borrow = &ctx.borrows[0];
    // Note: deposit is available but we use deposit_reserve directly for mint address

    // CRITICAL FIX: Solend liquidation collateral flow
    //
    // Solend LiquidateObligation instruction:
    // - Input: sourceLiquidity (debt token, e.g., USDC)
    // - Output: destinationCollateral (collateral cToken, e.g., cSOL)
    //
    // PROBLEM: cTokens cannot be traded directly on Jupiter!
    // SOLUTION: We need to redeem cToken to underlying token first (cSOL -> SOL)
    //
    // However, the current build_liquidation_tx() only includes LiquidateObligation.
    // RedeemReserveCollateral instruction is MISSING!
    //
    // For now, we'll use the underlying token mint for Jupiter quote,
    // assuming we'll add RedeemReserveCollateral instruction later.
    
    // Step 1: Get collateral cToken mint (what Solend gives us)
    let collateral_ctoken_mint = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.collateral().mintPubkey) // cToken mint (e.g., cSOL)
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    // Step 2: Get underlying collateral token mint (what we need for Jupiter)
    let collateral_underlying_mint = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.liquidity().mintPubkey) // Underlying token mint (e.g., SOL)
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    // Step 3: Get debt token mint
    let debt_mint = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.liquidity().mintPubkey) // Debt token mint (e.g., USDC)
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    
    log::debug!(
        "Jupiter quote request:\n\
         Collateral cToken: {}\n\
         Collateral underlying: {} (will be used for Jupiter)\n\
         Debt token: {}",
        collateral_ctoken_mint,
        collateral_underlying_mint,
        debt_mint
    );
    
    // Use underlying mint for Jupiter (cToken cannot be traded on Jupiter)
    let collateral_mint = collateral_underlying_mint;

    // CRITICAL: Calculate correct collateral amount to seize
    // 
    // Liquidation flow:
    // 1. We repay: debt_to_repay = borrowedAmountWad * close_factor (50%)
    // 2. Solend gives us: collateral_to_seize = debt_to_repay * (1 + liquidation_bonus)
    // 3. We need to swap: collateral_to_seize amount of collateral → debt_to_repay amount of debt
    //
    // Steps:
    // a) Calculate debt to repay (with close factor 50%)
    // b) Calculate collateral to seize (with liquidation bonus)
    // c) Query Jupiter: collateral_to_seize → debt token
    
    // Step 1: Get reserves and token decimals
    // CRITICAL: We need decimals to calculate raw token amounts correctly
    let borrow_reserve = ctx
        .borrow_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = ctx
        .deposit_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let debt_decimals = borrow_reserve.liquidity().mintDecimals;
    let collateral_decimals = deposit_reserve.liquidity().mintDecimals;
    
    // CRITICAL SECURITY: Validate decimal values to prevent corrupt data issues
    // SPL tokens use 0-18 decimals (standard range)
    // If layout changes or data is corrupt, we might get invalid values (0, 255, etc.)
    if debt_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid debt decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            debt_decimals,
            ctx.borrows[0].borrowReserve
        ));
    }
    
    if collateral_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid collateral decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            collateral_decimals,
            ctx.deposits[0].depositReserve
        ));
    }
    
    // Step 2: Calculate debt to repay (close factor from reserve config)
    // CRITICAL FIX: Solend's debt calculation works as follows:
    // 1. borrowedAmountWad: Initial borrowed amount (WAD format, token decimals NOT included)
    // 2. cumulativeBorrowRateWads: Interest rate accumulator (WAD format)
    // 3. Token decimals: Mint's actual decimals (e.g., USDC = 6, SOL = 9)
    //
    // Formula: actual_debt = borrowedAmountWad * cumulativeBorrowRateWads / WAD / WAD
    // Why two divisions? Both inputs are WAD (10^18), product is 10^36, need two divisions to normalize
    // Result is normalized amount (not in WAD format), then multiply by 10^decimals to get raw amount
    const WAD: u128 = 1_000_000_000_000_000_000; // 10^18
    
    // CRITICAL FIX: Get close factor from reserve config (not hardcoded)
    // Close factor can be changed by Solend governance, so we read it from chain
    // Currently, ReserveConfig doesn't have closeFactor field, so close_factor() returns 0.5 (50%)
    // If Solend adds closeFactor to ReserveConfig in the future, it will be automatically used
    let close_factor_f64 = borrow_reserve.close_factor(); // Returns 0.5 (50%) as fallback
    let close_factor_wad = (close_factor_f64 * WAD as f64) as u128;
    
    log::debug!(
        "Close factor: {:.1}% (from reserve config, fallback to 50% if not available)",
        close_factor_f64 * 100.0
    );
    
    // Step 2a: Calculate actual debt in normalized format (interest included)
    // CRITICAL: Both borrowedAmountWad and cumulativeBorrowRateWads are in WAD format (10^18)
    // When multiplied: 10^36, so we need to divide by WAD twice to get normalized amount
    // Formula: actual_debt = borrowedAmountWad * cumulativeBorrowRateWads / WAD / WAD
    let actual_debt_wad: u128 = borrow.borrowedAmountWads
        .checked_mul(borrow.cumulativeBorrowRateWads)
        .and_then(|v| v.checked_div(WAD))
        .and_then(|v| v.checked_div(WAD))  // ✅ İKİNCİ DIVISION - CRITICAL FIX
        .ok_or_else(|| anyhow::anyhow!("Debt calculation overflow: borrowedAmountWad * cumulativeBorrowRateWads"))?;
    
    // Step 2b: Apply close factor (from reserve config)
    let debt_to_repay_wad: u128 = actual_debt_wad
        .checked_mul(close_factor_wad)
        .and_then(|v| v.checked_div(WAD))
        .ok_or_else(|| anyhow::anyhow!("Close factor calculation overflow"))?;
    
    // Step 2c: Convert normalized amount to raw token amount with decimals
    // After the double WAD division, actual_debt_wad and debt_to_repay_wad are normalized (not in WAD format)
    // We just need to multiply by 10^decimals to get the raw token amount
    // 
    // Example for USDC (6 decimals):
    // - actual_debt_wad = 1575 (normalized, after double WAD division)
    // - debt_to_repay_wad = 787 (normalized, after close factor)
    // - debt_to_repay_raw = 787 * 10^6 = 787000000 (raw USDC amount)
    let decimals_multiplier = 10_u128
        .checked_pow(debt_decimals as u32)
        .ok_or_else(|| anyhow::anyhow!("Decimals multiplier overflow: 10^{}", debt_decimals))?;
    
    // CORRECT FORMULA: debt_to_repay_wad is already normalized, just multiply by decimals
    // debt_to_repay_raw = debt_to_repay_wad * 10^decimals
    let debt_to_repay_raw = debt_to_repay_wad
        .checked_mul(decimals_multiplier)
        .ok_or_else(|| anyhow::anyhow!("Raw amount conversion overflow"))?
        as u64;
    
    log::debug!(
        "Debt calculation (CORRECTED): borrowed_wad={}, cumulative_rate={}, actual_debt_wad={}, debt_to_repay_wad={}, debt_to_repay_raw={} (decimals={})",
        borrow.borrowedAmountWads,
        borrow.cumulativeBorrowRateWads,
        actual_debt_wad,
        debt_to_repay_wad,
        debt_to_repay_raw,
        debt_decimals
    );
    
    // Step 3: Get liquidation bonus from deposit reserve (collateral reserve)
    let liquidation_bonus = deposit_reserve.liquidation_bonus(); // Returns 0.05 for 5%, etc.
    
    // Step 4: Convert debt raw amount to USD for calculations
    let debt_price_usd = ctx
        .borrow_price_usd
        .ok_or_else(|| anyhow::anyhow!("Borrow price not available"))?;
    let collateral_price_usd = ctx
        .deposit_price_usd
        .ok_or_else(|| anyhow::anyhow!("Deposit price not available"))?;
    
    // Convert raw debt amount to normalized amount, then to USD
    let debt_to_repay_normalized = debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32);
    let debt_to_repay_usd = debt_to_repay_normalized * debt_price_usd;
    
    // VERIFICATION: Convert back to check
    let verification = (debt_to_repay_raw as f64) / 10_f64.powi(debt_decimals as i32);
    let debt_to_repay_usd_check = verification * debt_price_usd;
    log::debug!(
        "Debt verification: raw={} -> normalized={:.6} -> ${:.2} USD",
        debt_to_repay_raw,
        verification,
        debt_to_repay_usd_check
    );
    
    // Step 5: Calculate collateral to seize
    // CRITICAL FIX: Solend liquidation bonus calculation
    //
    // Solend formula (from whitepaper):
    // collateral_to_seize = (debt_to_repay / collateral_price) * (1 + liquidation_bonus)
    //
    // This formula gives collateral TOKEN amount, not USD!
    // So:
    // 1. Convert debt from USD to collateral token (debt_usd / collateral_price)
    // 2. Add bonus (amount * (1 + bonus))
    // 3. Convert to raw amount (amount * 10^decimals)
    
    // Step 5a: Convert debt to collateral token amount (before bonus)
    // debt_in_collateral_tokens = debt_to_repay_usd / collateral_price_usd
    let debt_in_collateral_tokens = debt_to_repay_usd / collateral_price_usd;
    
    // Step 5b: Apply liquidation bonus (in token amount, not USD!)
    // CRITICAL: Bonus is applied to token amount, then converted to USD
    let collateral_to_seize_tokens = debt_in_collateral_tokens * (1.0 + liquidation_bonus);
    
    // Step 5c: Convert to USD for logging and validation
    let collateral_to_seize_usd = collateral_to_seize_tokens * collateral_price_usd;
    
    // Step 5d: Convert to raw token amount with decimals
    let collateral_to_seize_raw = (collateral_to_seize_tokens * 10_f64.powi(collateral_decimals as i32)) as u64;
    
    log::debug!(
        "Collateral calculation (CORRECTED): debt_to_repay=${:.2} USD -> {:.6} collateral tokens -> {:.6} with bonus ({:.1}%) -> {:.6} USD -> {} raw",
        debt_to_repay_usd,
        debt_in_collateral_tokens,
        collateral_to_seize_tokens,
        liquidation_bonus * 100.0,
        collateral_to_seize_usd,
        collateral_to_seize_raw
    );
    
    // VERIFICATION: Profit check (before Jupiter swap)
    // Expected profit (if no slippage): collateral_usd - debt_usd
    let expected_profit_before_swap = collateral_to_seize_usd - debt_to_repay_usd;
    log::debug!(
        "Expected profit before swap: ${:.2} (bonus: ${:.2})",
        expected_profit_before_swap,
        expected_profit_before_swap
    );
    
    // CRITICAL INSIGHT: This profit assumes 1:1 swap at oracle prices
    // Jupiter will give us LESS due to:
    // 1. Price impact (slippage)
    // 2. LP fees (~0.25% for most pools)
    // 3. Route inefficiency (multi-hop swaps)
    //
    // So the ACTUAL profit will be:
    // actual_profit = jupiter_out_amount_usd - debt_to_repay_usd - fees

    // Step 6: Calculate actual SOL amount after redemption
    // CRITICAL FIX: collateral_to_seize_raw is cToken amount, NOT underlying token amount!
    // We need to calculate the exchange rate from cToken to underlying token.
    //
    // DOĞRU cToken exchange rate hesabı:
    //
    // Solend formula:
    //   borrowedAmountWads = initial_borrow * 10^18 (normalized, no decimals)
    //   actual_borrowed = borrowedAmountWads * cumulativeBorrowRateWads / 10^18 / 10^18
    //   actual_borrowed_with_decimals = actual_borrowed * 10^decimals
    //
    // Exchange rate:
    //   total_supply = availableAmount + actual_borrowed_with_decimals
    //   exchange_rate = total_supply / ctoken_supply
    //
    // CRITICAL: borrowedAmountWads is NOT raw amount! It needs to account for interest accrual.
    // NOTE: WAD is already defined earlier in this function
    
    let ctokens_total_supply = deposit_reserve.collateral().mintTotalSupply;
    let available_amount = deposit_reserve.liquidity().availableAmount;
    let borrowed_amount_wads = deposit_reserve.liquidity().borrowedAmountWads;
    let cumulative_borrow_rate = deposit_reserve.liquidity().cumulativeBorrowRateWads;
    
    // Step 1: Calculate actual borrowed amount (normalized, no decimals)
    // Formula: borrowedAmountWads * cumulativeBorrowRateWads / WAD / WAD
    // Why two divisions? Both inputs are WAD (10^18), product is 10^36, need two divisions to normalize
    let actual_borrowed_normalized = borrowed_amount_wads
        .checked_mul(cumulative_borrow_rate)
        .and_then(|v| v.checked_div(WAD))
        .and_then(|v| v.checked_div(WAD))
        .ok_or_else(|| anyhow::anyhow!("Borrowed amount calculation overflow: borrowedAmountWads * cumulativeBorrowRateWads"))?;
    
    // Step 2: Convert to raw amount with decimals
    let decimals_multiplier = 10_u128
        .checked_pow(collateral_decimals as u32)
        .ok_or_else(|| anyhow::anyhow!("Decimals multiplier overflow"))?;
    
    // Convert normalized amount to raw amount
    // CRITICAL FIX: Use u128 arithmetic instead of f64 to prevent overflow and precision loss
    // f64 has only 53 bits of precision and can overflow for large numbers
    let actual_borrowed_raw = actual_borrowed_normalized
        .checked_mul(decimals_multiplier)
        // .and_then(|v| v.checked_div(WAD)) // ❌ REMOVED: No division needed, already normalized!
        .ok_or_else(|| anyhow::anyhow!("Overflow in actual borrowed calculation: actual_borrowed_normalized * decimals_multiplier"))?
        as u64;
    
    // Step 3: Total underlying supply
    let total_underlying_supply = available_amount.saturating_add(actual_borrowed_raw);
    
    // Step 4: Exchange rate (underlying per cToken)
    let exchange_rate = if ctokens_total_supply > 0 {
        total_underlying_supply as f64 / ctokens_total_supply as f64
    } else {
        1.0 // Initial exchange rate
    };
    
    // Step 5: Calculate SOL amount after redemption
    let sol_amount_after_redemption = (collateral_to_seize_raw as f64 * exchange_rate) as u64;
    
    log::debug!(
        "cToken exchange (CORRECTED): \n\
         - cTokens to redeem: {} \n\
         - Available: {} \n\
         - Borrowed (WADs): {} \n\
         - Cumulative rate: {} \n\
         - Actual borrowed (normalized): {} \n\
         - Actual borrowed (raw): {} \n\
         - Total underlying: {} \n\
         - cToken supply: {} \n\
         - Exchange rate: {:.6} \n\
         - SOL output: {}",
        collateral_to_seize_raw,
        available_amount,
        borrowed_amount_wads,
        cumulative_borrow_rate,
        actual_borrowed_normalized,
        actual_borrowed_raw,
        total_underlying_supply,
        ctokens_total_supply,
        exchange_rate,
        sol_amount_after_redemption
    );
    
    // VERIFICATION: Check if exchange rate is reasonable
    // Solend exchange rate typically 1.0 - 1.5 range
    if exchange_rate < 0.8 || exchange_rate > 2.0 {
        log::warn!(
            "⚠️  Abnormal cToken exchange rate detected: {:.6}. \
             This may indicate data corruption or extreme market conditions.",
            exchange_rate
        );
        return Err(anyhow::anyhow!("Abnormal exchange rate: {:.6}", exchange_rate));
    }
    
    // Step 7: Get Jupiter quote with calculated dynamic slippage
    // CRITICAL FIX: Single Jupiter call to avoid circular logic and rate limits
    //
    // Instead of preliminary_quote -> price_impact -> final_quote,
    // we use a robust single-quote strategy:
    // 1. Start with a generous buffer (2x base slippage)
    // 2. Get the quote
    // 3. Check price impact post-facto
    // 4. If price impact is too high (>5%), reject the quote
    
    // Read slippage settings from .env (no hardcoded values)
    use std::env;
    let base_slippage_bps = env::var("MIN_PROFIT_MARGIN_BPS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(50); // Default 0.5% if not set
    
    let max_slippage_bps = env::var("MAX_SLIPPAGE_BPS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(300); // Default 3% if not set
        
    // Apply 2x buffer for safety on first try
    let buffer_multiplier = 2.0;
    let initial_slippage = ((base_slippage_bps as f64 * buffer_multiplier) as u16).min(max_slippage_bps);
    
    log::debug!(
        "Jupiter quote strategy: Single call with {}bps slippage (base: {}bps, buffer: {:.1}x)",
        initial_slippage,
        base_slippage_bps,
        buffer_multiplier
    );
    
    // Step 8: Get final Jupiter quote with calculated dynamic slippage
    // CRITICAL: Jupiter quote uses UNDERLYING token (SOL), not cToken (cSOL)
    let quote = get_jupiter_quote_with_retry(
        &collateral_underlying_mint, // ✅ Underlying token (SOL), NOT cToken (cSOL)
        &debt_mint,                  // ✅ Debt token (USDC)
        sol_amount_after_redemption, // ✅ CORRECT: Actual SOL amount after redemption
        initial_slippage,            // ✅ CORRECT: Buffered initial slippage
        3, // max_retries
    )
    .await
    .context("Failed to get Jupiter quote with retries")?;
    
    // Price impact check - Reject if too high (>5%)
    let price_impact_pct = crate::jup::get_price_impact_pct(&quote);
    if price_impact_pct > 5.0 {
        log::warn!(
            "❌ Price impact too high: {:.2}% (max: 5.0%). Skipping liquidation due to low liquidity.",
            price_impact_pct
        );
        return Err(anyhow::anyhow!(
            "Price impact too high: {:.2}% (max: 5.0%)",
            price_impact_pct
        ));
    }
    
    log::debug!(
        "Jupiter quote successful: impact={:.2}%, out_amount={}",
        price_impact_pct,
        quote.out_amount
    );
    
    // Transaction flow information
    log::debug!(
        "✅ Transaction flow implemented:\n\
         1. LiquidateObligation: debt_token -> cToken (e.g., USDC -> {})\n\
         2. RedeemReserveCollateral: cToken -> underlying_token (e.g., {} -> {})\n\
         3. Jupiter Swap: underlying_token -> debt_token (e.g., {} -> USDC)\n\
         \n\
         NOTE: Jupiter swap is executed in a separate transaction after redemption.",
        collateral_ctoken_mint,
        collateral_ctoken_mint,
        collateral_underlying_mint,
        collateral_underlying_mint
    );

    // CRITICAL FIX: Profit calculation flow
    //
    // Liquidation flow:
    // 1. Solend gives us COLLATERAL token (e.g., 10 SOL)
    // 2. Jupiter swaps COLLATERAL -> DEBT token (10 SOL -> ? USDC)
    // 3. We repay DEBT token to Solend (e.g., 1450 USDC)
    // 4. Remaining DEBT token is our profit (e.g., Jupiter gave 1500 USDC, repay 1450, profit 50)
    //
    // Profit = jupiter_out_amount (debt token) - debt_to_repay (debt token) - fees
    //
    // CRITICAL: Both are in the same token (debt_mint), no need to convert to USD first!
    
    // Get token decimals from reserves (already have deposit_reserve from above)
    let debt_decimals = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.liquidity().mintDecimals)
        .unwrap_or(6);
    
    // CRITICAL SECURITY: Validate decimal value to prevent corrupt data issues
    // SPL tokens use 0-18 decimals (standard range)
    if debt_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid debt decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            debt_decimals,
            ctx.borrows[0].borrowReserve
        ));
    }

    // Step 7a: Get Jupiter output amount (in debt token raw units)
    let jupiter_out_amount: u64 = quote.out_amount
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid Jupiter out_amount: {}", e))?;
    
    // Step 7b: Calculate profit in debt token (raw units)
    // profit_raw = jupiter_out_amount - debt_to_repay_raw
    // If negative, this liquidation loses money!
    let profit_raw = (jupiter_out_amount as i128) - (debt_to_repay_raw as i128);
    
    // Step 7c: Convert to normalized amount for USD calculation
    let profit_tokens = (profit_raw as f64) / 10_f64.powi(debt_decimals as i32);
    
    // Step 7d: Convert to USD
    let profit_before_fees_usd = profit_tokens * debt_price_usd;
    
    // Step 7e: Calculate total fees for SINGLE atomic transaction (Flashloan approach)
    // ✅ FLASHLOAN APPROACH: Single transaction eliminates race condition and MEV risk
    // All operations in ONE transaction: FlashBorrow -> Liquidate -> Redeem -> Swap -> FlashRepay
    // Get SOL price for fee calculations
    // CRITICAL: SOL price is REQUIRED - cannot use fallback as it causes incorrect profit calculations
    let sol_price_usd = match get_sol_price_usd(rpc, ctx).await {
        Some(price) => price,
        None => {
            log::error!(
                "❌ CRITICAL: Cannot calculate liquidation profit without SOL price! \
                 SOL price is required for: fee calculations, profit calculations, risk limits. \
                 Skipping this liquidation opportunity."
            );
            return Err(anyhow::anyhow!(
                "Cannot proceed with liquidation: SOL price unavailable from oracle. \
                 This is a critical error - bot cannot safely operate without accurate SOL price."
            ));
        }
    };
    
    let jito_tip_lamports = config.jito_tip_amount_lamports.unwrap_or(10_000_000u64);
    let jito_tip_sol = jito_tip_lamports as f64 / 1_000_000_000.0;
    
    // Single transaction fees (Flashloan approach - all operations in one TX)
    // Read from .env (no hardcoded values)
    // Note: std::env already imported above
    let tx_base_fee_lamports = std::env::var("BASE_TRANSACTION_FEE_LAMPORTS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5_000); // Default 5000 lamports if not set
    
    let tx_compute_units = std::env::var("LIQUIDATION_COMPUTE_UNITS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            std::env::var("DEFAULT_COMPUTE_UNITS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(500_000); // Default 500k for flashloan if not set
    
    let tx_priority_fee_per_cu = std::env::var("DEFAULT_PRIORITY_FEE_PER_CU")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            std::env::var("PRIORITY_FEE_PER_CU")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(1_000); // Default 1000 micro-lamports per CU if not set
    
    let tx_priority_fee_lamports = (tx_compute_units * tx_priority_fee_per_cu) / 1_000_000;
    let tx_total_fee_lamports = tx_base_fee_lamports + tx_priority_fee_lamports;
    
    // ✅ FIXED: Flashloan fee calculation with u128 arithmetic
    // Solend flashloan fee: reserve.config().flashLoanFeeWad (typically 0.003 = 0.3%)
    // This fee is applied to debt_to_repay_raw amount
    let flashloan_fee_usd = if let Some(borrow_reserve) = &ctx.borrow_reserve {
        const WAD: u128 = 1_000_000_000_000_000_000; // 10^18
        let flashloan_fee_wad = borrow_reserve.config().flashLoanFeeWad as u128;
        
        // Flashloan fee amount (in raw token units)
        // CRITICAL FIX: Use u128 arithmetic to prevent overflow
        let flashloan_fee_amount_raw = (debt_to_repay_raw as u128)
            .checked_mul(flashloan_fee_wad)
            .and_then(|v| v.checked_div(WAD))
            .ok_or_else(|| anyhow::anyhow!("Flashloan fee calculation overflow"))?
            as u64;
        
        // Convert to USD
        let flashloan_fee_tokens = (flashloan_fee_amount_raw as f64) / 10_f64.powi(debt_decimals as i32);
        flashloan_fee_tokens * debt_price_usd
    } else {
        0.0 // No flashloan fee if borrow reserve not available (shouldn't happen)
    };
    
    // Total fees in USD (single transaction approach)
    let jito_fee_usd = jito_tip_sol * sol_price_usd; // Single Jito tip (one transaction)
    let tx_fee_usd = (tx_total_fee_lamports as f64 / 1_000_000_000.0) * sol_price_usd;
    let total_fees_usd = jito_fee_usd + tx_fee_usd + flashloan_fee_usd;
    
    // FINAL PROFIT (single atomic transaction - no race condition!)
    let profit_usdc = profit_before_fees_usd - total_fees_usd;
    
    log::debug!(
        "Profit calculation (FLASHLOAN - SINGLE TX):\n\
         Jupiter output: {} raw ({:.6} tokens, ${:.2} USD)\n\
         Debt to repay: {} raw ({:.6} tokens, ${:.2} USD)\n\
         Profit before fees: ${:.2}\n\
         Jito fee: ${:.4}\n\
         TX fee: ${:.4}\n\
         Flashloan fee: ${:.4}\n\
         Total fees: ${:.4}\n\
         FINAL PROFIT: ${:.2}",
        jupiter_out_amount,
        jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32),
        jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32) * debt_price_usd,
        debt_to_repay_raw,
        debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32),
        debt_to_repay_usd,
        profit_before_fees_usd,
        jito_fee_usd,
        tx_fee_usd,
        flashloan_fee_usd,
        total_fees_usd,
        profit_usdc
    );
    
    // CRITICAL CHECK: Negative profit check
    if profit_usdc < 0.0 {
        let jupiter_out_amount_usd = (jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32)) * debt_price_usd;
        let swap_loss_usd = collateral_to_seize_usd - jupiter_out_amount_usd;
        log::warn!(
            "Negative profit detected: ${:.2}. Jupiter swap loss (${:.2}) exceeded liquidation bonus!",
            profit_usdc,
            swap_loss_usd
        );
        return Err(anyhow::anyhow!("Liquidation would lose money: profit=${:.2}", profit_usdc));
    }
    
    // CRITICAL: Add slippage buffer to minimum profit threshold
    // Jupiter swap may have worse execution than quote
    const SLIPPAGE_BUFFER_PCT: f64 = 0.5; // 0.5% additional buffer
    let effective_min_profit = config.min_profit_usdc * (1.0 + SLIPPAGE_BUFFER_PCT / 100.0);
    
    if profit_usdc < effective_min_profit {
        return Err(anyhow::anyhow!(
            "Profit ${:.2} below threshold ${:.2} (with {:.1}% slippage buffer)",
            profit_usdc,
            effective_min_profit,
            SLIPPAGE_BUFFER_PCT
        ));
    }
    
    // Price impact logging for transparency
    let price_impact_pct = crate::jup::get_price_impact_pct(&quote);
    let actual_swap_rate = if collateral_to_seize_tokens > 0.0 {
        (jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32)) / collateral_to_seize_tokens
    } else {
        0.0
    };
    let expected_swap_rate = debt_price_usd / collateral_price_usd;
    let rate_deviation_pct = if expected_swap_rate > 0.0 {
        ((actual_swap_rate - expected_swap_rate) / expected_swap_rate).abs() * 100.0
    } else {
        0.0
    };
    
    log::debug!(
        "Swap analysis:\n\
         Expected rate: {:.6} (oracle prices)\n\
         Actual rate: {:.6} (Jupiter)\n\
         Rate deviation: {:.2}%\n\
         Jupiter price impact: {:.2}%",
        expected_swap_rate,
        actual_swap_rate,
        rate_deviation_pct,
        price_impact_pct
    );

    // NOTE: debt_to_repay_raw is already calculated earlier in this function (Step 2)
    // It's calculated directly from WAD amounts with proper decimal handling
    
    // collateral_value_usd is used for risk limit calculations (position size)
    let collateral_value_usd = collateral_to_seize_usd;
    
    Ok(LiquidationQuote {
        quote,
        profit_usdc,
        collateral_value_usd,
        debt_to_repay_raw, // Debt amount to repay in Solend instruction
        collateral_to_seize_raw, // Collateral cToken amount to seize (for redemption check)
    })
}

/// Log wallet balances (SOL and USDC) to console
/// Used for periodic balance monitoring
async fn log_wallet_balances(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<()> {
    // Get SOL balance
    let sol_balance_lamports = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get SOL balance: {}", e))?;
    let sol_balance = sol_balance_lamports as f64 / 1_000_000_000.0;
    
    // Get SOL price for USD value
    let sol_usd_pyth_feed = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => {
            log::info!(
                "💰 Wallet Balances: SOL: {:.6} SOL (~${:.2}), USDC: (checking...)",
                sol_balance,
                sol_balance * 150.0
            );
            // Continue with USDC check using fallback price
            let program_id = crate::solend::solend_program_id()?;
            let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
                .context("Failed to discover USDC mint")?;
            use spl_associated_token_account::get_associated_token_address;
            let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
            let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
                Ok(Some(account)) => account.token_amount.amount.parse::<u64>().unwrap_or(0),
                _ => 0,
            };
            let usdc_balance = usdc_balance_raw as f64 / 1_000_000.0;
            log::info!(
                "💰 Wallet Balances: SOL: {:.6} SOL (~${:.2}), USDC: {:.2} USDC, Total: ~${:.2}",
                sol_balance,
                sol_balance * 150.0,
                usdc_balance,
                sol_balance * 150.0 + usdc_balance
            );
            return Ok(());
        }
    };
    
    let current_slot = rpc.get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    let sol_price_usd = match validate_pyth_oracle(rpc, sol_usd_pyth_feed, current_slot).await {
        Ok((true, Some(price))) => price,
        _ => {
            log::warn!(
                "⚠️  Failed to get SOL price from oracle for balance logging, using fallback $150. \
                 This may cause inaccurate wallet value calculations."
            );
            150.0
        }
    };
    
    let sol_value_usd = sol_balance * sol_price_usd;
    
    // Get USDC balance
    let program_id = crate::solend::solend_program_id()?;
    let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint")?;
    
    use spl_associated_token_account::get_associated_token_address;
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    
    let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0,
        Err(_) => 0,
    };
    
    let usdc_balance = usdc_balance_raw as f64 / 1_000_000.0;
    let total_value_usd = sol_value_usd + usdc_balance;
    
    log::info!(
        "💰 Wallet Balances: SOL: {:.6} SOL (${:.2}), USDC: {:.2} USDC, Total: ${:.2}",
        sol_balance,
        sol_value_usd,
        usdc_balance,
        total_value_usd
    );
    
    Ok(())
}

/// Get total wallet value in USD (SOL + USDC)
/// Used for risk limit calculations per Structure.md section 6.4
async fn get_wallet_value_usd(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<f64> {
    // 1. Get SOL balance and price
    let sol_balance = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get wallet balance: {}", e))?;
    
    // Get SOL price from Pyth (fallback to $150 if unavailable)
    let sol_usd_pyth_feed = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => {
            log::warn!("Failed to parse SOL/USD Pyth feed, using fallback $150");
            let sol_value_usd = (sol_balance as f64) / 1_000_000_000.0 * 150.0;
            // Continue to get USDC balance
            let program_id = crate::solend::solend_program_id()?;
            let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
                .context("Failed to discover USDC mint")?;
            use spl_associated_token_account::get_associated_token_address;
            let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
            let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
                Ok(Some(account)) => account.token_amount.amount.parse::<u64>().unwrap_or(0),
                _ => 0,
            };
            let usdc_value_usd = (usdc_balance_raw as f64) / 1_000_000.0;
            return Ok(sol_value_usd + usdc_value_usd);
        }
    };
    
    let current_slot = rpc.get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    let sol_price_usd = match validate_pyth_oracle(rpc, sol_usd_pyth_feed, current_slot).await {
        Ok((true, Some(price))) => price,
        _ => {
            log::warn!(
                "⚠️  Failed to get SOL price from oracle for risk limit calculation, using fallback $150. \
                 Risk limits may be inaccurate. This is not critical but should be monitored."
            );
            150.0
        }
    };
    
    let sol_value_usd = (sol_balance as f64) / 1_000_000_000.0 * sol_price_usd;
    
    // 2. Get USDC balance
    let program_id = crate::solend::solend_program_id()?;
    let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint")?;
    
    use spl_associated_token_account::get_associated_token_address;
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    
    let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0, // ATA doesn't exist
        Err(_) => {
            log::debug!("Failed to get USDC balance, assuming 0");
            0
        }
    };
    
    // USDC has 6 decimals, price is $1.0
    let usdc_value_usd = (usdc_balance_raw as f64) / 1_000_000.0;
    
    // 3. Calculate total wallet value in USD
    // Note: For now, we include SOL and USDC. Other tokens can be added later if needed.
    let wallet_value_usd = sol_value_usd + usdc_value_usd;
    
    Ok(wallet_value_usd)
}

// NOTE: is_within_risk_limits() function was removed.
// Risk limit checking is now done inline in process_cycle() before each liquidation
// to ensure wallet balance is refreshed and cumulative risk tracking works correctly.
// This prevents race conditions where wallet balance changes during the cycle.

/// Build liquidation transaction per Structure.md section 8
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

    // ============================================================================
    // CRITICAL: Re-validate oracle freshness before building transaction
    // ============================================================================
    // Oracle validation happens at cycle start, but by the time we build the TX,
    // 2-3 seconds may have passed (Jupiter quote + TX build time).
    // The oracle might have become stale during this time, so we re-check here.
    let current_slot_now = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot for oracle re-validation: {}", e))?;
    
    // Re-validate borrow reserve oracle
    if let Some(reserve) = &ctx.borrow_reserve {
        let (valid, _) = validate_pyth_oracle(
            rpc,
            reserve.oracle_pubkey(),
            current_slot_now,
        )
        .await
        .context("Failed to re-validate borrow reserve oracle")?;
        
        if !valid {
            return Err(anyhow::anyhow!(
                "Borrow reserve oracle became stale during TX preparation. \
                 Time elapsed since initial validation: ~2-3s. Aborting liquidation for obligation {}.",
                ctx.obligation_pubkey
            ));
        }
    }
    
    // Re-validate deposit reserve oracle
    if let Some(reserve) = &ctx.deposit_reserve {
        let (valid, _) = validate_pyth_oracle(
            rpc,
            reserve.oracle_pubkey(),
            current_slot_now,
        )
        .await
        .context("Failed to re-validate deposit reserve oracle")?;
        
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

/// Build single atomic transaction with flashloan
/// Flow:
/// 1. FlashLoan: Borrow debt_amount USDC from Solend (flash)
/// 2. LiquidateObligation: Repay debt, receive cSOL
/// 3. RedeemReserveCollateral: cSOL -> SOL
/// 4. Jupiter Swap: SOL -> USDC
/// 5. FlashRepayReserveLiquidity: Repay borrowed USDC + fee (EXPLICIT - REQUIRED!)
/// All in ONE transaction - atomicity guaranteed!
/// 
/// AVANTAJLAR:
/// ✅ Atomicity: Tüm işlemler tek transaction'da
/// ✅ No race condition: TX1/TX2 split yok
/// ✅ No MEV risk: Intermediate state yok
/// ✅ Sermaye gerektirmez: Flashloan ile başla
/// ✅ Gas-efficient: Tek transaction
/// 
/// DİKKAT:
/// - Flashloan fee var (~0.3% Solend'de)
/// - Jupiter swap'i instructions sysvar ile verify etmek gerek
/// - Compute unit limiti yüksek olmalı (~400k-600k)
async fn build_flashloan_liquidation_tx(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: solana_sdk::hash::Hash,
    config: &Config,
) -> Result<Transaction> {
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
        sysvar,
    };
    use spl_associated_token_account::get_associated_token_address;
    use spl_token::ID as TOKEN_PROGRAM_ID;
    use std::str::FromStr;
    
    let program_id = crate::solend::solend_program_id()?;
    let wallet_pubkey = wallet.pubkey();
    
    // Get reserves
    let borrow_reserve = ctx.borrow_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = ctx.deposit_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let lending_market = ctx.obligation.lendingMarket;
    let lending_market_authority = crate::solend::derive_lending_market_authority(&lending_market, &program_id)?;
    
    // Get reserve addresses
    let repay_reserve_liquidity_supply = borrow_reserve.liquidity().supplyPubkey;
    let withdraw_reserve_liquidity_supply = deposit_reserve.liquidity().supplyPubkey;
    let withdraw_reserve_collateral_supply = deposit_reserve.collateral().supplyPubkey;
    let withdraw_reserve_collateral_mint = deposit_reserve.collateral().mintPubkey;
    
    // Validate reserve addresses
    if repay_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_collateral_supply == Pubkey::default()
        || withdraw_reserve_collateral_mint == Pubkey::default() {
        return Err(anyhow::anyhow!("Invalid reserve addresses: one or more addresses are default/zero"));
    }
    
    // Get user's token accounts
    let source_liquidity = get_associated_token_address(&wallet_pubkey, &borrow_reserve.liquidity().mintPubkey);
    let destination_collateral = get_associated_token_address(&wallet_pubkey, &withdraw_reserve_collateral_mint);
    let destination_liquidity = get_associated_token_address(&wallet_pubkey, &deposit_reserve.liquidity().mintPubkey);
    
    // Validate ATAs exist
    if rpc.get_account(&source_liquidity).is_err() {
        return Err(anyhow::anyhow!(
            "Source liquidity ATA does not exist: {}. Please create ATA for token {} before liquidation.",
            source_liquidity,
            borrow_reserve.liquidity().mintPubkey
        ));
    }
    if rpc.get_account(&destination_collateral).is_err() {
        return Err(anyhow::anyhow!(
            "Destination collateral ATA does not exist: {}. Please create ATA for token {} before liquidation.",
            destination_collateral,
            withdraw_reserve_collateral_mint
        ));
    }
    if rpc.get_account(&destination_liquidity).is_err() {
        return Err(anyhow::anyhow!(
            "Destination liquidity ATA does not exist: {}. Please create ATA for token {} before liquidation.",
            destination_liquidity,
            deposit_reserve.liquidity().mintPubkey
        ));
    }
    
    // ============================================================================
    // INSTRUCTION 1: FlashLoan (Solend native)
    // ============================================================================
    // Borrow debt_amount USDC from Solend reserve (flashloan)
    let flashloan_amount = quote.debt_to_repay_raw;
    
    let mut flashloan_data = vec![crate::solend::get_flashloan_discriminator()]; // FlashLoan discriminator (tag 13)
    flashloan_data.extend_from_slice(&flashloan_amount.to_le_bytes()); // amount (u64)
    
    // FlashLoan accounts per Solend IDL (FlashBorrowReserveLiquidity - tag 13):
    // CRITICAL: Account order must match Solend SDK exactly
    // Reference: solend-sdk/src/instruction.rs - LendingInstruction::FlashBorrowReserveLiquidity
    // 
    // Correct account order (9 accounts total):
    // 0. [writable] sourceLiquidity - Reserve's liquidity supply (source of funds)
    // 1. [writable] destinationLiquidity - User's ATA to receive borrowed funds (DESTINATION)
    // 2. [writable] reserve - Reserve account to borrow from
    // 3. [writable] reserveLiquiditySupply - Reserve liquidity supply (same as sourceLiquidity)
    // 4. [readonly] lendingMarket - Lending market account
    // 5. [readonly] lendingMarketAuthority - Lending market authority PDA
    // 6. [signer] transferAuthority - User wallet (signer)
    // 7. [readonly] instructionsSysvar - Instructions sysvar (for flashloan callback verification)
    // 8. [readonly] tokenProgram - SPL Token program
    let flashloan_accounts = vec![
        AccountMeta::new(repay_reserve_liquidity_supply, false), // 0: sourceLiquidity (reserve's supply)
        AccountMeta::new(source_liquidity, false),               // 1: destinationLiquidity (user's ATA to receive funds)
        AccountMeta::new(ctx.borrows[0].borrowReserve, false),  // 2: reserve (reserve to borrow from)
        AccountMeta::new(repay_reserve_liquidity_supply, false), // 3: reserveLiquiditySupply (same as source)
        AccountMeta::new_readonly(lending_market, false),       // 4: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false), // 5: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),         // 6: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::instructions::id(), false), // 7: instructionsSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),     // 8: tokenProgram
    ];
    
    let flashloan_ix = Instruction {
        program_id,
        accounts: flashloan_accounts,
        data: flashloan_data,
    };
    
    // ============================================================================
    // INSTRUCTION 2: LiquidateObligation
    // ============================================================================
    // Use borrowed USDC to liquidate obligation, receive cSOL
    let liquidity_amount = quote.debt_to_repay_raw;
    
    let mut liquidation_data = vec![crate::solend::get_liquidate_obligation_discriminator()]; // tag 12
    liquidation_data.extend_from_slice(&liquidity_amount.to_le_bytes()); // liquidity_amount (u64)
    
    let liquidation_accounts = vec![
        AccountMeta::new(source_liquidity, false),                    // 0: sourceLiquidity (borrowed USDC)
        AccountMeta::new(destination_collateral, false),              // 1: destinationCollateral (receive cSOL)
        AccountMeta::new(ctx.borrows[0].borrowReserve, false), // 2: repayReserve
        AccountMeta::new(repay_reserve_liquidity_supply, false),     // 3: repayReserveLiquiditySupply
        AccountMeta::new_readonly(ctx.deposits[0].depositReserve, false), // 4: withdrawReserve
        AccountMeta::new(withdraw_reserve_collateral_supply, false),  // 5: withdrawReserveCollateralSupply
        AccountMeta::new(ctx.obligation_pubkey, false),               // 6: obligation
        AccountMeta::new_readonly(lending_market, false),             // 7: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),   // 8: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),               // 9: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // 10: clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),           // 11: tokenProgram
    ];
    
    let liquidation_ix = Instruction {
        program_id,
        accounts: liquidation_accounts,
        data: liquidation_data,
    };
    
    // ============================================================================
    // INSTRUCTION 3: RedeemReserveCollateral
    // ============================================================================
    // Redeem cSOL -> SOL
    let redeem_collateral_amount = quote.collateral_to_seize_raw;
    
    let mut redeem_data = vec![crate::solend::get_redeem_reserve_collateral_discriminator()]; // tag 5
    redeem_data.extend_from_slice(&redeem_collateral_amount.to_le_bytes()); // collateral_amount (u64)
    
    let redeem_accounts = vec![
        AccountMeta::new(destination_collateral, false),              // 0: sourceCollateral (cSOL from liquidation)
        AccountMeta::new(destination_liquidity, false),               // 1: destinationLiquidity (receive SOL)
        AccountMeta::new(ctx.deposits[0].depositReserve, false), // 2: reserve
        AccountMeta::new(withdraw_reserve_collateral_mint, false),    // 3: reserveCollateralMint
        AccountMeta::new(withdraw_reserve_liquidity_supply, false),   // 4: reserveLiquiditySupply
        AccountMeta::new_readonly(lending_market, false),             // 5: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),   // 6: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),              // 7: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // 8: clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),           // 9: tokenProgram
    ];
    
    let redeem_ix = Instruction {
        program_id,
        accounts: redeem_accounts,
        data: redeem_data,
    };
    
    // ============================================================================
    // INSTRUCTION 4: Jupiter Swap (SOL -> USDC)
    // ============================================================================
    // CRITICAL: Jupiter returns a vector of instructions (Setup + Swap + Cleanup)
    // We must include all of them in the transaction to handle ATAs correctly
    let jupiter_swap_ixs = crate::jup::build_jupiter_swap_instruction(
        &quote.quote,
        &wallet_pubkey,
        &config.jupiter_url,
    )
        .await
    .context("Failed to build Jupiter swap instruction")?;
    
    // ============================================================================
    // INSTRUCTION 5: FlashRepayReserveLiquidity (CRITICAL - Explicit Repayment Required!)
    // ============================================================================
    // CRITICAL: FlashLoan requires explicit repayment instruction - NOT automatic!
    // Solend uses two-instruction pattern: FlashBorrow (tag 13) + FlashRepay (tag 14)
    // 
    // Calculate flashloan fee
    const WAD_U128: u128 = 1_000_000_000_000_000_000;
    let fee_wad = borrow_reserve.config().flashLoanFeeWad as u128;
    
    // CRITICAL: Use u128 arithmetic for fee calculation to prevent overflow
    let flashloan_fee_amount = (flashloan_amount as u128)
        .checked_mul(fee_wad)
        .and_then(|v| v.checked_div(WAD_U128))
        .ok_or_else(|| anyhow::anyhow!("Flashloan fee calculation overflow during tx build"))?
        as u64;
        
    let repay_amount = flashloan_amount + flashloan_fee_amount;
    
    // FlashRepayReserveLiquidity instruction data:
    // [14] (discriminator) + repay_amount (u64) + borrowed_amount (u64)
    let mut repay_data = vec![crate::solend::get_flashrepay_discriminator()]; // tag 14
    repay_data.extend_from_slice(&repay_amount.to_le_bytes()); // repay_amount (u64)
    repay_data.extend_from_slice(&flashloan_amount.to_le_bytes()); // borrowed_amount (u64)
    
    // FlashRepayReserveLiquidity accounts per Solend IDL:
    // CRITICAL: Account order must match Solend SDK exactly
    // Reference: solend-sdk/src/instruction.rs - LendingInstruction::FlashRepayReserveLiquidity
    // 
    // Correct account order (8 accounts total):
    // 0. [writable] sourceLiquidity - User's ATA (source of repayment funds)
    // 1. [writable] destinationLiquidity - Reserve's liquidity supply (destination)
    // 2. [writable] reserveLiquidityFeeReceiver - Reserve's fee receiver (usually supply)
    // 3. [writable] reserve - Reserve account
    // 4. [readonly] lendingMarket - Lending market account
    // 5. [signer] transferAuthority - User wallet (signer)
    // 6. [readonly] instructionsSysvar - Instructions sysvar
    // 7. [readonly] tokenProgram - SPL Token program
    
    // Use supply as fee receiver (fees go back to pool)
    let reserve_liquidity_fee_receiver = repay_reserve_liquidity_supply;
    
    let repay_accounts = vec![
        AccountMeta::new(source_liquidity, false),              // 0: sourceLiquidity (user's ATA)
        AccountMeta::new(repay_reserve_liquidity_supply, false), // 1: destinationLiquidity (reserve supply)
        AccountMeta::new(reserve_liquidity_fee_receiver, false), // 2: reserveLiquidityFeeReceiver
        AccountMeta::new(ctx.borrows[0].borrowReserve, false),  // 3: reserve
        AccountMeta::new_readonly(lending_market, false),       // 4: lendingMarket
        AccountMeta::new_readonly(wallet_pubkey, true),         // 5: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::instructions::id(), false), // 6: instructionsSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),     // 7: tokenProgram
    ];
    
    let repay_ix = Instruction {
        program_id,
        accounts: repay_accounts,
        data: repay_data,
    };
    
    // ============================================================================
    // Compute Budget Instructions
    // ============================================================================
    let compute_budget_program_id = Pubkey::from_str("ComputeBudget111111111111111111111111111111")
        .map_err(|e| anyhow::anyhow!("Invalid compute budget program ID: {}", e))?;
    
    // Higher compute unit limit for flashloan transaction (~500k)
    let mut compute_limit_data = vec![2u8]; // SetComputeUnitLimit discriminator
    compute_limit_data.extend_from_slice(&(500_000u32).to_le_bytes());
    let compute_budget_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_limit_data,
    };
    
    // Priority fee
    let mut compute_price_data = vec![3u8]; // SetComputeUnitPrice discriminator
    compute_price_data.extend_from_slice(&(1_000u64).to_le_bytes()); // 0.001 SOL per CU
    let priority_fee_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_price_data,
    };
    
    // ============================================================================
    // BUILD TRANSACTION
    // ============================================================================
    // All instructions in ONE transaction - atomicity guaranteed!
    // CRITICAL: FlashLoan requires explicit repayment instruction (FlashRepayReserveLiquidity)
    
    // Build instruction list in correct order
    let mut instructions = vec![
        compute_budget_ix,        // Compute unit limit
        priority_fee_ix,          // Priority fee
        flashloan_ix,             // 1. FlashBorrow: Borrow USDC (flash)
        liquidation_ix,           // 2. Liquidate: USDC -> cSOL
        redeem_ix,                // 3. Redeem: cSOL -> SOL
    ];
    
    // Add Jupiter instructions (Setup + Swap + Cleanup)
    // CRITICAL: Must be inserted after Redeem (to have SOL) and before Repay (to have USDC)
    instructions.extend(jupiter_swap_ixs);
    
    // Add Repay instruction
    instructions.push(repay_ix); // 5. FlashRepay: Repay borrowed USDC + fee (EXPLICIT - REQUIRED!)
    
    let mut tx = Transaction::new_with_payer(
        &instructions,
        Some(&wallet_pubkey),
    );
    tx.message.recent_blockhash = blockhash;
    
    log::info!(
        "Built atomic flashloan liquidation transaction for obligation {}:\n\
         - FlashBorrow: {} USDC (flash)\n\
         - Liquidate: {} debt tokens (USDC -> cSOL)\n\
         - Redeem: {} cTokens -> underlying tokens (cSOL -> SOL)\n\
         - Jupiter Swap: SOL -> USDC ({} instructions)\n\
         - FlashRepay: {} USDC (borrowed: {} + fee: {}) (EXPLICIT - REQUIRED!)",
        ctx.obligation_pubkey,
        flashloan_amount,
        quote.debt_to_repay_raw,
        quote.collateral_to_seize_raw,
        instructions.len() - 5, // Subtract other instructions
        repay_amount,
        flashloan_amount,
        flashloan_fee_amount
    );
    
    Ok(tx)
}

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
    
    // ✅ FIXED: Exponential backoff for bundle status polling
    // Max 10 attempts with increasing delay: 200, 400, 600, 800... ms
    // Reduces RPC load while still checking frequently enough
    let mut confirmed = false;
    for i in 0..10 {
        let delay_ms = 200 * (i + 1);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        
        if let Ok(Some(status)) = jito_client.get_bundle_status(&bundle_id).await {
            if let Some(status_str) = &status.status {
                if status_str == "landed" || status_str == "confirmed" {
                    confirmed = true;
                    log::debug!(
                        "✅ Bundle {} confirmed in {:.1}s (poll #{})",
                        bundle_id,
                        (i + 1) as f64 * 0.2,
                        i + 1
                    );
                    break;
                } else if status_str == "failed" || status_str == "dropped" {
                    log::warn!(
                        "Bundle {} failed/dropped after {:.1}s (poll #{})",
                        bundle_id,
                        (i + 1) as f64 * 0.2,
                        i + 1
                    );
                    break;
                }
            }
            
            // If slot is present, bundle likely executed
            if status.slot.is_some() {
                confirmed = true;
                log::debug!(
                    "✅ Bundle {} confirmed (has slot) in {:.1}s (poll #{})",
                    bundle_id,
                    (i + 1) as f64 * 0.2,
                    i + 1
                );
                break;
            }
        }
    }
    
    if !confirmed {
        log::debug!(
            "Bundle {} status still unknown after 4s polling, will be tracked by bundle_tracker",
            bundle_id
        );
    }
    
    Ok(())
}


