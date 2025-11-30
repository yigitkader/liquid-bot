use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub address: Pubkey,
    pub health_factor: f64,
    pub collateral_usd: f64,
    pub debt_usd: f64,
    pub collateral_assets: Vec<Asset>,
    pub debt_assets: Vec<Asset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub mint: Pubkey,
    pub amount: u64,
    pub amount_usd: f64,
    pub ltv: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    pub position: Position,
    pub max_liquidatable: u64,
    pub seizable_collateral: u64,
    pub estimated_profit: f64,
    pub debt_mint: Pubkey,
    pub collateral_mint: Pubkey,
}

