use bitcoin_ipc::bitcoin_utils::make_rpc_client_from_env;
use bitcoin_ipc::provider;
use dotenv::dotenv;
use log::error;
use std::sync::Arc;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load .env file

    dotenv().ok();

    // Initialize the logger, configurable by the RUST_LOG env

    env_logger::init();

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

    // Start up the actix-web server

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| "3030".to_string());

    let server_data = Arc::new(provider::ServerData { btc_rpc });

    provider::Server::new(token, port, server_data)
        .serve()
        .await
}
