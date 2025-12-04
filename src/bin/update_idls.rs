use anyhow::{Context, Result};
use clap::Parser;
use liquid_bot::core::registry::ProgramIds;
use log;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

// IDL dosyaları için helper struct (sadece bu binary'de kullanılıyor)
struct IdlFiles;

impl IdlFiles {
    fn solend() -> PathBuf {
        PathBuf::from("idl/solend.json")
    }
    
    fn solend_exists() -> bool { Self::solend().exists() }
    fn pyth() -> PathBuf { PathBuf::from("idl/pyth.json") }
    fn pyth_exists() -> bool { Self::pyth().exists() }
    fn switchboard() -> PathBuf { PathBuf::from("idl/switchboard.json") }
    fn switchboard_exists() -> bool { Self::switchboard().exists() }
    
    fn check_all() -> (bool, bool, bool) {
        (Self::solend_exists(), Self::pyth_exists(), Self::switchboard_exists())
    }
}

// IDL kaynak URL'leri ve çekme fonksiyonları (sadece bu binary'de kullanılıyor)
struct IdlSources;

impl IdlSources {
    const SOLEND_GITHUB: &'static str = "https://raw.githubusercontent.com/solendprotocol/solend-program/master/idl/solend_program.json";
    const PYTH_GITHUB: &'static str = "https://raw.githubusercontent.com/pyth-network/pyth-solana-program/main/idl/pyth_solana_receiver_v2.json";
    const SWITCHBOARD_GITHUB: &'static str = "https://raw.githubusercontent.com/switchboard-xyz/switchboard-v2/main/programs/aggregator/program-idl.json";
    
    async fn fetch_solend(force: bool) -> Result<()> {
        Self::fetch_idl(Self::SOLEND_GITHUB, IdlFiles::solend(), "Solend", force).await
    }
    
    async fn fetch_pyth(force: bool) -> Result<()> {
        Self::fetch_idl(Self::PYTH_GITHUB, IdlFiles::pyth(), "Pyth", force).await
    }
    
    async fn fetch_switchboard(force: bool) -> Result<()> {
        Self::fetch_idl(Self::SWITCHBOARD_GITHUB, IdlFiles::switchboard(), "Switchboard", force).await
    }
    
    async fn fetch_idl(url: &str, path: PathBuf, name: &str, force: bool) -> Result<()> {
        if !force && path.exists() {
            log::info!("⏭️  {} IDL already exists at {:?} (use force=true to update)", name, path);
            return Ok(());
        }
        
        log::info!("Fetching {} IDL from GitHub...", name);
        let response = reqwest::get(url).await.with_context(|| format!("Failed to fetch {} IDL", name))?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Failed to fetch {} IDL: HTTP {}", name, response.status()));
        }
        let content = response.text().await.with_context(|| format!("Failed to read {} IDL content", name))?;
        fs::create_dir_all("idl").context("Failed to create idl directory")?;
        fs::write(&path, content.as_bytes()).with_context(|| format!("Failed to write {} IDL to {:?}", name, path))?;
        log::info!("✅ {} IDL saved to {:?}", name, path);
        Ok(())
    }
    
    async fn fetch_with_anchor_cli(program_id: &str, output_path: &PathBuf, force: bool) -> Result<()> {
        if !force && output_path.exists() {
            log::info!("⏭️  IDL already exists at {:?} (use force=true to update)", output_path);
            return Ok(());
        }
        
        log::info!("Fetching IDL using Anchor CLI for program: {}", program_id);
        
        if !CliTools::is_anchor_cli_available() {
            return Err(anyhow::anyhow!(
                "Anchor CLI not found. Please install: cargo install --git https://github.com/coral-xyz/anchor avm && avm install latest && avm use latest"
            ));
        }
        
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).context("Failed to create IDL directory")?;
        }
        
        let output = Command::new(CliTools::ANCHOR_CLI)
            .args(&[
                "idl", "fetch", program_id,
                "--provider.cluster", "mainnet",
                "--out", output_path.to_str().unwrap(),
            ])
            .output()
            .context("Failed to execute Anchor CLI")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Anchor CLI failed: {}", stderr));
        }
        
        log::info!("✅ IDL fetched successfully using Anchor CLI: {:?}", output_path);
        Ok(())
    }
}

// CLI araçları için helper struct (sadece bu binary'de kullanılıyor)
struct CliTools;

impl CliTools {
    const ANCHOR_CLI: &'static str = "anchor";
    
    fn is_anchor_cli_available() -> bool {
        std::process::Command::new(Self::ANCHOR_CLI)
            .arg("--version")
            .output()
            .is_ok()
    }
}

#[derive(Parser, Debug)]
#[command(name = "update_idls")]
#[command(about = "Fetch and update IDL files from official sources. Uses Anchor CLI by default, falls back to GitHub if Anchor CLI is not available.")]
struct Args {
    /// Force update even if IDL files already exist
    #[arg(short, long)]
    force: bool,
    
