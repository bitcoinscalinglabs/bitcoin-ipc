use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use bitcoin_ipc::bitcoin_utils::make_rpc_client_from_env;
use bitcoin_ipc::db::HeedDb;
use bitcoin_ipc::{eth_utils, provider};
use clap::Parser;
use log::error;

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

    let btc_rpc = Arc::new(make_rpc_client_from_env());

    // Set correct fvm network

    eth_utils::set_fvm_network();

    // Start up the actix-web server

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| DEFAULT_PROVIDER_PORT.to_string());

    let server_data = Arc::new(provider::ServerData { db, btc_rpc });

    provider::Server::new(token, port, server_data)
        .serve()
        .await
}
