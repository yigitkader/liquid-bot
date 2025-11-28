use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub fn parse_pubkey(s: &str) -> Result<Pubkey> {
    Pubkey::from_str(s).with_context(|| format!("Invalid pubkey: {}", s))
}

pub fn parse_pubkey_opt(s: &str) -> Option<Pubkey> {
    Pubkey::from_str(s).ok()
}

