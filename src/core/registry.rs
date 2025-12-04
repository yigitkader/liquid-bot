//! Merkezi Registry Modülü
//! 
//! Bu modül projedeki tüm bağımlılıkları (program ID'leri, mint adresleri, 
//! reserve adresleri, IDL dosyaları) merkezi bir yerden yönetir.
//! 
//! Bu sayede:
//! - Hardcoded değerler tek bir yerden yönetilir
//! - Değişiklikler kolayca yapılabilir
//! - Versiyon kontrolü ve güncellemeler daha kolay olur
//! - Kod tekrarı azalır

use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

/// Program ID'leri için registry
pub struct ProgramIds;

impl ProgramIds {
    /// Solend Program ID (Mainnet)
    pub const SOLEND: &'static str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
    
    /// Pyth Network Program ID (Mainnet)
    pub const PYTH: &'static str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH";
    
    /// Switchboard Program ID (Mainnet)
    pub const SWITCHBOARD: &'static str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";
    
    /// Associated Token Program ID
    pub const ASSOCIATED_TOKEN: &'static str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
    
    /// Standard SPL Token Program ID (spl_token::id() kullanılabilir ama burada da tutuyoruz)
    pub const TOKEN: &'static str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
    
    /// Token-2022 Program ID (Token Extensions)
    pub const TOKEN_2022: &'static str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
    
    /// Solend Program ID'yi Pubkey olarak döndürür
    pub fn solend() -> Result<Pubkey> {
        Pubkey::from_str(Self::SOLEND)
            .context("Failed to parse Solend program ID")
    }
    
    /// Pyth Program ID'yi Pubkey olarak döndürür
    pub fn pyth() -> Result<Pubkey> {
        Pubkey::from_str(Self::PYTH)
            .context("Failed to parse Pyth program ID")
    }
    
    /// Switchboard Program ID'yi Pubkey olarak döndürür
    pub fn switchboard() -> Result<Pubkey> {
        Pubkey::from_str(Self::SWITCHBOARD)
            .context("Failed to parse Switchboard program ID")
    }
    
    /// Associated Token Program ID'yi Pubkey olarak döndürür
    pub fn associated_token() -> Result<Pubkey> {
        Pubkey::from_str(Self::ASSOCIATED_TOKEN)
            .context("Failed to parse Associated Token program ID")
    }
    
    /// Token-2022 Program ID'yi Pubkey olarak döndürür
    pub fn token_2022() -> Result<Pubkey> {
        Pubkey::from_str(Self::TOKEN_2022)
            .context("Failed to parse Token-2022 program ID")
    }
    
    /// Standard Token Program ID'yi Pubkey olarak döndürür
    /// Not: spl_token::id() kullanılabilir ama registry üzerinden erişim tutarlılık sağlar
    pub fn token() -> Result<Pubkey> {
        Pubkey::from_str(Self::TOKEN)
            .context("Failed to parse Token program ID")
    }
}

/// Mint adresleri için registry
pub struct MintAddresses;

impl MintAddresses {
    /// USDC Mint (Mainnet)
    pub const USDC: &'static str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    
    /// SOL Mint (Wrapped SOL)
    pub const SOL: &'static str = "So11111111111111111111111111111111111111112";
    
    /// USDT Mint (Mainnet)
    pub const USDT: &'static str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
    
    /// ETH Mint (Wrapped ETH)
    pub const ETH: &'static str = "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs";
    
    /// BTC Mint (Wrapped BTC)
    pub const BTC: &'static str = "9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E";
    
    /// DAI Mint
    pub const DAI: &'static str = "EjmyN6qEC1Tf1JxiG1ae7UTJhUxSwk1TCWNWqxWV4J6o";
    
    /// FRAX Mint
    pub const FRAX: &'static str = "FR87nWEUxVgerFGhZM8Y4AggKGLnaXswr1Pd8wZ4kZcp";
    
    /// UST Mint (TerraUSD)
    pub const UST: &'static str = "9vMJfxuKxXBoEa7rM12mYLMwTacLMLDJqHozw96WQL8i";
    
