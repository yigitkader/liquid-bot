use anyhow::{Context, Result};
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::health::HealthManager;
use crate::protocol::Protocol;
use crate::solana_client::SolanaClient;
use base64::{Engine as _, engine::general_purpose};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use sha2::Digest;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// WebSocket subscription request for Solana PubSub API
#[derive(Serialize)]
struct SubscribeRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Vec<serde_json::Value>,
}

/// WebSocket subscription response from Solana PubSub API
#[derive(Deserialize, Debug)]
struct SubscribeResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Option<u64>,
    result: Option<u64>, // Subscription ID
    error: Option<SubscribeError>,
}

#[derive(Deserialize, Debug)]
struct SubscribeError {
    code: i32,
    message: String,
}

/// Program notification from Solana PubSub API
/// Format: https://docs.solana.com/api/websocket#programsubscribe
#[derive(Deserialize, Debug)]
struct ProgramNotification {
    #[allow(dead_code)]
    jsonrpc: String,
    method: String,
    params: ProgramNotificationParams,
}

#[derive(Deserialize, Debug)]
struct ProgramNotificationParams {
    result: ProgramNotificationResult,
    subscription: u64,
}

#[derive(Deserialize, Debug)]
struct ProgramNotificationResult {
    #[serde(rename = "context")]
    #[allow(dead_code)]
    context: NotificationContext,
    #[serde(rename = "value")]
    value: ProgramAccountValue,
}

#[derive(Deserialize, Debug)]
struct NotificationContext {
    #[allow(dead_code)]
    slot: u64,
}

/// Program account value in notification
/// This contains the account pubkey and account data
#[derive(Deserialize, Debug)]
struct ProgramAccountValue {
    #[serde(rename = "pubkey")]
    pubkey: String, // Account pubkey as string
    #[serde(rename = "account")]
    account: AccountData,
}

#[derive(Deserialize, Debug)]
struct AccountData {
    data: Vec<String>, // [base64_data, encoding]
    executable: bool,
    lamports: u64,
    owner: String,
    rent_epoch: u64,
}

/// Slot notification from Solana PubSub API
/// Format: https://docs.solana.com/api/websocket#slotsubscribe
#[derive(Deserialize, Debug)]
struct SlotNotification {
    #[allow(dead_code)]
    jsonrpc: String,
    method: String,
    params: SlotNotificationParams,
}

#[derive(Deserialize, Debug)]
struct SlotNotificationParams {
    result: SlotNotificationResult,
    subscription: u64,
}

#[derive(Deserialize, Debug)]
struct SlotNotificationResult {
    slot: u64,
    parent: Option<u64>,
    root: Option<u64>,
}

/// Account notification from Solana PubSub API
/// Format: https://docs.solana.com/api/websocket#accountsubscribe
#[derive(Deserialize, Debug)]
struct AccountNotification {
    #[allow(dead_code)]
    jsonrpc: String,
    method: String,
    params: AccountNotificationParams,
}

#[derive(Deserialize, Debug)]
struct AccountNotificationParams {
    result: AccountNotificationResult,
    subscription: u64,
}

#[derive(Deserialize, Debug)]
struct AccountNotificationResult {
    #[serde(rename = "context")]
    #[allow(dead_code)]
    context: NotificationContext,
    #[serde(rename = "value")]
    value: AccountNotificationValue,
}

#[derive(Deserialize, Debug)]
struct AccountNotificationValue {
    #[serde(rename = "data")]
    data: Vec<String>, // [base64_data, encoding]
    executable: bool,
    lamports: u64,
    owner: String,
    rent_epoch: u64,
}

/// Account cache for tracking known accounts
/// Used to identify accounts from WebSocket notifications
type AccountCache = Arc<tokio::sync::RwLock<HashMap<String, Pubkey>>>;

