// Helper utilities
use anyhow::Result;

pub fn parse_pubkey(s: &str) -> Result<solana_sdk::pubkey::Pubkey> {
    s.parse().map_err(|e| anyhow::anyhow!("Invalid pubkey: {}", e))
}

pub fn parse_pubkey_opt(s: &str) -> Option<solana_sdk::pubkey::Pubkey> {
    s.parse().ok()
}