    /// BUSD Mint
    pub const BUSD: &'static str = "AZsHEMXd36Bj1EMNXhowJajpUXzrKcK57wW4ZGXVa7yR";
    
    /// TUSD Mint
    pub const TUSD: &'static str = "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R";
    
    /// USDP Mint (Pax Dollar)
    pub const USDP: &'static str = "EchesyfXePKdLbiHRbgTbYq4qP8zF8LzF6S9X5YJ7KzN";
    
    /// USDC Mint'ini Pubkey olarak döndürür
    pub fn usdc() -> Result<Pubkey> {
        Pubkey::from_str(Self::USDC)
            .context("Failed to parse USDC mint address")
    }
    
    /// SOL Mint'ini Pubkey olarak döndürür
    pub fn sol() -> Result<Pubkey> {
        Pubkey::from_str(Self::SOL)
            .context("Failed to parse SOL mint address")
    }
    
    /// USDT Mint'ini Pubkey olarak döndürür
    pub fn usdt() -> Result<Pubkey> {
        Pubkey::from_str(Self::USDT)
            .context("Failed to parse USDT mint address")
    }
    
    /// ETH Mint'ini Pubkey olarak döndürür
    pub fn eth() -> Result<Pubkey> {
        Pubkey::from_str(Self::ETH)
            .context("Failed to parse ETH mint address")
    }
    
    /// BTC Mint'ini Pubkey olarak döndürür
    pub fn btc() -> Result<Pubkey> {
        Pubkey::from_str(Self::BTC)
            .context("Failed to parse BTC mint address")
    }
    
    /// Tüm stablecoin mint adreslerini döndürür
    pub fn stablecoins() -> Vec<&'static str> {
        vec![
            Self::USDC,
            Self::USDT,
            Self::DAI,
            Self::FRAX,
            Self::UST,
            Self::BUSD,
            Self::TUSD,
            Self::USDP,
        ]
    }
    
    /// Stablecoin mint adreslerini Pubkey HashSet olarak döndürür
    pub fn stablecoins_as_pubkeys() -> Result<std::collections::HashSet<Pubkey>> {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for mint_str in Self::stablecoins() {
            let pubkey = Pubkey::from_str(mint_str)
                .with_context(|| format!("Failed to parse stablecoin mint: {}", mint_str))?;
            set.insert(pubkey);
        }
        Ok(set)
    }
}

/// Reserve adresleri için registry
pub struct ReserveAddresses;

impl ReserveAddresses {
    /// USDC Reserve Address (Mainnet)
    pub const USDC: &'static str = "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw";
    
    /// SOL Reserve Address (Mainnet)
    pub const SOL: &'static str = "8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36";
    
    /// USDC Reserve adresini Pubkey olarak döndürür
    pub fn usdc() -> Result<Pubkey> {
        Pubkey::from_str(Self::USDC)
            .context("Failed to parse USDC reserve address")
    }
    
    /// SOL Reserve adresini Pubkey olarak döndürür
    pub fn sol() -> Result<Pubkey> {
        Pubkey::from_str(Self::SOL)
            .context("Failed to parse SOL reserve address")
    }
}

/// Lending Market adresleri için registry
pub struct LendingMarketAddresses;

impl LendingMarketAddresses {
    /// Main Lending Market Address (Mainnet)
    pub const MAIN: &'static str = "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";
    
    /// Main Lending Market adresini Pubkey olarak döndürür
    pub fn main() -> Result<Pubkey> {
        Pubkey::from_str(Self::MAIN)
            .context("Failed to parse main lending market address")
    }
}

/// IDL dosyaları için registry
/// 
/// Not: Şu anda sadece Solend IDL aktif olarak kullanılıyor.
/// Pyth ve Switchboard için SDK kullanıldığı için IDL'e ihtiyaç yok,
/// ancak gelecekte kullanım için burada tutuluyor.
pub struct IdlFiles;

impl IdlFiles {
    /// Solend IDL dosyasının path'ini döndürür
    /// 
    /// Bu IDL aktif olarak kullanılıyor:
    /// - Instruction account order'ı için referans
    /// - Account structure validation için
    pub fn solend() -> PathBuf {
        PathBuf::from("idl/solend.json")
    }
    
