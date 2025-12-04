use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use tokio::sync::{broadcast, Mutex};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Clone)]
pub struct AccountUpdate {
    pub pubkey: Pubkey,
    pub account: solana_sdk::account::Account,
    pub slot: u64,
}

pub struct WsClient {
    url: String,
    connection: Arc<
        Mutex<
            Option<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
            >,
        >,
    >,
    subscriptions: Arc<Mutex<HashMap<u64, SubscriptionInfo>>>,
    next_request_id: Arc<Mutex<u64>>,
    account_update_tx: broadcast::Sender<AccountUpdate>,
}

#[derive(Debug, Clone)]
struct SubscriptionInfo {
    subscription_type: SubscriptionType,
}

#[derive(Debug, Clone)]
enum SubscriptionType {
    Program(Pubkey),
    Account(Pubkey),
    Slot,
}

use std::sync::Arc;

impl WsClient {
    pub fn new(url: String) -> Self {
        let (tx, _) = broadcast::channel(1000);
        WsClient {
            url,
            connection: Arc::new(Mutex::new(None)),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            next_request_id: Arc::new(Mutex::new(1)),
            account_update_tx: tx,
        }
    }

    pub fn subscribe_account_updates(&self) -> broadcast::Receiver<AccountUpdate> {
        self.account_update_tx.subscribe()
    }

    pub async fn connect(&self) -> Result<()> {
        let (ws_stream, _) = connect_async(&self.url)
            .await
            .context("Failed to connect to WebSocket")?;

        let mut conn = self.connection.lock().await;
        *conn = Some(ws_stream);
        Ok(())
    }

    async fn send_request(&self, method: &str, params: serde_json::Value) -> Result<u64> {
        let mut request_id = self.next_request_id.lock().await;
        let id = *request_id;
        *request_id = request_id.wrapping_add(1);

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let mut conn = self.connection.lock().await;
        if let Some(ref mut stream) = *conn {
            let message = Message::Text(serde_json::to_string(&request)?);
            stream
                .send(message)
                .await
                .context("Failed to send WebSocket message")?;
        } else {
            return Err(anyhow::anyhow!("WebSocket not connected"));
        }

        Ok(id)
    }