/// WebSocket listener - account değişikliklerini dinler
/// 
/// Solana WebSocket PubSub API kullanarak program account'larını real-time dinler.
/// Bu yaklaşım RPC polling'den çok daha hızlıdır (<100ms latency) ve rate limit sorunu yoktur.
/// 
/// Strategy:
/// 1. Initial account discovery: RPC ile tüm program account'larını fetch et ve cache'le
/// 2. WebSocket subscription: programSubscribe ile değişiklikleri dinle
/// 3. Real-time updates: Gelen bildirimleri parse et ve event bus'a yayınla
pub async fn run_ws_listener(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    let program_id = protocol.program_id();
    // Fallback WebSocket URL: Solana'nın resmi WS endpoint'i (programSubscribe %100 destekli)
    let fallback_ws_url = "wss://api.mainnet-beta.solana.com".to_string();
    
    // ✅ DÜZELTME: Config mutation yerine direkt doğru URL'yi seç
    // Eğer HTTP endpoint Helius ise ve WS endpoint de Helius'u işaret ediyorsa,
    // Helius WS'in `programSubscribe` desteği olmadığı bilindiği için en baştan
    // Solana'nın resmi WS endpoint'ine yönlen.
    //
    // Böylece:
    // - `Method not found` hatasını hiç görmeyiz
    // - Loglar daha temiz olur
    // - HTTP tarafında Helius'un tüm avantajlarını (yüksek rate limit vb.) kullanmaya devam ederiz
    let http_is_helius = config.rpc_http_url.to_lowercase().contains("helius");
    let ws_is_helius = config.rpc_ws_url.to_lowercase().contains("helius");
    let is_helius_combo = http_is_helius && ws_is_helius;
    
    // ✅ DÜZELTME: Config mutation yerine direkt doğru URL'yi seç
    let mut ws_url = if is_helius_combo {
        log::info!("💡 Helius HTTP + Helius WS algılandı. Helius WS `programSubscribe` desteklemediği için, WebSocket endpoint'i otomatik olarak Solana'nın resmi WS'ine taşınıyor.");
        log::info!("   WS: {} -> {}", config.rpc_ws_url, fallback_ws_url);
        fallback_ws_url.clone()
    } else {
        config.rpc_ws_url.clone()
    };
    let mut tried_fallback_ws = false;
    let mut reconnect_delay = Duration::from_secs(1);
    let max_reconnect_delay = Duration::from_secs(60);
    let mut consecutive_errors = 0u32;
    let max_consecutive_errors = config.max_consecutive_errors;

    log::info!(
        "📡 Starting WebSocket listener for protocol: {} (program: {})",
        protocol.id(),
        program_id
    );
    log::info!("   WebSocket URL: {}", ws_url);

    // Step 1: Initial account discovery via RPC
    // This ensures we have all existing accounts before starting WebSocket
    log::info!("🔍 Performing initial account discovery via RPC...");
    let account_cache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
    
    match initial_account_discovery(
        &rpc_client,
        &protocol,
        &bus,
        Arc::clone(&account_cache),
        Arc::clone(&health_manager),
    )
    .await
    {
        Ok(count) => {
            log::info!("✅ Initial discovery complete: {} accounts found and cached", count);
            consecutive_errors = 0; // Reset on successful discovery
        }
        Err(e) => {
            log::warn!("⚠️  Initial discovery failed: {}. Continuing with WebSocket only.", e);
            // Continue anyway - WebSocket will still work for new accounts
        }
    }

    // Step 1.5: Discover oracle accounts from reserves
    // This collects all oracle accounts (Pyth/Switchboard) from reserve accounts
    // so we can subscribe to price feed updates
    // 
    // ✅ FIXED: Reserve struct layout corrected - oracle_option field added, oracle offsets fixed
    log::info!("🔍 Discovering oracle accounts from reserves...");
    let oracle_accounts = discover_oracle_accounts(&rpc_client, &protocol).await;
    let oracle_accounts = match oracle_accounts {
        Ok(accounts) => {
            log::info!("✅ Found {} oracle accounts to subscribe", accounts.len());
            if !accounts.is_empty() {
                log::debug!("Sample oracle accounts: {:?}", accounts.iter().take(5).map(|info| info.oracle_account).collect::<Vec<_>>());
            }
            // Limit to reasonable number to prevent WebSocket overload
            // If too many, only use first 100 (most important ones)
            if accounts.len() > 100 {
                log::warn!("⚠️  Found {} oracle accounts, limiting to first 100 to prevent WebSocket overload", accounts.len());
                accounts.into_iter().take(100).collect()
            } else {
                accounts
            }
        }
        Err(e) => {
            log::warn!("⚠️  Oracle discovery failed: {}. Price feed subscriptions will be skipped.", e);
            Vec::new()
        }
    };

    // Step 2: Start WebSocket listener
    let mut subscription_id_counter = 1u64;

    loop {
        match connect_and_listen(
            &ws_url,
            &program_id,
            &bus,
            Arc::clone(&rpc_client),
            protocol.as_ref(),
            Arc::clone(&health_manager),
            Arc::clone(&account_cache),
            &mut subscription_id_counter,
            &oracle_accounts,
        )
        .await
        {
            Ok(()) => {
                // Normal shutdown (shouldn't happen in production)
                log::info!("WebSocket connection closed normally");
                break;
            }
            Err(e) => {
                let error_str = e.to_string();
                
                // Check if this is a "Method not found" or "subscription not supported" error
                // These indicate the RPC endpoint doesn't support WebSocket subscriptions.
                // Some premium HTTP RPC providers (e.g., Helius) desteklemiyor olabilir.
                // Bu durumda önce Solana'nın resmi WS endpoint'ine otomatik fallback deniyoruz.
                if error_str.contains("Method not found")
                    || error_str.contains("subscription not supported")
                    || error_str.contains("code: -32601")
                {
                    log::error!("❌ WebSocket subscriptions not supported by this RPC endpoint");
                    log::warn!("⚠️  The endpoint '{}' does not support 'programSubscribe' method", ws_url);

                    // Eğer şu anda Helius (veya başka bir HTTP endpoint) kullanıyorsak ve WS de aynı
                    // endpoint'i gösteriyorsa, WS'i otomatik olarak Solana'nın resmi WS'ine taşımayı dene.
                    // ✅ DÜZELTME: Aynı mantığı kullan, ama daha temiz
                    let http_is_helius = config.rpc_http_url.to_lowercase().contains("helius");
                    let ws_is_helius = ws_url.to_lowercase().contains("helius");
                    let is_helius_combo = http_is_helius && ws_is_helius;

                    if is_helius_combo && !tried_fallback_ws {
                        log::info!("💡 Helius HTTP + Helius WS kombinasyonu algılandı, ancak WS programSubscribe desteklemiyor.");
                        log::info!(
                            "   Otomatik çözüm: WebSocket endpoint'i Solana'nın resmi WS'ine alınacak: {} -> {}",
                            ws_url,
                            fallback_ws_url
                        );
                        ws_url = fallback_ws_url.clone();
                        tried_fallback_ws = true;
                        log::info!("🔁 Retrying WebSocket connection with fallback WS endpoint...");
                        // Döngüyü yeniden dene (RPC polling'e düşme, sadece WS endpoint'ini değiştir)
                        continue;
                    }

                    log::info!("💡 Falling back to RPC polling (this will be handled by data_source)");
                    return Err(e);
                }
                
                consecutive_errors += 1;
                let error_msg = format!("WebSocket error: {}", e);
                log::error!("{} (consecutive errors: {})", error_msg, consecutive_errors);
                health_manager.record_error(error_msg.clone()).await;

                if consecutive_errors >= max_consecutive_errors {
                    log::error!(
                        "Too many consecutive WebSocket errors ({}), stopping listener",
                        consecutive_errors
                    );
                    return Err(anyhow::anyhow!("Too many consecutive WebSocket errors"));
                }

                // Exponential backoff with jitter
                let jitter_ms = rand::thread_rng().gen_range(0..1000);
                let backoff = reconnect_delay + Duration::from_millis(jitter_ms);
                log::warn!("Reconnecting in {:.2}s...", backoff.as_secs_f64());
                sleep(backoff).await;

                // Increase reconnect delay (exponential backoff, capped at max)
                reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
            }
        }
    }

    Ok(())
}