    /// Solend IDL dosyasının var olup olmadığını kontrol eder
    pub fn solend_exists() -> bool {
        Self::solend().exists()
    }
    
    /// Pyth IDL dosyasının path'ini döndürür
    /// 
    /// Not: Şu anda kullanılmıyor - pyth-sdk-solana SDK kullanılıyor.
    /// Gelecekte Anchor IDL parsing için eklenebilir.
    pub fn pyth() -> PathBuf {
        PathBuf::from("idl/pyth.json")
    }
    
    /// Pyth IDL dosyasının var olup olmadığını kontrol eder
    pub fn pyth_exists() -> bool {
        Self::pyth().exists()
    }
    
    /// Switchboard IDL dosyasının path'ini döndürür
    /// 
    /// Not: Şu anda kullanılmıyor - switchboard-on-demand SDK kullanılıyor.
    /// Kodda "Full SDK integration would require Anchor IDL parsing" notu var.
    /// Gelecekte tam entegrasyon için eklenebilir.
    pub fn switchboard() -> PathBuf {
        PathBuf::from("idl/switchboard.json")
    }
    
    /// Switchboard IDL dosyasının var olup olmadığını kontrol eder
    pub fn switchboard_exists() -> bool {
        Self::switchboard().exists()
    }
    
    /// Tüm IDL dosyalarının var olup olmadığını kontrol eder
    /// 
    /// Returns: (solend_exists, pyth_exists, switchboard_exists)
    pub fn check_all() -> (bool, bool, bool) {
        (
            Self::solend_exists(),
            Self::pyth_exists(),
            Self::switchboard_exists(),
        )
    }
    
    /// Eksik IDL dosyalarını listeler
    pub fn missing_idls() -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !Self::solend_exists() {
            missing.push("solend.json");
        }
        // Pyth ve Switchboard opsiyonel olduğu için eksik listesine eklenmiyor
        // Ancak gelecekte gerekirse buraya eklenebilir
        missing
    }
}

/// IDL kaynak URL'leri ve çekme fonksiyonları
pub struct IdlSources;

impl IdlSources {
    /// Solend IDL'in resmi GitHub URL'i
    /// 
    /// Not: Solend'in resmi IDL'i GitHub'da tutuluyor
    /// Anchor program IDL'ini çekmek için Anchor CLI kullanılabilir veya
    /// GitHub'dan direkt indirilebilir
    pub const SOLEND_GITHUB: &'static str = "https://raw.githubusercontent.com/solendprotocol/solend-program/master/idl/solend_program.json";
    
    /// Solend IDL'i Anchor program'dan çekmek için kullanılabilir
    /// anchor idl fetch <program_id> --provider.cluster mainnet
    pub const SOLEND_PROGRAM_ID: &'static str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
    
    /// Pyth IDL'in resmi kaynağı
    /// Pyth genellikle GitHub'da IDL'lerini tutar
    pub const PYTH_GITHUB: &'static str = "https://raw.githubusercontent.com/pyth-network/pyth-solana-program/main/idl/pyth_solana_receiver_v2.json";
    
    /// Switchboard IDL'in resmi kaynağı
    /// Switchboard V2 IDL'i
    pub const SWITCHBOARD_GITHUB: &'static str = "https://raw.githubusercontent.com/switchboard-xyz/switchboard-v2/main/programs/aggregator/program-idl.json";
    