    /// Update only Solend IDL
    #[arg(long)]
    solend_only: bool,
    
    /// Update only Pyth IDL
    #[arg(long)]
    pyth_only: bool,
    
    /// Update only Switchboard IDL
    #[arg(long)]
    switchboard_only: bool,
    
    /// Use GitHub sources instead of Anchor CLI (fallback method)
    #[arg(long)]
    use_github: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .init();
    
    let args = Args::parse();
    
    log::info!("🔄 Starting IDL update process...");
    log::info!("   Force mode: {}", args.force);
    
    let update_all = !args.solend_only && !args.pyth_only && !args.switchboard_only;
    let do_solend = args.solend_only || update_all;
    let do_pyth = args.pyth_only || update_all;
    let do_switchboard = args.switchboard_only || update_all;
    
    let use_anchor_cli = !args.use_github && CliTools::is_anchor_cli_available();
    
    if use_anchor_cli {
        log::info!("📦 Using Anchor CLI for IDL fetching (recommended method)...");
        
        if do_solend {
            log::info!("Fetching Solend IDL using Anchor CLI...");
            match IdlSources::fetch_with_anchor_cli(ProgramIds::SOLEND, &IdlFiles::solend(), args.force).await {
                Ok(_) => log::info!("✅ Solend IDL fetched successfully"),
                Err(e) => {
                    log::error!("❌ Failed to fetch Solend IDL with Anchor CLI: {}", e);
                    log::info!("   Falling back to GitHub...");
                    IdlSources::fetch_solend(args.force).await
                        .context("Failed to fetch Solend IDL from GitHub (fallback)")?;
                }
            }
        }
        
        if do_pyth {
            log::info!("Fetching Pyth IDL using Anchor CLI...");
            match IdlSources::fetch_with_anchor_cli(ProgramIds::PYTH, &IdlFiles::pyth(), args.force).await {
                Ok(_) => log::info!("✅ Pyth IDL fetched successfully"),
                Err(e) => {
                    log::warn!("⚠️  Failed to fetch Pyth IDL with Anchor CLI: {} (optional)", e);
                    log::info!("   Falling back to GitHub...");
                    if let Err(gh_err) = IdlSources::fetch_pyth(args.force).await {
                        log::warn!("⚠️  Failed to fetch Pyth IDL from GitHub: {} (optional, skipping)", gh_err);
                    }
                }
            }
        }
        
        if do_switchboard {
            log::info!("Fetching Switchboard IDL using Anchor CLI...");
            match IdlSources::fetch_with_anchor_cli(ProgramIds::SWITCHBOARD, &IdlFiles::switchboard(), args.force).await {
                Ok(_) => log::info!("✅ Switchboard IDL fetched successfully"),
                Err(e) => {
                    log::warn!("⚠️  Failed to fetch Switchboard IDL with Anchor CLI: {} (optional)", e);
                    log::info!("   Falling back to GitHub...");
                    if let Err(gh_err) = IdlSources::fetch_switchboard(args.force).await {
                        log::warn!("⚠️  Failed to fetch Switchboard IDL from GitHub: {} (optional, skipping)", gh_err);
                    }
                }
            }
        }
    } else {
        if args.use_github {
            log::info!("🌐 Using GitHub sources for IDL fetching (fallback method)...");
        } else {
            log::warn!("⚠️  Anchor CLI not found, falling back to GitHub sources...");
            log::info!("   Install Anchor CLI for better reliability: cargo install --git https://github.com/coral-xyz/anchor avm && avm install latest && avm use latest");
            log::info!("🌐 Using GitHub sources for IDL fetching...");
        }
        
        if do_solend {
            log::info!("Fetching Solend IDL...");
            IdlSources::fetch_solend(args.force).await
                .context("Failed to fetch Solend IDL")?;
        }
        
        if do_pyth {
            log::info!("Fetching Pyth IDL...");
            IdlSources::fetch_pyth(args.force).await
                .context("Failed to fetch Pyth IDL")?;
        }
        
        if do_switchboard {
            log::info!("Fetching Switchboard IDL...");
            IdlSources::fetch_switchboard(args.force).await
                .context("Failed to fetch Switchboard IDL")?;
        }
    }
    
    log::info!("✅ IDL update completed successfully!");
    
    let (solend_exists, pyth_exists, switchboard_exists) = IdlFiles::check_all();
    
    log::info!("📊 IDL Status:");
    log::info!("   Solend: {}", if solend_exists { "✅" } else { "❌" });
    log::info!("   Pyth: {}", if pyth_exists { "✅" } else { "❌" });
    log::info!("   Switchboard: {}", if switchboard_exists { "✅" } else { "❌" });
    
    Ok(())
}
