use include_dir::{include_dir, Dir};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

static DEMO_IPC: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/internal/demo.ipc");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Trace)
        .init();

    let home_dir = env::var("HOME").map_err(|_| "HOME environment variable not set")?;
    let ipc_dir = PathBuf::from(&home_dir).join(".ipc");

    // Check if $HOME/.ipc already exists
    if ipc_dir.exists() {
        println!("Directory {} already exists. Skipping.", ipc_dir.display());
    } else {
        // Extract the embedded demo.ipc directory to $HOME/.ipc
        extract_embedded_dir(&DEMO_IPC, &ipc_dir)?;

        // Process all .env files in the extracted directory
        process_env_files(&ipc_dir, &home_dir)?;

        println!(
            "Successfully extracted demo.ipc to {} and processed .env files",
            ipc_dir.display()
        );
    }

    // Setup Bitcoin wallets
    setup_bitcoin_wallets().await?;

    Ok(())
}

async fn setup_bitcoin_wallets() -> Result<(), Box<dyn std::error::Error>> {
    println!("Setting up Bitcoin wallets...");

    // Check if default wallet already exists
    let wallet_check = Command::new("bitcoin-cli")
        .args(&["--rpcwallet=default", "getwalletinfo"])
        .output();

    let wallets_exist = match wallet_check {
        Ok(output) => output.status.success(),
        Err(_) => false,
    };

    let wallet_names = vec![
        "default",
        "validator1",
        "validator2",
        "validator3",
        "validator4",
        "validator5",
        "validator6",
        "user1",
        "user2",
    ];

    if wallets_exist {
        println!("Default wallet exists, skipping wallet creation");

        // Load all wallets
        for wallet in &wallet_names {
            let output = Command::new("bitcoin-cli")
                .args(&["loadwallet", wallet])
                .output()?;

            if output.status.success() {
                println!("Loaded wallet: {}", wallet);
            } else {
                println!(
                    "Failed to load wallet {}: {}",
                    wallet,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    } else {
        println!("Creating Bitcoin wallets...");

        // Create wallets
        for wallet in &wallet_names {
            let output = Command::new("bitcoin-cli")
                .args(&["createwallet", wallet])
                .output()?;

            if output.status.success() {
                println!("Created wallet: {}", wallet);
            } else {
                println!(
                    "Failed to create wallet {}: {}",
                    wallet,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }

        println!("Funding wallets...");

        // Fund validator wallets
        for i in 1..=6 {
            let wallet_name = format!("validator{}", i);
            fund_wallet(&wallet_name, 2).await?;
        }

        // Fund user wallets
        fund_wallet("user1", 2).await?;
        fund_wallet("user2", 2).await?;

        // Generate blocks to default wallet (for mining rewards)
        fund_wallet("default", 102).await?;
    }

    // Check and print balances
    println!("Checking wallet balances...");
    for wallet in &wallet_names {
        check_wallet_balance(wallet).await?;
    }

    Ok(())
}

async fn fund_wallet(wallet_name: &str, blocks: u32) -> Result<(), Box<dyn std::error::Error>> {
    // Get new address for the wallet
    let address_output = Command::new("bitcoin-cli")
        .args(&[&format!("--rpcwallet={}", wallet_name), "getnewaddress"])
        .output()?;

    if !address_output.status.success() {
        println!(
            "Failed to get address for wallet {}: {}",
            wallet_name,
            String::from_utf8_lossy(&address_output.stderr)
        );
        return Ok(());
    }

    let address = String::from_utf8(address_output.stdout)?.trim().to_string();

    // Generate blocks to the address
    let generate_output = Command::new("bitcoin-cli")
        .args(&["generatetoaddress", &blocks.to_string(), &address])
        .output()?;

    if generate_output.status.success() {
        println!("Generated {} blocks to wallet {}", blocks, wallet_name);
    } else {
        println!(
            "Failed to generate blocks for wallet {}: {}",
            wallet_name,
            String::from_utf8_lossy(&generate_output.stderr)
        );
    }

    Ok(())
}

async fn check_wallet_balance(wallet_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("bitcoin-cli")
        .args(&[&format!("--rpcwallet={}", wallet_name), "getbalance"])
        .output()?;

    if output.status.success() {
        let balance = String::from_utf8(output.stdout)?.trim().to_string();
        println!("Wallet {:<12}\tbalance: {} BTC", wallet_name, balance);
    } else {
        println!(
            "Failed to get balance for wallet {}: {}",
            wallet_name,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

fn extract_embedded_dir(dir: &Dir, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;

    // Extract all files in this directory
    for file in dir.files() {
        let file_name = file.path().file_name().unwrap();
        let file_path = dst.join(file_name);
        fs::write(&file_path, file.contents())?;
    }

    // Recursively extract subdirectories
    for subdir in dir.dirs() {
        let subdir_name = subdir.path().file_name().unwrap();
        let subdir_path = dst.join(subdir_name);
        extract_embedded_dir(subdir, &subdir_path)?;
    }

    Ok(())
}

fn process_env_files(dir: &Path, home_dir: &str) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Recursively process subdirectories
            process_env_files(&path, home_dir)?;
        } else if path.file_name().and_then(|n| n.to_str()) == Some(".env") {
            // Process .env file
            let content = fs::read_to_string(&path)?;
            let updated_content = content.replace("$HOME", home_dir);
            fs::write(&path, updated_content)?;
            println!("Processed .env file: {}", path.display());
        }
    }

    Ok(())
}