    /// Solend IDL'i GitHub'dan çeker ve kaydeder
    /// 
    /// Returns: Ok(()) başarılı, Err(e) hata durumunda
    pub async fn fetch_solend() -> Result<()> {
        log::info!("Fetching Solend IDL from GitHub...");
        
        let response = reqwest::get(Self::SOLEND_GITHUB)
            .await
            .context("Failed to fetch Solend IDL from GitHub")?;
        
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch Solend IDL: HTTP {}",
                response.status()
            ));
        }
        
        let idl_content = response.text().await
            .context("Failed to read Solend IDL content")?;
        
        // IDL dizinini oluştur
        fs::create_dir_all("idl")
            .context("Failed to create idl directory")?;
        
        // IDL'i kaydet
        let path = IdlFiles::solend();
        let mut file = fs::File::create(&path)
            .with_context(|| format!("Failed to create file: {:?}", path))?;
        
        file.write_all(idl_content.as_bytes())
            .with_context(|| format!("Failed to write IDL to {:?}", path))?;
        
        log::info!("✅ Solend IDL saved to {:?}", path);
        Ok(())
    }
    
    /// Pyth IDL'i GitHub'dan çeker ve kaydeder
    pub async fn fetch_pyth() -> Result<()> {
        log::info!("Fetching Pyth IDL from GitHub...");
        
        let response = reqwest::get(Self::PYTH_GITHUB)
            .await
            .context("Failed to fetch Pyth IDL from GitHub")?;
        
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch Pyth IDL: HTTP {}",
                response.status()
            ));
        }
        
        let idl_content = response.text().await
            .context("Failed to read Pyth IDL content")?;
        
        fs::create_dir_all("idl")
            .context("Failed to create idl directory")?;
        
        let path = IdlFiles::pyth();
        let mut file = fs::File::create(&path)
            .with_context(|| format!("Failed to create file: {:?}", path))?;
        
        file.write_all(idl_content.as_bytes())
            .with_context(|| format!("Failed to write IDL to {:?}", path))?;
        
        log::info!("✅ Pyth IDL saved to {:?}", path);
        Ok(())
    }
    
    /// Switchboard IDL'i GitHub'dan çeker ve kaydeder
    pub async fn fetch_switchboard() -> Result<()> {
        log::info!("Fetching Switchboard IDL from GitHub...");
        
        let response = reqwest::get(Self::SWITCHBOARD_GITHUB)
            .await
            .context("Failed to fetch Switchboard IDL from GitHub")?;
        
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch Switchboard IDL: HTTP {}",
                response.status()
            ));
        }
        
        let idl_content = response.text().await
            .context("Failed to read Switchboard IDL content")?;
        
        fs::create_dir_all("idl")
            .context("Failed to create idl directory")?;
        
        let path = IdlFiles::switchboard();
        let mut file = fs::File::create(&path)
            .with_context(|| format!("Failed to create file: {:?}", path))?;
        
        file.write_all(idl_content.as_bytes())
            .with_context(|| format!("Failed to write IDL to {:?}", path))?;
        
        log::info!("✅ Switchboard IDL saved to {:?}", path);
        Ok(())
    }
    
    /// Tüm IDL'leri çeker ve günceller
    /// 
    /// force: true ise mevcut dosyaların üzerine yazar
    pub async fn fetch_all(force: bool) -> Result<()> {
        log::info!("🔄 Fetching all IDL files...");
        
        let mut results = Vec::new();
        
        // Solend IDL (zorunlu)
        match Self::fetch_solend().await {
            Ok(_) => {
                log::info!("✅ Solend IDL fetched successfully");
                results.push(("Solend", true));
            }
            Err(e) => {
                log::error!("❌ Failed to fetch Solend IDL: {}", e);
                results.push(("Solend", false));
            }
        }
        
        // Pyth IDL (opsiyonel)
        if force || !IdlFiles::pyth_exists() {
            match Self::fetch_pyth().await {
                Ok(_) => {
                    log::info!("✅ Pyth IDL fetched successfully");
                    results.push(("Pyth", true));
                }
                Err(e) => {
                    log::warn!("⚠️  Failed to fetch Pyth IDL: {} (optional)", e);
                    results.push(("Pyth", false));
                }
            }
        } else {
            log::info!("⏭️  Skipping Pyth IDL (already exists, use force=true to update)");
            results.push(("Pyth", true));
        }
        
        // Switchboard IDL (opsiyonel)
        if force || !IdlFiles::switchboard_exists() {
            match Self::fetch_switchboard().await {
                Ok(_) => {
                    log::info!("✅ Switchboard IDL fetched successfully");
                    results.push(("Switchboard", true));
                }
                Err(e) => {
                    log::warn!("⚠️  Failed to fetch Switchboard IDL: {} (optional)", e);
                    results.push(("Switchboard", false));
                }
            }
        } else {
            log::info!("⏭️  Skipping Switchboard IDL (already exists, use force=true to update)");
            results.push(("Switchboard", true));
        }
        
        let success_count = results.iter().filter(|(_, success)| *success).count();
        log::info!("📊 IDL fetch summary: {}/{} successful", success_count, results.len());
        
        // Solend başarısız olursa hata döndür
        if !results.iter().any(|(name, success)| name == &"Solend" && *success) {
            return Err(anyhow::anyhow!("Failed to fetch required Solend IDL"));
        }
        
        Ok(())
    }
    
    /// Anchor CLI kullanarak program IDL'ini çeker
    /// 
    /// Bu fonksiyon Anchor CLI'nin yüklü olmasını gerektirir
    /// anchor idl fetch <program_id> --provider.cluster mainnet
    pub async fn fetch_with_anchor_cli(program_id: &str, output_path: &PathBuf) -> Result<()> {
        log::info!("Fetching IDL using Anchor CLI for program: {}", program_id);
        
        // Anchor CLI kontrolü
        if !CliTools::is_anchor_cli_available() {
            return Err(anyhow::anyhow!(
                "Anchor CLI not found. Please install: cargo install --git https://github.com/coral-xyz/anchor avm && avm install latest && avm use latest"
            ));
        }
        
        // IDL dizinini oluştur
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create IDL directory")?;
        }
        
        // Anchor CLI komutunu çalıştır
        let output = Command::new(CliTools::ANCHOR_CLI)
            .args(&[
                "idl",
                "fetch",
                program_id,
                "--provider.cluster",
                "mainnet",
                "--file",
                output_path.to_str().unwrap(),
            ])
            .output()
            .context("Failed to execute Anchor CLI")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "Anchor CLI failed: {}",
                stderr
            ));
        }
        
        log::info!("✅ IDL fetched successfully using Anchor CLI: {:?}", output_path);
        Ok(())
    }
}

