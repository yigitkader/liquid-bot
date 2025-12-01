use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use tokio::sync::Mutex;
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
    connection: Arc<Mutex<Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>>>,
    subscriptions: Arc<Mutex<HashMap<u64, SubscriptionInfo>>>,
    next_request_id: Arc<Mutex<u64>>,
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
        WsClient {
            url,
            connection: Arc::new(Mutex::new(None)),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            next_request_id: Arc::new(Mutex::new(1)),
        }
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
            stream.send(message).await
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
                                        return Err(anyhow::anyhow!("WebSocket RPC error: {}", error));
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
        
        let subscription_id = result.as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid subscription ID in response"))?;

        let mut subscriptions = self.subscriptions.lock().await;
        subscriptions.insert(subscription_id, SubscriptionInfo {
            subscription_type: SubscriptionType::Program(*program_id),
        });

        Ok(subscription_id)
    }

    pub async fn subscribe_account(&self, pubkey: &Pubkey) -> Result<u64> {
        let params = json!({
            "account": pubkey.to_string(),
            "encoding": "base64",
            "commitment": "confirmed"
        });

        let request_id = self.send_request("accountSubscribe", params).await?;
        let result = self.wait_for_response(request_id).await?;
        
        let subscription_id = result.as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid subscription ID in response"))?;

        let mut subscriptions = self.subscriptions.lock().await;
        subscriptions.insert(subscription_id, SubscriptionInfo {
            subscription_type: SubscriptionType::Account(*pubkey),
        });

        Ok(subscription_id)
    }

    pub async fn subscribe_slot(&self) -> Result<u64> {
        let params = json!([]);

        let request_id = self.send_request("slotSubscribe", params).await?;
        let result = self.wait_for_response(request_id).await?;
        
        let subscription_id = result.as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid subscription ID in response"))?;

        let mut subscriptions = self.subscriptions.lock().await;
        subscriptions.insert(subscription_id, SubscriptionInfo {
            subscription_type: SubscriptionType::Slot,
        });

        Ok(subscription_id)
    }

    pub async fn listen(&self) -> Option<AccountUpdate> {
        let mut conn = self.connection.lock().await;
        if let Some(ref mut stream) = *conn {
            if let Some(Ok(msg)) = stream.next().await {
                match msg {
                    Message::Text(text) => {
                        if let Ok(notification) = serde_json::from_str::<serde_json::Value>(&text) {
                            if notification.get("method") == Some(&json!("programNotification")) ||
                               notification.get("method") == Some(&json!("accountNotification")) {
                                if let Some(params) = notification.get("params") {
                                        if let Some(result) = params.get("result") {
                                            if let Some(value) = result.get("value") {
                                            if let Some(account_data) = value.get("data") {
                                                if let Some(data_array) = account_data.as_array() {
                                                    if data_array.len() >= 2 {
                                                        if let Some(base64_data) = data_array[0].as_str() {
                                                            use base64::{Engine as _, engine::general_purpose};
                                                            if let Ok(decoded) = general_purpose::STANDARD.decode(base64_data) {
                                                                let pubkey_str = value.get("owner")
                                                                    .and_then(|v| v.as_str())
                                                                    .unwrap_or("");
                                                                let pubkey = pubkey_str.parse::<Pubkey>().ok();
                                                                let slot = value.get("lamports")
                                                                    .and_then(|v| v.as_u64())
                                                                    .unwrap_or(0);
                                                                
                                                                if let Some(pk) = pubkey {
                                                                    let account = solana_sdk::account::Account {
                                                                        lamports: slot,
                                                                        data: decoded,
                                                                        owner: pk,
                                                                        executable: false,
                                                                        rent_epoch: 0,
                                                                    };
                                                                    
                                                                    return Some(AccountUpdate {
                                                                        pubkey: pk,
                                                                        account,
                                                                        slot,
                                                                    });
                                                                }
                                                            }
                                                        }
                                                    }
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
        }
        None
    }

    pub async fn reconnect_with_backoff(&self) {
        let mut backoff = Duration::from_secs(1);
        loop {
            sleep(backoff).await;
            if self.connect().await.is_ok() {
                let subscriptions = self.subscriptions.lock().await;
                for (_, info) in subscriptions.iter() {
                    match &info.subscription_type {
                        SubscriptionType::Program(program_id) => {
                            if let Err(e) = self.subscribe_program(program_id).await {
                                log::warn!("Failed to resubscribe to program {}: {}", program_id, e);
                            }
                        }
                        SubscriptionType::Account(pubkey) => {
                            if let Err(e) = self.subscribe_account(pubkey).await {
                                log::warn!("Failed to resubscribe to account {}: {}", pubkey, e);
                            }
                        }
                        SubscriptionType::Slot => {
                            if let Err(e) = self.subscribe_slot().await {
                                log::warn!("Failed to resubscribe to slot: {}", e);
                            }
                        }
                    }
                }
                break;
            }
            backoff = backoff.saturating_mul(2);
            if backoff > Duration::from_secs(60) {
                backoff = Duration::from_secs(60);
            }
        }
    }
}