    async fn wait_for_response(&self, expected_id: u64) -> Result<serde_json::Value> {
        let mut conn = self.connection.lock().await;
        if let Some(ref mut stream) = *conn {
            loop {
                if let Some(msg) = stream.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            let response: serde_json::Value = serde_json::from_str(&text)
                                .context("Failed to parse WebSocket response")?;

                            if let Some(id) = response.get("id").and_then(|v| v.as_u64()) {
                                if id == expected_id {
                                    if let Some(result) = response.get("result") {
                                        return Ok(result.clone());
                                    } else if let Some(error) = response.get("error") {
                                        return Err(anyhow::anyhow!(
                                            "WebSocket RPC error: {}",
                                            error
                                        ));
                                    }
                                }
                            }
                        }
                        Ok(Message::Close(_)) => {
                            return Err(anyhow::anyhow!("WebSocket connection closed"));
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!("WebSocket error: {}", e));
                        }
                        _ => {}
                    }
                } else {
                    return Err(anyhow::anyhow!("WebSocket stream ended"));
                }
            }
        } else {
            Err(anyhow::anyhow!("WebSocket not connected"))
        }
    }

    pub async fn subscribe_program(&self, program_id: &Pubkey) -> Result<u64> {
        let params = json!([
            program_id.to_string(),
            {
                "encoding": "base64",
                "commitment": "confirmed"
            }
        ]);

        let request_id = self.send_request("programSubscribe", params).await?;
        let result = self.wait_for_response(request_id).await?;

        let subscription_id = result
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid subscription ID in response"))?;

        let mut subscriptions = self.subscriptions.lock().await;
        subscriptions.insert(
            subscription_id,
            SubscriptionInfo {
                subscription_type: SubscriptionType::Program(*program_id),
            },
        );

        Ok(subscription_id)
    }

    pub async fn subscribe_account(&self, pubkey: &Pubkey) -> Result<u64> {
        let params = json!([
            pubkey.to_string(),
            {
            "encoding": "base64",
            "commitment": "confirmed"
            }
        ]);

        let request_id = self.send_request("accountSubscribe", params).await?;
        let result = self.wait_for_response(request_id).await?;

        let subscription_id = result
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid subscription ID in response"))?;

        let mut subscriptions = self.subscriptions.lock().await;
        subscriptions.insert(
            subscription_id,
            SubscriptionInfo {
                subscription_type: SubscriptionType::Account(*pubkey),
            },
        );

        Ok(subscription_id)
    }

    pub async fn subscribe_slot(&self) -> Result<u64> {
        let params = json!([]);

        let request_id = self.send_request("slotSubscribe", params).await?;
        let result = self.wait_for_response(request_id).await?;

        let subscription_id = result
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid subscription ID in response"))?;

        let mut subscriptions = self.subscriptions.lock().await;
        subscriptions.insert(
            subscription_id,
            SubscriptionInfo {
                subscription_type: SubscriptionType::Slot,
            },
        );

        Ok(subscription_id)
    }

    pub async fn listen(&self) -> Option<AccountUpdate> {
        let msg_opt = {
            let mut conn = self.connection.lock().await;
            if let Some(ref mut stream) = *conn {
                stream.next().await.map(|r| {
                    r.map_err(|e| {
                        let error_str = e.to_string();
                        log::warn!("WebSocket stream error: {} (connection may be lost)", error_str);
                        // Log additional context for common error types
                        if error_str.contains("ConnectionClosed") || error_str.contains("connection closed") {
                            log::debug!("WebSocket: Connection was closed by server or network");
                        } else if error_str.contains("timeout") || error_str.contains("Timeout") {
                            log::debug!("WebSocket: Connection timeout detected");
                        } else if error_str.contains("reset") || error_str.contains("Reset") {
                            log::debug!("WebSocket: Connection was reset");
                        }
                        e
                    }).ok()
                })
            } else {
                log::debug!("WebSocket listen: connection is None (not connected)");
                None
            }
        };

        if let Some(Some(msg)) = msg_opt {
            match msg {
                Message::Text(text) => {
                    if let Ok(notification) = serde_json::from_str::<serde_json::Value>(&text) {
                        let method = notification.get("method").and_then(|m| m.as_str());

                        if method == Some("programNotification")
                            || method == Some("accountNotification")
                        {
                            if let Some(params) = notification.get("params") {
                                let subscription_id =
                                    params.get("subscription").and_then(|v| v.as_u64());

                                if let Some(result) = params.get("result") {
                                    let slot = result
                                        .get("context")
                                        .and_then(|c| c.get("slot"))
                                        .and_then(|s| s.as_u64())
                                        .unwrap_or(0);

                                    let account_info =
                                        result.get("value").and_then(|v| v.as_object());

                                    if let Some(value) = account_info {
                                        let pubkey = if method == Some("accountNotification") {
                                            if let Some(sub_id) = subscription_id {
                                                let subscriptions = self.subscriptions.lock().await;
                                                subscriptions.get(&sub_id).and_then(|info| {
                                                    if let SubscriptionType::Account(pk) =
                                                        &info.subscription_type
                                                    {
                                                        Some(*pk)
                                                    } else {
                                                        None
                                                    }
                                                })
                                            } else {
                                                None
                                            }
                                        } else {
                                            value
                                                .get("pubkey")
                                                .and_then(|v| v.as_str())
                                                .and_then(|s| s.parse::<Pubkey>().ok())
                                        };

                                        let account_data = value
                                            .get("data")
                                            .and_then(|d| d.as_array())
                                            .filter(|arr| arr.len() >= 2)
                                            .and_then(|arr| arr[0].as_str());

                                        let owner = value
                                            .get("owner")
                                            .or_else(|| {
                                                value.get("account").and_then(|a| a.get("owner"))
                                            })
                                            .and_then(|v| v.as_str())
                                            .and_then(|s| s.parse::<Pubkey>().ok());

                                        let lamports = value
                                            .get("lamports")
                                            .or_else(|| {
                                                value.get("account").and_then(|a| a.get("lamports"))
                                            })
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0);

                                        let executable = value
                                            .get("executable")
                                            .or_else(|| {
                                                value
                                                    .get("account")
                                                    .and_then(|a| a.get("executable"))
                                            })
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);

                                        let rent_epoch = value
                                            .get("rentEpoch")
                                            .or_else(|| value.get("rent_epoch"))
                                            .or_else(|| {
                                                value
                                                    .get("account")
                                                    .and_then(|a| a.get("rentEpoch"))
                                            })
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0);

                                        if let (Some(pk), Some(owner_pk), Some(base64_data)) =
                                            (pubkey, owner, account_data)
                                        {
                                            use base64::{engine::general_purpose, Engine as _};
                                            if let Ok(decoded) =
                                                general_purpose::STANDARD.decode(base64_data)
                                            {
                                                let account = solana_sdk::account::Account {
                                                    lamports,
                                                    data: decoded,
                                                    owner: owner_pk,
                                                    executable,
                                                    rent_epoch,
                                                };

                                                let update = AccountUpdate {
                                                    pubkey: pk,
                                                    account,
                                                    slot,
                                                };

                                                let _ = self.account_update_tx.send(update.clone());

                                                return Some(update);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Message::Close(_) => {
                    return None;
                }
                _ => {}
            }
        }
        None
    }

    pub async fn reconnect_with_backoff(&self) -> Result<()> {
        const MAX_RECONNECT_ATTEMPTS: usize = 10;
        let mut backoff = Duration::from_secs(1);

        for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
            sleep(backoff).await;

            log::info!(
                "WebSocket reconnect attempt {}/{}",
                attempt,
                MAX_RECONNECT_ATTEMPTS
            );

            if self.connect().await.is_ok() {
                log::info!(
                    "✅ WebSocket reconnected successfully on attempt {}",
                    attempt
                );

                let old_subscriptions = {
                    let mut subscriptions = self.subscriptions.lock().await;
                    let old_count = subscriptions.len();
                    let old = subscriptions.clone(); // Clone before clearing

                    // ✅ CRITICAL: Clear old subscription IDs to prevent memory leak
                    subscriptions.clear();

                    if old_count > 0 {
                        log::info!(
                            "Cleared {} stale subscription(s) before resubscribing (preventing memory leak)",
                            old_count
                        );
                    }

                    old
                };

                let mut failed_subscriptions = Vec::new();

                for (old_id, info) in old_subscriptions.iter() {
                    let resubscribe_result = match &info.subscription_type {
                        SubscriptionType::Program(program_id) => {
                            self.subscribe_program(program_id).await.map(|new_id| {
                                log::info!(
                                    "✅ Resubscribed: {} -> {} (program: {})",
                                    old_id,
                                    new_id,
                                    program_id
                                );
                                new_id
                            })
                        }
                        SubscriptionType::Account(pubkey) => {
                            self.subscribe_account(pubkey).await.map(|new_id| {
                                log::info!(
                                    "✅ Resubscribed: {} -> {} (account: {})",
                                    old_id,
                                    new_id,
                                    pubkey
                                );
                                new_id
                            })
                        }
                        SubscriptionType::Slot => self.subscribe_slot().await.map(|new_id| {
                            log::info!("✅ Resubscribed: {} -> {} (slot)", old_id, new_id);
                            new_id
                        }),
                    };

                    match resubscribe_result {
                        Ok(_) => {
                            // Successfully resubscribed
                        }
                        Err(e) => {
                            log::error!("❌ Resubscribe failed for {}: {}", old_id, e);
                            failed_subscriptions.push((*old_id, info.clone()));
                            // ✅ FIX: Don't restore failed subscription - old ID is invalid
                            // This prevents silent data loss where we think we're subscribed but aren't
                            // The subscription will be retried below, and if that fails, it's logged
                        }
                    }
                }

                if !failed_subscriptions.is_empty() {
                    log::info!(
                        "Retrying {} failed subscriptions...",
                        failed_subscriptions.len()
                    );
                    for (old_id, info) in failed_subscriptions.iter() {
                        let retry_result = match &info.subscription_type {
                            SubscriptionType::Program(program_id) => {
                                self.subscribe_program(program_id).await
                            }
                            SubscriptionType::Account(pubkey) => {
                                self.subscribe_account(pubkey).await
                            }
                            SubscriptionType::Slot => self.subscribe_slot().await,
                        };

                        match retry_result {
                            Ok(new_id) => {
                                log::info!("✅ Retry successful: {} -> {}", old_id, new_id);
                                // New subscription ID already added to HashMap by subscribe_* methods
                            }
                            Err(e) => {
                                log::error!(
                                    "❌ Retry failed for {}: {} - subscription lost, will need manual resubscription",
                                    old_id,
                                    e
                                );
                                // ✅ FIX: Don't restore old subscription - it's invalid
                                // This prevents silent data loss where we think we're subscribed but aren't
                                // Scanner/other components should handle missing subscriptions
                            }
                        }
                    }
                }

                return Ok(());
            }

            backoff = backoff.saturating_mul(2);
            if backoff > Duration::from_secs(60) {
                backoff = Duration::from_secs(60);
            }

            log::warn!(
                "WebSocket reconnect attempt {}/{} failed, retrying in {:.1}s...",
                attempt,
                MAX_RECONNECT_ATTEMPTS,
                backoff.as_secs_f64()
            );
        }

        log::error!(
            "❌ WebSocket connection permanently lost after {} attempts. Network may be down or RPC endpoint unreachable.",
            MAX_RECONNECT_ATTEMPTS
        );
        log::warn!("⚠️  Falling back to RPC polling mode to continue operation...");
        Err(anyhow::anyhow!(
            "WebSocket connection permanently lost after {} reconnect attempts",
            MAX_RECONNECT_ATTEMPTS
        ))
    }
}