/// Initial account discovery: Fetch all program accounts via RPC and cache them
async fn initial_account_discovery(
    rpc_client: &Arc<SolanaClient>,
    protocol: &Arc<dyn Protocol>,
    bus: &EventBus,
    account_cache: AccountCache,
    health_manager: Arc<HealthManager>,
) -> Result<usize> {
    let program_id = protocol.program_id();

    // Fetch all program accounts
    let accounts = rpc_client
        .get_program_accounts(&program_id)
        .await
        .context("Failed to fetch program accounts for initial discovery")?;

    log::debug!(
        "Fetched {} accounts for initial discovery (program: {})",
        accounts.len(),
        program_id
    );

    let mut position_count = 0;
    let mut cache = account_cache.write().await;

    // Process each account
    for (account_pubkey, account) in accounts {
        // Cache the account pubkey (using account data hash as key for lookup)
        // Note: We hash the raw account data for cache lookup
        let account_data_hash = format!("{:x}", sha2::Sha256::digest(&account.data));
        cache.insert(account_data_hash, account_pubkey);

        // Parse and publish position
        match protocol
            .parse_account_position(&account_pubkey, &account, Some(Arc::clone(rpc_client)))
            .await
        {
            Ok(Some(position)) => {
                bus.publish(Event::AccountUpdated(position))
                    .map_err(|e| anyhow::anyhow!("Failed to publish event: {}", e))?;
                position_count += 1;
            }
            Ok(None) => {
                // This account is not a position (e.g., market account, reserve account, etc.)
                // Ignore it
            }
            Err(e) => {
                log::warn!("Failed to parse account {}: {}", account_pubkey, e);
                // Continue on parse error
            }
        }
    }

    health_manager.record_successful_poll().await;
    Ok(position_count)
}

/// Oracle account info with mint mapping
#[derive(Debug, Clone)]
struct OracleAccountInfo {
    oracle_account: Pubkey,
    mint: Option<Pubkey>,
    label: String,
}