/// Solana SDK versiyonları için registry
/// Not: Bu bilgiler Cargo.toml'da da tutulur, burada referans amaçlıdır
/// Cargo.toml güncellendiğinde burayı da güncellemeyi unutmayın!
pub struct SolanaSdkVersions;

impl SolanaSdkVersions {
    /// Solana SDK versiyonu (Cargo.toml ile senkronize tutulmalı)
    pub const SOLANA_SDK: &'static str = "1.18";
    
    /// Solana Client versiyonu
    pub const SOLANA_CLIENT: &'static str = "1.18";
    
    /// Solana Program versiyonu
    pub const SOLANA_PROGRAM: &'static str = "1.18";
    
    /// Solana Account Decoder versiyonu
    pub const SOLANA_ACCOUNT_DECODER: &'static str = "1.18";
    
    /// Anchor Lang versiyonu
    pub const ANCHOR_LANG: &'static str = "0.29";
    
    /// Anchor Client versiyonu
    pub const ANCHOR_CLIENT: &'static str = "0.29";
    
    /// SPL Token versiyonu
    pub const SPL_TOKEN: &'static str = "4.0";
    
    /// SPL Token 2022 versiyonu
    pub const SPL_TOKEN_2022: &'static str = "1.0";
    
    /// SPL Associated Token Account versiyonu
    pub const SPL_ASSOCIATED_TOKEN_ACCOUNT: &'static str = "2.3";
    
    /// Pyth SDK Solana versiyonu
    pub const PYTH_SDK_SOLANA: &'static str = "0.10";
    
    /// Switchboard On-Demand versiyonu
    pub const SWITCHBOARD_ON_DEMAND: &'static str = "0.11";
}

/// CLI araçları için registry
/// Bu araçlar sistemde yüklü olması gereken komut satırı araçlarıdır
pub struct CliTools;

impl CliTools {
    /// Solana CLI komut adı
    pub const SOLANA_CLI: &'static str = "solana";
    
