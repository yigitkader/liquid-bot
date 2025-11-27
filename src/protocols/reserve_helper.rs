use anyhow::{Context, Result};
use solana_sdk::{
    pubkey::Pubkey,
    account::Account,
};

// Solend reserve account struct'ını kullan
use crate::protocols::solend_reserve::SolendReserve;

#[derive(Debug, Clone)]
pub struct ReserveInfo {
    pub reserve_pubkey: Pubkey,
    pub mint: Option<Pubkey>,
    pub ltv: f64,
    pub borrow_rate: f64,
    pub liquidity_mint: Option<Pubkey>,
    pub collateral_mint: Option<Pubkey>,
    pub liquidity_supply: Option<Pubkey>,
    pub collateral_supply: Option<Pubkey>,
    pub liquidation_bonus: f64,
    pub pyth_oracle: Option<Pubkey>,
    pub switchboard_oracle: Option<Pubkey>,
}

pub async fn parse_reserve_account(
    reserve_pubkey: &Pubkey,
    account_data: &Account,
) -> Result<ReserveInfo> {

    // Parse Solend reserve account
    // ⚠️ PRODUCTION WARNING: Bu struct gerçek Solend IDL'ine göre doğrulanmamıştır!
    // Parse error alırsanız, gerçek IDL'den struct'ı güncelleyin: ./scripts/fetch_solend_idl.sh
    let reserve = SolendReserve::from_account_data(&account_data.data)
        .context(format!(
            "Failed to parse Solend reserve account {}. \
            This might indicate the struct structure doesn't match the real Solend IDL. \
            Please validate against official IDL: ./scripts/fetch_solend_idl.sh",
            reserve_pubkey
        ))?;

    let liquidity_mint = reserve.liquidity_mint();
    let collateral_mint = reserve.collateral_mint();
    let ltv = reserve.ltv();
    let liquidation_bonus = reserve.liquidation_bonus();

    // Calculate borrow rate based on utilization rate
    // Solend uses a piecewise linear function:
    // - If utilization < optimal: linear interpolation between min and optimal
    // - If utilization >= optimal: linear interpolation between optimal and max
    let utilization_rate = if reserve.liquidity.available_amount + (reserve.liquidity.borrowed_amount_wads / 1_000_000_000_000_000_000) as u64 > 0 {
        let total_liquidity = reserve.liquidity.available_amount + (reserve.liquidity.borrowed_amount_wads / 1_000_000_000_000_000_000) as u64;
        (reserve.liquidity.borrowed_amount_wads / 1_000_000_000_000_000_000) as f64 / total_liquidity as f64
    } else {
        0.0
    };
    
    let optimal_utilization = reserve.config.optimal_utilization_rate as f64 / 100.0;
    let min_rate = reserve.config.min_borrow_rate as f64 / 100.0;
    let optimal_rate = reserve.config.optimal_borrow_rate as f64 / 100.0;
    let max_rate = reserve.config.max_borrow_rate as f64 / 100.0;
    
    let borrow_rate = if utilization_rate < optimal_utilization {
        // Linear interpolation between min and optimal
        if optimal_utilization > 0.0 {
            min_rate + (optimal_rate - min_rate) * (utilization_rate / optimal_utilization)
        } else {
            min_rate
        }
    } else {
        // Linear interpolation between optimal and max
        let remaining_utilization = 1.0 - optimal_utilization;
        if remaining_utilization > 0.0 {
            let excess_utilization = utilization_rate - optimal_utilization;
            optimal_rate + (max_rate - optimal_rate) * (excess_utilization / remaining_utilization)
        } else {
            optimal_rate
        }
    };
    
    log::debug!(
        "Borrow rate calculation: utilization={:.2}%, optimal_util={:.2}%, rate={:.4}% (min={:.2}%, optimal={:.2}%, max={:.2}%)",
        utilization_rate * 100.0,
        optimal_utilization * 100.0,
        borrow_rate * 100.0,
        min_rate * 100.0,
        optimal_rate * 100.0,
        max_rate * 100.0
    );
    let mint = Some(liquidity_mint);
    let pyth_oracle_raw = reserve.pyth_oracle();
    let switchboard_oracle_raw = reserve.switchboard_oracle();
    
    // Solend'in gerçek kodunda oracle_option YOK!
    // Her iki oracle da account'ta var, hangisinin aktif olduğu program tarafından belirlenir
    // Default pubkey (111...111) olmayan oracle'ı aktif kabul ediyoruz
    let pyth_oracle = if pyth_oracle_raw != Pubkey::default() {
        Some(pyth_oracle_raw)
    } else {
        None
    };
    let switchboard_oracle = if switchboard_oracle_raw != Pubkey::default() {
        Some(switchboard_oracle_raw)
    } else {
        None
    };
    
    if pyth_oracle.is_none() && switchboard_oracle.is_none() {
        log::debug!("No oracle found for reserve {} (both are default)", reserve_pubkey);
    } else if pyth_oracle.is_some() && switchboard_oracle.is_some() {
        log::debug!("Both oracles present for reserve {}: pyth={}, switchboard={}", 
            reserve_pubkey, pyth_oracle.unwrap(), switchboard_oracle.unwrap());
    } else if pyth_oracle.is_some() {
        log::debug!("Only Pyth oracle present for reserve {}: {}", reserve_pubkey, pyth_oracle.unwrap());
    } else {
        log::debug!("Only Switchboard oracle present for reserve {}: {}", reserve_pubkey, switchboard_oracle.unwrap());
    }

    log::debug!(
        "Reserve {} oracles: pyth={:?}, switchboard={:?}",
        reserve_pubkey,
        pyth_oracle,
        switchboard_oracle
    );
    
    log::debug!(
        "Parsed reserve {}: mint={}, ltv={:.2}, borrow_rate={:.4}, liquidation_bonus={:.2}, pyth_oracle={:?}, switchboard_oracle={:?}",
        reserve_pubkey,
        liquidity_mint,
        ltv,
        borrow_rate,
        liquidation_bonus,
        pyth_oracle,
        switchboard_oracle
    );
    
    Ok(ReserveInfo {
        reserve_pubkey: *reserve_pubkey,
        mint,
        ltv,
        borrow_rate,
        liquidity_mint: Some(liquidity_mint),
        collateral_mint: Some(collateral_mint),
        liquidity_supply: Some(reserve.liquidity_supply()),
        collateral_supply: Some(reserve.collateral_supply()),
        liquidation_bonus,
        pyth_oracle,
        switchboard_oracle,
    })
}

pub async fn get_reserve_mint(
    _reserve_pubkey: &Pubkey,
    _rpc_client: Option<&crate::solana_client::SolanaClient>,
) -> Result<Option<Pubkey>> {
    Ok(None)
}