/// Discover oracle accounts from reserve accounts
/// This scans all program accounts to find reserves and extracts their oracle accounts
async fn discover_oracle_accounts(
    rpc_client: &Arc<SolanaClient>,
    protocol: &Arc<dyn Protocol>,
) -> Result<Vec<OracleAccountInfo>> {
    // Get all program accounts
    let accounts = rpc_client
        .get_program_accounts(&protocol.program_id())
        .await
        .context("Failed to fetch program accounts for oracle discovery")?;

    let total_accounts = accounts.len();
    let mut oracle_accounts = Vec::new();
    let mut reserve_count = 0;

    // Scan accounts to find reserves
    let mut parse_errors = 0;
    let mut first_parse_error: Option<String> = None;
    let mut sample_accounts_checked = 0;
    const MAX_SAMPLE_ACCOUNTS: usize = 100; // Check first 100 accounts to detect parsing issues early
    
    for (account_pubkey, account) in accounts {
        // Try to parse as reserve account
        // Note: We use reserve_helper to parse reserves
        use crate::protocols::reserve_helper::parse_reserve_account;
        
        sample_accounts_checked += 1;
        
        match parse_reserve_account(&account_pubkey, &account).await {
            Ok(reserve_info) => {
                reserve_count += 1;
                
                // Get mint from reserve (liquidity_mint or collateral_mint)
                let mint = reserve_info.liquidity_mint.or(reserve_info.collateral_mint);
                
                // Extract oracle accounts with mint mapping
                // Filter out default/empty pubkeys (these indicate no oracle configured)
                if let Some(pyth_oracle) = reserve_info.pyth_oracle {
                    if pyth_oracle != Pubkey::default() {
                        oracle_accounts.push(OracleAccountInfo {
                            oracle_account: pyth_oracle,
                            mint,
                            label: format!("pyth_{}", account_pubkey),
                        });
                        log::debug!("Found Pyth oracle: {} for reserve {}", pyth_oracle, account_pubkey);
                    } else {
                        log::debug!("Reserve {} has default/empty Pyth oracle, skipping", account_pubkey);
                    }
                }
                if let Some(switchboard_oracle) = reserve_info.switchboard_oracle {
                    if switchboard_oracle != Pubkey::default() {
                        oracle_accounts.push(OracleAccountInfo {
                            oracle_account: switchboard_oracle,
                            mint,
                            label: format!("switchboard_{}", account_pubkey),
                        });
                        log::debug!("Found Switchboard oracle: {} for reserve {}", switchboard_oracle, account_pubkey);
                    } else {
                        log::debug!("Reserve {} has default/empty Switchboard oracle, skipping", account_pubkey);
                    }
                }
            }
            Err(e) => {
                // Not a reserve account, skip silently (most accounts are not reserves)
                // But track errors to detect if struct layout is wrong
                parse_errors += 1;
                
                // Save first parse error for detailed reporting
                if first_parse_error.is_none() {
                    first_parse_error = Some(format!("Account {}: {}", account_pubkey, e));
                }
                
                // Early detection: If first 100 accounts all fail to parse, struct layout is likely wrong
                if sample_accounts_checked >= MAX_SAMPLE_ACCOUNTS && reserve_count == 0 {
                    log::error!("❌ CRITICAL: First {} accounts all failed to parse as reserves!", MAX_SAMPLE_ACCOUNTS);
                    log::error!("   This indicates the reserve struct layout is INCORRECT!");
                    log::error!("   First parse error: {}", first_parse_error.as_ref().unwrap());
                    log::error!("   ");
                    log::error!("   ACTION REQUIRED:");
                    log::error!("   1. Check src/protocols/solend_reserve.rs struct layout");
                    log::error!("   2. Run: ./scripts/check_oracle_option.sh to verify offsets");
                    log::error!("   3. Compare with official Solend source code");
                    log::error!("   ");
                    // Continue scanning but we know there's a problem
                }
                
                // Only log first few errors to avoid spam
                if parse_errors <= 5 {
                    log::debug!("Account {} is not a reserve: {}", account_pubkey, e);
                }
            }
        }
    }
    
    if parse_errors > 5 {
        log::debug!("Skipped {} non-reserve accounts during oracle discovery", parse_errors);
    }

    log::info!(
        "Oracle discovery: scanned {} accounts, found {} reserves, extracted {} oracle accounts",
        total_accounts,
        reserve_count,
        oracle_accounts.len()
    );
    
    // ❌ CRITICAL CHECK: If no reserves found, struct layout is 100% wrong
    if reserve_count == 0 {
        log::error!("❌ CRITICAL ERROR: No reserves found during oracle discovery!");
        log::error!("   Scanned {} accounts, but 0 reserves were successfully parsed.", total_accounts);
        log::error!("   ");
        log::error!("   This means the reserve struct layout in src/protocols/solend_reserve.rs is INCORRECT!");
        log::error!("   ");
        log::error!("   Possible causes:");
        log::error!("   1. Struct field order is wrong");
        log::error!("   2. Field types are wrong (u8 vs u32, etc.)");
        log::error!("   3. Missing fields (e.g., oracle_option)");
        log::error!("   4. Wrong offsets (oracle positions incorrect)");
        log::error!("   ");
        log::error!("   ACTION REQUIRED:");
        log::error!("   1. Run: ./scripts/check_oracle_option.sh to check real reserve layout");
        log::error!("   2. Compare with official Solend source code:");
        log::error!("      https://github.com/solendprotocol/solana-program-library/blob/master/token-lending/program/src/state/reserve.rs");
        log::error!("   3. Update src/protocols/solend_reserve.rs struct to match");
        log::error!("   ");
        if let Some(ref first_error) = first_parse_error {
            log::error!("   First parse error: {}", first_error);
        }
        log::error!("   ");
        
        // Return empty vector but log critical error
        // Bot can continue without oracle discovery, but this needs to be fixed
        log::error!("   Oracle discovery will be skipped due to struct layout mismatch.");
        log::error!("   Bot will continue running, but price feed subscriptions will not work.");
        return Ok(Vec::new());
    }
    
    if oracle_accounts.is_empty() && reserve_count > 0 {
        log::warn!("⚠️  Found {} reserves but no oracle accounts. This might indicate:", reserve_count);
        log::warn!("   1. Reserves don't have oracle accounts configured");
        log::warn!("   2. Oracle accounts are default/empty pubkeys");
    }

    // Remove duplicates (same oracle account might be used by multiple reserves)
    oracle_accounts.sort_by_key(|info| info.oracle_account);
    oracle_accounts.dedup_by_key(|info| info.oracle_account);

    Ok(oracle_accounts)
}

