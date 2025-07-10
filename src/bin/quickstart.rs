use bitcoin::secp256k1::{Secp256k1, SecretKey};
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

static DEMO_IPC: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/internal/demo.ipc");

use bitcoin_ipc::eth_utils::eth_addr_from_x_only_pubkey;

#[derive(Debug, Deserialize, Serialize)]
struct KeystoreEntry {
    address: String,
    private_key: String,
}

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

    // Print validator/user addresses and public keys
    print_validator_user_keys(&ipc_dir)?;

    Ok(())
}

async fn setup_bitcoin_wallets() -> Result<(), Box<dyn std::error::Error>> {
    // Check if default wallet already exists by trying to load it first
    let wallet_load = Command::new("bitcoin-cli")
        .args(["--regtest", "loadwallet", "default"])
        .output();

    let wallets_exist = match wallet_load {
        Ok(output) => {
            if output.status.success() {
                true // Successfully loaded, so it exists
            } else {
                let error_message = String::from_utf8_lossy(&output.stderr);
                // If already loaded, it exists. If path doesn't exist, it doesn't exist.
                !error_message.contains("Path does not exist.")
            }
        }
        Err(_) => false,
    };

    let wallet_names = vec![
        "default",
        "validator1",
        "validator2",
        "validator3",
        "validator4",
        "validator5",
        "user1",
        "user2",
    ];

    if wallets_exist {
        println!("Default wallet exists, skipping wallet creation.");

        // Load all wallets
        for wallet in &wallet_names {
            let output = Command::new("bitcoin-cli")
                .args(["--regtest", "loadwallet", wallet])
                .output()?;

            if output.status.success() {
                println!("Loaded wallet: {}", wallet);
            } else {
                let error_message = String::from_utf8_lossy(&output.stderr);

                if !error_message.contains("already loaded") {
                    println!("Failed to load wallet {}: {}", wallet, error_message);
                }
            }
        }
    } else {
        println!("Creating Bitcoin wallets...");

        // Create wallets
        for wallet in &wallet_names {
            let output = Command::new("bitcoin-cli")
                .args(["--regtest", "createwallet", wallet])
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
    println!("\n=== Bitcoin wallet balances ===");
    for wallet in &wallet_names {
        check_wallet_balance(wallet).await?;
    }

    Ok(())
}

async fn fund_wallet(wallet_name: &str, blocks: u32) -> Result<(), Box<dyn std::error::Error>> {
    // Get new address for the wallet
    let address_output = Command::new("bitcoin-cli")
        .args([
            "--regtest",
            &format!("--rpcwallet={}", wallet_name),
            "getnewaddress",
        ])
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
        .args([
            "--regtest",
            "generatetoaddress",
            &blocks.to_string(),
            &address,
        ])
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
        .args([
            "--regtest",
            &format!("--rpcwallet={}", wallet_name),
            "getbalance",
        ])
        .output()?;

    if output.status.success() {
        let balance = String::from_utf8(output.stdout)?.trim().to_string();
        println!("{:<12}\tbalance: {} BTC", wallet_name, balance);
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

fn print_validator_user_keys(ipc_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let keystore_path = ipc_dir.join("btc_keystore.json");

    if !keystore_path.exists() {
        println!("Keystore file not found at: {}", keystore_path.display());
        return Ok(());
    }

    let keystore_content = fs::read_to_string(&keystore_path)?;
    let keystore: Vec<KeystoreEntry> = serde_json::from_str(&keystore_content)?;

    let labels = [
        "validator1",
        "validator2",
        "validator3",
        "validator4",
        "validator5",
        "user1",
        "user2",
    ];

    println!("\n=== Validator/User Keys and Addresses ===");
    println!(
        "{:<12} {:<42} {:<66}",
        "Label", "ETH Address", "X-Only PubKey"
    );
    println!("{}", "=".repeat(120));

    let secp = Secp256k1::new();
    let mut whitelist_xpks = Vec::new();

    for (i, entry) in keystore.iter().enumerate() {
        if i >= labels.len() {
            break;
        }

        let label = labels[i];

        // Parse private key
        let sk_hex = entry
            .private_key
            .trim()
            .strip_prefix("0x")
            .unwrap_or(&entry.private_key);
        let sk_bytes = hex::decode(sk_hex)?;
        let secret_key = SecretKey::from_slice(&sk_bytes)?;

        // Derive x-only public key
        let (x_only_pubkey, _) = secret_key.x_only_public_key(&secp);

        // Collect first 4 validator x-only public keys for whitelist
        if i < 4 {
            whitelist_xpks.push(x_only_pubkey.to_string());
        }

        // Derive Ethereum address
        let eth_address = eth_addr_from_x_only_pubkey(x_only_pubkey);

        println!(
            "{:<12} {:<42} {:<66}",
            label,
            format!("0x{:x}", eth_address),
            x_only_pubkey.to_string()
        );
    }

    println!("{}", "=".repeat(120));

    // Print recommended whitelist (first 4 validators' x-only public keys)
    println!("\n=== Recommended Whitelist (First 4 Validators) ===");
    println!("{}", whitelist_xpks.join(","));
    println!();

    Ok(())
}