    /// Anchor CLI komut adı
    pub const ANCHOR_CLI: &'static str = "anchor";
    
    /// SPL Token CLI komut adı (spl-token)
    pub const SPL_TOKEN_CLI: &'static str = "spl-token";
    
    /// Cargo komut adı
    pub const CARGO: &'static str = "cargo";
    
    /// Solana CLI önerilen versiyonu (semver formatında)
    /// Not: Bu versiyon SDK versiyonu ile uyumlu olmalıdır
    pub const SOLANA_CLI_VERSION: &'static str = "1.18";
    
    /// Anchor CLI önerilen versiyonu (semver formatında)
    /// Not: Bu versiyon Anchor SDK versiyonu ile uyumlu olmalıdır
    pub const ANCHOR_CLI_VERSION: &'static str = "0.29";
    
    /// SPL Token CLI önerilen versiyonu (semver formatında)
    /// Not: Bu versiyon SPL Token SDK versiyonu ile uyumlu olmalıdır
    pub const SPL_TOKEN_CLI_VERSION: &'static str = "4.0";
    
    /// Solana CLI'nin yüklü olup olmadığını kontrol eder
    /// Script'lerde kullanılabilir: `if CliTools::is_solana_cli_available() { ... }`
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_solana_cli_available() -> bool {
        std::process::Command::new(Self::SOLANA_CLI)
            .arg("--version")
            .output()
            .is_ok()
    }
    
    /// Anchor CLI'nin yüklü olup olmadığını kontrol eder
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_anchor_cli_available() -> bool {
        std::process::Command::new(Self::ANCHOR_CLI)
            .arg("--version")
            .output()
            .is_ok()
    }
    
    /// SPL Token CLI'nin yüklü olup olmadığını kontrol eder
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_spl_token_cli_available() -> bool {
        std::process::Command::new(Self::SPL_TOKEN_CLI)
            .arg("--version")
            .output()
            .is_ok()
    }
    
    /// Cargo'nun yüklü olup olmadığını kontrol eder
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_cargo_available() -> bool {
        std::process::Command::new(Self::CARGO)
            .arg("--version")
            .output()
            .is_ok()
    }
    
    /// Tüm gerekli CLI araçlarının yüklü olup olmadığını kontrol eder
    /// Script'lerde kullanılabilir
    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_all_cli_tools() -> Vec<(&'static str, bool)> {
        vec![
            (Self::SOLANA_CLI, Self::is_solana_cli_available()),
            (Self::ANCHOR_CLI, Self::is_anchor_cli_available()),
            (Self::SPL_TOKEN_CLI, Self::is_spl_token_cli_available()),
            (Self::CARGO, Self::is_cargo_available()),
        ]
    }
}

/// CLI komut şablonları için registry
/// Yaygın kullanılan CLI komutlarını merkezi bir yerden yönetir
pub mod cli_commands {
    use super::CliTools;
    
    /// Solana CLI komutları
    pub struct Solana;
    
    impl Solana {
        /// Wallet adresini almak için komut
        pub fn get_address(wallet_path: &str) -> String {
            format!("{} address -k {}", CliTools::SOLANA_CLI, wallet_path)
        }
        
        /// Wallet bakiyesini almak için komut
        pub fn get_balance(address: &str) -> String {
            format!("{} balance {}", CliTools::SOLANA_CLI, address)
        }
        
        /// Account bilgisini almak için komut
        pub fn get_account(address: &str, rpc_url: Option<&str>) -> String {
            if let Some(url) = rpc_url {
                format!("{} account {} --url {}", CliTools::SOLANA_CLI, address, url)
            } else {
                format!("{} account {}", CliTools::SOLANA_CLI, address)
            }
        }
        
        /// Program account'larını almak için komut
        pub fn get_program_accounts(program_id: &str, rpc_url: Option<&str>) -> String {
            if let Some(url) = rpc_url {
                format!("{} program show {} --url {}", CliTools::SOLANA_CLI, program_id, url)
            } else {
                format!("{} program show {}", CliTools::SOLANA_CLI, program_id)
            }
        }
    }
    