async fn connect_and_listen(
    ws_url: &str,
    program_id: &Pubkey,
    bus: &EventBus,
    rpc_client: Arc<SolanaClient>,
    protocol: &dyn Protocol,
    health_manager: Arc<HealthManager>,
    account_cache: AccountCache,
    subscription_id_counter: &mut u64,
    oracle_accounts: &[OracleAccountInfo],
) -> Result<()> {
    // Connect to WebSocket
    let (ws_stream, _) = connect_async(ws_url)
        .await
        .context("Failed to connect to WebSocket endpoint")?;

    log::info!("✅ WebSocket connected");

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to program accounts
    // Solana PubSub API: programSubscribe method
    // Documentation: https://docs.solana.com/api/websocket#programsubscribe
    // Format: params must be an array with [program_id_string, options_object]
    let subscribe_request = SubscribeRequest {
        jsonrpc: "2.0".to_string(),
        id: *subscription_id_counter,
        method: "programSubscribe".to_string(),
        params: vec![
            json!(program_id.to_string()),
            json!({
                "encoding": "base64",
                "commitment": "confirmed"
            }),
        ],
    };

    let request_json = serde_json::to_string(&subscribe_request)
        .context("Failed to serialize subscribe request")?;

    log::info!("📤 Sending programSubscribe request: {}", request_json);
    log::info!("   Program ID: {}", program_id);

    write
        .send(Message::Text(request_json))
        .await
        .context("Failed to send subscription request")?;

    *subscription_id_counter += 1;

    // Subscribe to slot updates (for periodic re-check)
    // Solana PubSub API: slotSubscribe method
    // Documentation: https://docs.solana.com/api/websocket#slotsubscribe
    let slot_subscribe_request = SubscribeRequest {
        jsonrpc: "2.0".to_string(),
        id: *subscription_id_counter,
        method: "slotSubscribe".to_string(),
        params: vec![],
    };

    let slot_request_json = serde_json::to_string(&slot_subscribe_request)
        .context("Failed to serialize slot subscribe request")?;

    log::info!("📤 Sending slotSubscribe request: {}", slot_request_json);

    write
        .send(Message::Text(slot_request_json))
        .await
        .context("Failed to send slot subscription request")?;

    *subscription_id_counter += 1;

    // Subscribe to oracle account updates (price feeds)
    // Solana PubSub API: accountSubscribe method
    // Documentation: https://docs.solana.com/api/websocket#accountsubscribe
    let mut oracle_subscription_ids: Vec<(Pubkey, u64)> = Vec::new();
    
    for oracle_info in oracle_accounts {
        let account_subscribe_request = SubscribeRequest {
            jsonrpc: "2.0".to_string(),
            id: *subscription_id_counter,
            method: "accountSubscribe".to_string(),
            params: vec![
                json!(oracle_info.oracle_account.to_string()),
                json!({
                    "encoding": "base64",
                    "commitment": "confirmed"
                }),
            ],
        };

        let account_request_json = serde_json::to_string(&account_subscribe_request)
            .context("Failed to serialize account subscribe request")?;

        log::info!("📤 Sending accountSubscribe request for oracle: {} ({})", oracle_info.oracle_account, oracle_info.label);

        write
            .send(Message::Text(account_request_json))
            .await
            .context("Failed to send account subscription request")?;

        *subscription_id_counter += 1;
    }

    if !oracle_accounts.is_empty() {
        log::info!("📤 Subscribed to {} oracle accounts for price feed updates", oracle_accounts.len());
    }

    // Wait for subscription confirmation
    let mut subscription_id: Option<u64> = None;
    let mut slot_subscription_id: Option<u64> = None;
    let mut confirmed = false;
    let mut oracle_subscriptions_confirmed = 0usize;
    let mut consecutive_errors = 0u32;
    const MAX_CONSECUTIVE_NOTIFICATION_ERRORS: u32 = 100; // Max errors before reconnecting

    // Read messages with timeout
    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        log::debug!("📥 Received WebSocket message ({} bytes): {}", text.len(), text);

                        // Try to parse as subscription response
                        if let Ok(response) = serde_json::from_str::<SubscribeResponse>(&text) {
                            if let Some(sub_id) = response.result {
                                // Check which subscription this is
                                // We track subscription IDs by checking which one is None
                                if subscription_id.is_none() {
                                    subscription_id = Some(sub_id);
                                    log::info!("✅ Subscribed to program accounts (subscription ID: {})", sub_id);
                                } else if slot_subscription_id.is_none() {
                                    slot_subscription_id = Some(sub_id);
                                    log::info!("✅ Subscribed to slot updates (subscription ID: {})", sub_id);
                                } else if oracle_subscriptions_confirmed < oracle_accounts.len() {
                                    // This is an oracle account subscription
                                    let oracle_info = &oracle_accounts[oracle_subscriptions_confirmed];
                                    oracle_subscription_ids.push((oracle_info.oracle_account, sub_id));
                                    oracle_subscriptions_confirmed += 1;
                                    log::info!("✅ Subscribed to oracle account {} (subscription ID: {})", oracle_info.oracle_account, sub_id);
                                }
                                confirmed = true;
                                consecutive_errors = 0; // Reset on successful subscription
                                health_manager.record_successful_poll().await;
                            } else if let Some(error) = response.error {
                                // Check if this is a "Method not found" error (code: -32601)
                                // This indicates the RPC endpoint doesn't support WebSocket subscriptions
                                if error.code == -32601 {
                                    log::error!("❌ Subscription failed - Method not found. Full response: {}", text);
                                    return Err(anyhow::anyhow!(
                                        "WebSocket subscription not supported by RPC endpoint: {} (code: {}). \
                                        This endpoint does not support 'programSubscribe' method. \
                                        Please use a premium RPC provider (Helius, Triton, QuickNode) or the bot will fallback to RPC polling.",
                                        error.message,
                                        error.code
                                    ));
                                }
                                log::error!("❌ Subscription error. Full response: {}", text);
                                return Err(anyhow::anyhow!(
                                    "Subscription error: {} (code: {})",
                                    error.message,
                                    error.code
                                ));
                            }
                            continue;
                        }

                        // Try to parse as program notification
                        // Format: https://docs.solana.com/api/websocket#programsubscribe
                        // The notification contains the account pubkey in params.result.value.pubkey
                        if let Ok(notification) = serde_json::from_str::<ProgramNotification>(&text) {
                            if notification.method == "programNotification" {
                                if let Some(sub_id) = subscription_id {
                                    if notification.params.subscription == sub_id {
                                        // Extract account pubkey from notification
                                        let account_pubkey_str = &notification.params.result.value.pubkey;
                                        log::debug!("📨 Received programNotification for account: {}", account_pubkey_str);
                                        match account_pubkey_str.parse::<Pubkey>() {
                                            Ok(account_pubkey) => {
                                                // Process the notification
                                                if let Err(e) = handle_program_notification(
                                                    account_pubkey,
                                                    &notification.params.result.value.account,
                                                    bus,
                                                    Arc::clone(&rpc_client),
                                                    protocol,
                                                    Arc::clone(&health_manager),
                                                    Arc::clone(&account_cache),
                                                )
                                                .await
                                                {
                                                    log::warn!("Failed to handle program notification for {}: {}", account_pubkey, e);
                                                    consecutive_errors += 1;
                                                    if consecutive_errors >= MAX_CONSECUTIVE_NOTIFICATION_ERRORS {
                                                        log::error!(
                                                            "Too many consecutive notification errors ({}), reconnecting...",
                                                            consecutive_errors
                                                        );
                                                        return Ok(()); // Will trigger reconnect
                                                    }
                                                } else {
                                                    consecutive_errors = 0; // Reset on success
                                                }
                                            }
                                            Err(e) => {
                                                log::warn!("Failed to parse account pubkey '{}': {}", account_pubkey_str, e);
                                                log::debug!("Full notification: {}", text);
                                                consecutive_errors += 1;
                                            }
                                        }
                                    } else {
                                        log::debug!("Notification subscription ID mismatch: expected {}, got {}", sub_id, notification.params.subscription);
                                    }
                                } else {
                                    log::warn!("Received notification before subscription confirmed");
                                }
                            } else {
                                log::debug!("Received notification with unexpected method: {}", notification.method);
                            }
                            continue;
                        }

                        // Try to parse as account notification (oracle price updates)
                        if let Ok(notification) = serde_json::from_str::<AccountNotification>(&text) {
                            if notification.method == "accountNotification" {
                                // Check if this is one of our oracle account subscriptions
                                let oracle_info_opt = oracle_subscription_ids.iter()
                                    .find(|(_, sub_id)| notification.params.subscription == *sub_id)
                                    .and_then(|(account, _)| {
                                        oracle_accounts.iter().find(|info| info.oracle_account == *account)
                                    });
                                
                                if let Some(oracle_info) = oracle_info_opt {
                                    log::debug!("📨 Received accountNotification for oracle: {} ({})", oracle_info.oracle_account, oracle_info.label);
                                    
                                    // Parse oracle price from account data
                                    if let Err(e) = handle_oracle_notification(
                                        oracle_info.oracle_account,
                                        oracle_info.mint,
                                        &notification.params.result.value,
                                        bus,
                                        Arc::clone(&rpc_client),
                                    )
                                    .await
                                    {
                                        log::warn!("Failed to handle oracle notification for {}: {}", oracle_info.oracle_account, e);
                                        consecutive_errors += 1;
                                        if consecutive_errors >= MAX_CONSECUTIVE_NOTIFICATION_ERRORS {
                                            log::error!(
                                                "Too many consecutive notification errors ({}), reconnecting...",
                                                consecutive_errors
                                            );
                                            return Ok(()); // Will trigger reconnect
                                        }
                                    } else {
                                        consecutive_errors = 0; // Reset on success
                                    }
                                } else {
                                    log::debug!("Received account notification for unknown subscription ID: {}", notification.params.subscription);
                                }
                            } else {
                                log::debug!("Received notification with unexpected method: {}", notification.method);
                            }
                            continue;
                        }

                        // Try to parse as slot notification
                        if let Ok(notification) = serde_json::from_str::<SlotNotification>(&text) {
                            if notification.method == "slotNotification" {
                                if let Some(sub_id) = slot_subscription_id {
                                    if notification.params.subscription == sub_id {
                                        let slot = notification.params.result.slot;
                                        log::debug!("📨 Received slotNotification: slot={}", slot);
                                        
                                        // Publish SlotUpdate event
                                        if let Err(e) = bus.publish(Event::SlotUpdate { slot }) {
                                            log::warn!("Failed to publish SlotUpdate event: {}", e);
                                        } else {
                                            log::debug!("Published SlotUpdate event: slot={}", slot);
                                        }
                                    } else {
                                        log::debug!("Slot notification subscription ID mismatch: expected {}, got {}", sub_id, notification.params.subscription);
                                    }
                                } else {
                                    log::debug!("Received slot notification before subscription confirmed");
                                }
                            } else {
                                log::debug!("Received notification with unexpected method: {}", notification.method);
                            }
                            continue;
                        }

                        // Fallback: Try to parse as generic JSON to extract pubkey
                        // This handles different RPC provider formats (some providers return slightly different structures)
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(method) = parsed.get("method").and_then(|m| m.as_str()) {
                                if method == "programNotification" {
                                    log::debug!("📨 Received programNotification (fallback parsing)");
                                    
                                    // Try multiple extraction paths for different RPC provider formats
                                    let mut account_pubkey: Option<Pubkey> = None;
                                    let mut account_data: Option<AccountData> = None;
                                    
                                    // Path 1: params.result.value.pubkey (standard format)
                                    if let Some(params) = parsed.get("params") {
                                        if let Some(result) = params.get("result") {
                                            if let Some(value) = result.get("value") {
                                                if let Some(pubkey_str) = value.get("pubkey").and_then(|p| p.as_str()) {
                                                    if let Ok(pubkey) = pubkey_str.parse::<Pubkey>() {
                                                        account_pubkey = Some(pubkey);
                                                    }
                                                }
                                                if let Some(acc_data) = value.get("account") {
                                                    if let Ok(acc) = serde_json::from_value::<AccountData>(acc_data.clone()) {
                                                        account_data = Some(acc);
                                                    }
                                                }
                                            }
                                        }
                                        
                                        // Path 2: params.value.pubkey (alternative format used by some providers)
                                        if account_pubkey.is_none() {
                                            if let Some(value) = params.get("value") {
                                                if let Some(pubkey_str) = value.get("pubkey").and_then(|p| p.as_str()) {
                                                    if let Ok(pubkey) = pubkey_str.parse::<Pubkey>() {
                                                        account_pubkey = Some(pubkey);
                                                    }
                                                }
                                                if let Some(acc_data) = value.get("account") {
                                                    if let Ok(acc) = serde_json::from_value::<AccountData>(acc_data.clone()) {
                                                        account_data = Some(acc);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    
                                    // Process if we successfully extracted both
                                    if let (Some(pubkey), Some(acc)) = (account_pubkey, account_data) {
                                        log::debug!("✅ Extracted account from notification (fallback): {}", pubkey);
                                        if let Err(e) = handle_program_notification(
                                            pubkey,
                                            &acc,
                                            bus,
                                            Arc::clone(&rpc_client),
                                            protocol,
                                            Arc::clone(&health_manager),
                                            Arc::clone(&account_cache),
                                        )
                                        .await
                                        {
                                            log::warn!("Failed to handle program notification (fallback) for {}: {}", pubkey, e);
                                            consecutive_errors += 1;
                                            if consecutive_errors >= MAX_CONSECUTIVE_NOTIFICATION_ERRORS {
                                                log::error!(
                                                    "Too many consecutive notification errors ({}), reconnecting...",
                                                    consecutive_errors
                                                );
                                                return Ok(()); // Will trigger reconnect
                                            }
                                        } else {
                                            consecutive_errors = 0;
                                        }
                                    } else {
                                        log::warn!("⚠️  Could not extract account data from notification (fallback). Method: {}, Full message: {}", method, text);
                                    }
                                    continue;
                                }
                            }
                        }

                        // Unknown message format - log for debugging
                        log::debug!("⚠️  Unknown message format (not a subscription response or notification): {}", text);
                    }
                    Some(Ok(Message::Close(_))) => {
                        log::warn!("WebSocket connection closed by server");
                        return Ok(()); // Normal close, will trigger reconnect
                    }
                    Some(Err(e)) => {
                        return Err(anyhow::anyhow!("WebSocket error: {}", e));
                    }
                    None => {
                        return Err(anyhow::anyhow!("WebSocket stream ended"));
                    }
                    _ => {
                        // Binary or other message types - ignore
                    }
                }
            }
            _ = sleep(Duration::from_secs(30)) => {
                // Send ping to keep connection alive
                if confirmed {
                    let ping = json!({
                        "jsonrpc": "2.0",
                        "id": *subscription_id_counter,
                        "method": "ping"
                    });
                    *subscription_id_counter += 1;
                    if let Err(e) = write.send(Message::Text(ping.to_string())).await {
                        log::warn!("Failed to send ping: {}", e);
                        return Err(anyhow::anyhow!("Failed to send ping"));
                    }
                }
            }
        }
    }
}

