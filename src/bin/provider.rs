use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use bitcoin_ipc::db::HeedDb;
use bitcoin_ipc::{bitcoin_utils, eth_utils, provider};
use clap::Parser;
use log::{error, info};

const DEFAULT_PROVIDER_PORT: &str = "3030";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to env file
    #[arg(long, default_value = ".env")]
    env: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Parse command line arguments

    let args = Args::parse();

    // Load .env file

    let env_path = if args.env.starts_with('/') {
        PathBuf::from(&args.env)
    } else {
        env::current_dir().map(|a| a.join(&args.env)).unwrap()
    };

    dotenv::from_path(env_path.as_path())
        .unwrap_or_else(|_| panic!("Failed to load env file: {}", args.env));

    // Initialize the logger, configurable by the RUST_LOG env

    env_logger::init();

    // Initialize the database

    let db = Arc::new(
        HeedDb::new(
            &std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            true, // using db in read-only mode
        )
        .await
        .expect("Failed to initialize database"),
    );

    // Load auth token from env

    let token = std::env::var("PROVIDER_AUTH_TOKEN").map_err(|e| {
        error!("Couldn't load PROVIDER_AUTH_TOKEN: {}", e);
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "Couldn't load PROVIDER_AUTH_TOKEN",
        )
    })?;

    // Init the bitcoincore_rpc client

    let btc_rpc = Arc::new(bitcoin_utils::make_rpc_client_from_env());
    let btc_watchonly_rpc = Arc::new(bitcoin_utils::make_watchonly_rpc_client_from_env());

    // Load validator secret key
    let validator_sk = load_validator_sk()?;

    // Set correct fvm network

    eth_utils::set_fvm_network();

    // Start up the actix-web server

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| DEFAULT_PROVIDER_PORT.to_string());

    let server_data = Arc::new(provider::ServerData {
        db,
        btc_rpc,
        btc_watchonly_rpc,
        validator_sk,
    });

    provider::Server::new(token, port, server_data)
        .serve()
        .await
}

fn load_validator_sk() -> Result<bitcoin::secp256k1::SecretKey, std::io::Error> {
    // Load validator secret key from path
    let sk_path = std::env::var("VALIDATOR_SK_PATH").map_err(|e| {
        error!("Couldn't load VALIDATOR_SK_PATH: {}", e);
        std::io::Error::new(std::io::ErrorKind::Other, "Couldn't load VALIDATOR_SK_PATH")
    })?;
    let sk_path = PathBuf::from(sk_path);

    let sk_hex = std::fs::read_to_string(&sk_path).map_err(|e| {
        error!(
            "Couldn't read validator secret key from {}: {}",
            sk_path.display(),
            e
        );
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Couldn't read validator secret key: {}", e),
        )
    })?;

    // Trim whitespace and remove any '0x' prefix if present
    let sk_hex = sk_hex.trim();
    let sk_hex = sk_hex.strip_prefix("0x").unwrap_or(sk_hex);

    // Convert hex string to bytes
    let sk_bytes = hex::decode(sk_hex).map_err(|e| {
        error!("Invalid hex encoding in secret key file: {}", e);
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Invalid hex encoding in secret key file: {}", e),
        )
    })?;

    let validator_sk = bitcoin::secp256k1::SecretKey::from_slice(&sk_bytes).map_err(|e| {
        error!("Invalid validator secret key: {}", e);
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Invalid validator secret key: {}", e),
        )
    })?;

    info!("Loaded validator secret key from {}", sk_path.display());

    Ok(validator_sk)
}