    /// Anchor CLI komutları
    pub struct Anchor;
    
    impl Anchor {
        /// Anchor projesi oluşturmak için komut
        pub fn new(project_name: &str) -> String {
            format!("{} new {}", CliTools::ANCHOR_CLI, project_name)
        }
        
        /// Anchor projesi build etmek için komut
        pub fn build() -> String {
            format!("{} build", CliTools::ANCHOR_CLI)
        }
        
        /// Anchor projesi deploy etmek için komut
        pub fn deploy() -> String {
            format!("{} deploy", CliTools::ANCHOR_CLI)
        }
        
        /// IDL dosyasını güncellemek için komut
        pub fn idl_update(idl_path: &str) -> String {
            format!("{} idl update --filepath {}", CliTools::ANCHOR_CLI, idl_path)
        }
    }
    
    /// SPL Token CLI komutları
    pub struct SplToken;
    
    impl SplToken {
        /// Token account oluşturmak için komut
        pub fn create_account(mint: &str, owner: Option<&str>) -> String {
            if let Some(owner_addr) = owner {
                format!("{} create-account {} --owner {}", CliTools::SPL_TOKEN_CLI, mint, owner_addr)
            } else {
                format!("{} create-account {}", CliTools::SPL_TOKEN_CLI, mint)
            }
        }
        
        /// Token transfer yapmak için komut
        pub fn transfer(source: &str, destination: &str, amount: &str) -> String {
            format!("{} transfer {} {} {}", CliTools::SPL_TOKEN_CLI, source, destination, amount)
        }
        
        /// Token balance kontrol etmek için komut
        pub fn balance(token_account: &str) -> String {
            format!("{} balance {}", CliTools::SPL_TOKEN_CLI, token_account)
        }
    }
    
    /// Cargo komutları
    pub struct Cargo;
    
    impl Cargo {
        /// Cargo build komutu
        pub fn build(release: bool) -> String {
            if release {
                format!("{} build --release", CliTools::CARGO)
            } else {
                format!("{} build", CliTools::CARGO)
            }
        }
        
        /// Cargo test komutu
        pub fn test() -> String {
            format!("{} test", CliTools::CARGO)
        }
        
        /// Cargo run komutu (binary ile)
        pub fn run_bin(bin_name: &str) -> String {
            format!("{} run --bin {}", CliTools::CARGO, bin_name)
        }
        
        /// Cargo check komutu
        pub fn check() -> String {
            format!("{} check", CliTools::CARGO)
        }
    }
}

/// CLI komutlarına kolay erişim için re-export
pub use cli_commands::{Solana as SolanaCli, Anchor as AnchorCli, SplToken as SplTokenCli, Cargo as CargoCli};

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_program_ids_parse() {
        assert!(ProgramIds::solend().is_ok());
        assert!(ProgramIds::pyth().is_ok());
        assert!(ProgramIds::switchboard().is_ok());
        assert!(ProgramIds::associated_token().is_ok());
        assert!(ProgramIds::token_2022().is_ok());
    }
    
    #[test]
    fn test_mint_addresses_parse() {
        assert!(MintAddresses::usdc().is_ok());
        assert!(MintAddresses::sol().is_ok());
        assert!(MintAddresses::usdt().is_ok());
        assert!(MintAddresses::eth().is_ok());
        assert!(MintAddresses::btc().is_ok());
    }
    
    #[test]
    fn test_reserve_addresses_parse() {
        assert!(ReserveAddresses::usdc().is_ok());
        assert!(ReserveAddresses::sol().is_ok());
    }
    
    #[test]
    fn test_lending_market_addresses_parse() {
        assert!(LendingMarketAddresses::main().is_ok());
    }
    
    #[test]
    fn test_stablecoins_parse() {
        let stablecoins = MintAddresses::stablecoins_as_pubkeys().unwrap();
        assert!(!stablecoins.is_empty());
        assert!(stablecoins.contains(&MintAddresses::usdc().unwrap()));
        assert!(stablecoins.contains(&MintAddresses::usdt().unwrap()));
    }
}

