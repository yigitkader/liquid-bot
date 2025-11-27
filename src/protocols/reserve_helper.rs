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

    // todo:
    // Solend reserve account'unu parse et
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

    //todo:
    // Borrow rate'i hesapla
    // Gerçek borrow rate'i hesaplamak için utilization rate'e göre config'deki rate'leri kullanmalıyız
    // Şu an optimal borrow rate'i kullanıyoruz (basit yaklaşım)
    // Gerçek implementasyonda utilization rate'e göre min/optimal/max arasında interpolasyon yapılmalı
    let borrow_rate = reserve.config.optimal_borrow_rate as f64 / 100.0; // APY olarak (0.0 - 1.0)
    let mint = Some(liquidity_mint);
    let pyth_oracle = reserve.pyth_oracle();
    let switchboard_oracle = reserve.switchboard_oracle();

    let pyth_oracle = if pyth_oracle != Pubkey::default() {
        Some(pyth_oracle)
    } else {
        log::debug!("Pyth oracle is default (zero) for reserve {}", reserve_pubkey);
        None
    };
    
    let switchboard_oracle = if switchboard_oracle != Pubkey::default() {
        Some(switchboard_oracle)
    } else {
        log::debug!("Switchboard oracle is default (zero) for reserve {}", reserve_pubkey);
        None
    };

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

