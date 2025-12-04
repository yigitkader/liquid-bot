// WebSocket connection and reconnection management module

use anyhow::{Context, Result};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message, WebSocketStream, MaybeTlsStream};
use tokio::net::TcpStream;
use std::sync::Arc;
use tokio::sync::Mutex;
use futures_util::{SinkExt, StreamExt};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct ConnectionManager {
    url: String,
    connection: Arc<Mutex<Option<WsStream>>>,
}

impl ConnectionManager {
    pub fn new(url: String) -> Self {
        Self {
            url,
            connection: Arc::new(Mutex::new(None)),
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

    pub async fn disconnect(&self) {
        let mut conn = self.connection.lock().await;
        *conn = None;
    }

    pub async fn is_connected(&self) -> bool {
        let conn = self.connection.lock().await;
        conn.is_some()
    }

    pub async fn reconnect_with_backoff(&self, max_attempts: usize) -> Result<()> {
        let mut backoff = Duration::from_secs(1);

        for attempt in 1..=max_attempts {
            sleep(backoff).await;

            log::info!(
                "WebSocket reconnect attempt {}/{}",
                attempt,
                max_attempts
            );

            if self.connect().await.is_ok() {
                log::info!(
                    "✅ WebSocket reconnected successfully on attempt {}",
                    attempt
                );
                return Ok(());
            }

            backoff = backoff.min(Duration::from_secs(30));
            backoff *= 2;
        }

        Err(anyhow::anyhow!(
            "Failed to reconnect after {} attempts",
            max_attempts
        ))
    }

    pub fn connection(&self) -> Arc<Mutex<Option<WsStream>>> {
        Arc::clone(&self.connection)
    }
}