async fn handle_program_notification(
    account_pubkey: Pubkey,
    account_data: &AccountData,
    bus: &EventBus,
    rpc_client: Arc<SolanaClient>,
    protocol: &dyn Protocol,
    health_manager: Arc<HealthManager>,
    account_cache: AccountCache,
) -> Result<()> {
    // Update account cache
    // Decode account data first, then hash it for cache lookup
    let account_bytes = general_purpose::STANDARD
        .decode(&account_data.data[0])
        .context("Failed to decode account data for cache")?;
    let account_data_hash = format!("{:x}", sha2::Sha256::digest(&account_bytes));
    {
        let mut cache = account_cache.write().await;
        cache.insert(account_data_hash, account_pubkey);
    }

    // Account bytes already decoded above for cache
    // Reuse the decoded bytes

    // Parse owner pubkey
    let owner_pubkey = account_data.owner.parse::<Pubkey>()
        .context("Failed to parse owner pubkey")?;

    // Create Solana account struct
    let account = solana_sdk::account::Account {
        lamports: account_data.lamports,
        data: account_bytes,
        owner: owner_pubkey,
        executable: account_data.executable,
        rent_epoch: account_data.rent_epoch,
    };

    // Parse account position using protocol
    match protocol
        .parse_account_position(&account_pubkey, &account, Some(Arc::clone(&rpc_client)))
        .await
    {
        Ok(Some(position)) => {
            bus.publish(Event::AccountUpdated(position))
                .map_err(|e| anyhow::anyhow!("Failed to publish event: {}", e))?;
            health_manager.record_successful_poll().await;
            log::debug!("Published account update: {}", account_pubkey);
        }
        Ok(None) => {
            // This account is not a position (e.g., market account, reserve account, etc.)
            // Ignore it
        }
        Err(e) => {
            log::warn!("Failed to parse account {}: {}", account_pubkey, e);
            // Continue on parse error
        }
    }

    Ok(())
}

