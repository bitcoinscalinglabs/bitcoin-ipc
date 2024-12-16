use actix_web;
use bitcoin_ipc::{bitcoin_utils, utils};
use jsonrpc_v2::{Data, Error as JsonRpcError, Params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use bitcoincore_rpc::{Client, RpcApi};

#[derive(Serialize, Deserialize)]
struct GetBlockHashParams {
    height: u64,
}

async fn getblockhash(
    data: Data<Arc<Client>>,
    Params(params): Params<GetBlockHashParams>,
) -> Result<String, JsonRpcError> {
    let client = data.as_ref();

    match client.get_block_hash(params.height) {
        Ok(block_hash) => Ok(block_hash.to_string()),
        Err(e) => Err(JsonRpcError::Full {
            code: -1,
            message: e.to_string(),
            data: None,
        }),
    }
}

fn make_bitcoincore_rpc() -> Arc<Client> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = match utils::load_env() {
        Ok(env) => env,
        Err(e) => {
            panic!("Error: {}", e);
        }
    };

    let rpc = match bitcoin_utils::init_rpc_client(rpc_user, rpc_pass, rpc_url) {
        Ok(rpc) => rpc,
        Err(e) => {
            panic!("Error: {}", e);
        }
    };
    let _ = rpc.load_wallet(&wallet_name);
    let rpc = Arc::new(rpc);
    rpc
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let btc_rpc = make_bitcoincore_rpc();

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| "3030".to_string());
    println!("Server is running on http://127.0.0.1:{}", port);

    let addr = format!("127.0.0.1:{}", port);

    let rpc_server = jsonrpc_v2::Server::new()
        .with_data(Data::new(btc_rpc))
        .with_method("getblockhash", getblockhash)
        .finish();

    actix_web::HttpServer::new(move || {
        let rpc = rpc_server.clone();
        actix_web::App::new().service(
            actix_web::web::service("/api")
                .guard(actix_web::guard::Post())
                .finish(rpc.into_web_service()),
        )
    })
    .bind(addr)?
    .run()
    .await
}
