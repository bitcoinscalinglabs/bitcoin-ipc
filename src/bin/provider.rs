use bitcoin_ipc::bitcoin_utils;
use bitcoin_ipc::provider;
use dotenv::dotenv;
use log::error;
use std::sync::Arc;

use bitcoincore_rpc::{Client, RpcApi};

fn make_bitcoincore_rpc() -> Arc<Client> {
    let rpc_user = std::env::var("RPC_USER").expect("RPC_USER env var not defined");
    let rpc_pass = std::env::var("RPC_PASS").expect("RPC_PASS env var not defined");
    let rpc_url = std::env::var("RPC_URL").expect("RPC_URL env var not defined");
    let wallet_name = std::env::var("WALLET_NAME").expect("WALLET_NAME env var not defined");

    let rpc = match bitcoin_utils::init_rpc_client(rpc_user, rpc_pass, rpc_url) {
        Ok(rpc) => rpc,
        Err(e) => {
            panic!("Error: {}", e);
        }
    };
    let _ = rpc.load_wallet(&wallet_name);
    Arc::new(rpc)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load .env file

    dotenv().ok();

    // Initialize the logger, configurable by the RUST_LOG env

    env_logger::init();

    // Init the bitcoincore_rpc client

    let btc_rpc = make_bitcoincore_rpc();

    // Load auth token from env

    let token = std::env::var("PROVIDER_AUTH_TOKEN").map_err(|e| {
        error!("Couldn't load PROVIDER_AUTH_TOKEN: {}", e);
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "Couldn't load PROVIDER_AUTH_TOKEN",
        )
    })?;

    // Start up the actix-web server
    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| "3030".to_string());

    let server_data = Arc::new(provider::ServerData { btc_rpc });

    provider::Server::new(token, port, server_data)
        .serve()
        .await
}