async fn handle_oracle_notification(
    oracle_account: Pubkey,
    mint: Option<Pubkey>,
    account_data: &AccountNotificationValue,
    bus: &EventBus,
    rpc_client: Arc<SolanaClient>,
) -> Result<()> {
    // Parse owner to determine oracle type
    let owner = account_data.owner.parse::<Pubkey>()
        .context("Failed to parse owner pubkey")?;

    // Check if this is a Pyth oracle
    use crate::protocols::oracle_helper::{PYTH_PROGRAM_ID, read_pyth_price};
    let pyth_program_id = Pubkey::try_from(PYTH_PROGRAM_ID)
        .context("Invalid Pyth program ID")?;

    if owner == pyth_program_id {
        // This is a Pyth oracle
        // Read price from Pyth oracle
        match read_pyth_price(&oracle_account, Arc::clone(&rpc_client), None).await {
            Ok(Some(price)) => {
                // Use mint from oracle info, or fallback to "unknown"
                let mint_str = mint.map(|m| m.to_string()).unwrap_or_else(|| "unknown".to_string());
                
                log::debug!(
                    "Oracle price update: oracle={}, mint={}, price=${:.4}, confidence=${:.4}",
                    oracle_account,
                    mint_str,
                    price.price,
                    price.confidence
                );

                // Publish PriceUpdate event
                if let Err(e) = bus.publish(Event::PriceUpdate {
                    mint: mint_str,
                    price: price.price,
                    confidence: price.confidence,
                    timestamp: price.timestamp,
                }) {
                    log::warn!("Failed to publish PriceUpdate event: {}", e);
                }
            }
            Ok(None) => {
                log::debug!("Oracle price not available for {}", oracle_account);
            }
            Err(e) => {
                log::warn!("Failed to read Pyth price for {}: {}", oracle_account, e);
            }
        }
    } else {
        // This might be a Switchboard oracle or other type
        log::debug!("Oracle account {} has unknown owner: {}", oracle_account, owner);
    }

    Ok(())
}
