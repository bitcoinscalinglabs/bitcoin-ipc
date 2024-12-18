use crate::utils;
use bitcoincore_rpc::{Client, RpcApi};
use jsonrpc_v2::{Data, Error as JsonRpcError, MapRouter, Params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub type RpcServer = Arc<jsonrpc_v2::Server<MapRouter>>;

#[derive(Clone)]
pub struct ServerData {
    pub btc_rpc: Arc<Client>,
    pub config: utils::Config,
}

#[derive(Serialize, Deserialize)]
pub struct GetBlockHashParams {
    height: u64,
}

pub async fn get_block_hash(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetBlockHashParams>,
) -> Result<String, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_hash(params.height) {
        Ok(block_hash) => Ok(block_hash.to_string()),
        Err(e) => Err(JsonRpcError::Full {
            code: -1,
            message: e.to_string(),
            data: None,
        }),
    }
}

pub async fn get_block_count(data: Data<Arc<ServerData>>) -> Result<u64, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_count() {
        Ok(block_count) => Ok(block_count),
        Err(e) => Err(JsonRpcError::Full {
            code: -1,
            message: e.to_string(),
            data: None,
        }),
    }
}

pub async fn get_confirmed_block(data: Data<Arc<ServerData>>) -> Result<String, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    let confirmations = data.config.ipc_finalization_parameter;

    match client.get_block_count() {
        Ok(current_height) => {
            if current_height < confirmations {
                return Err(JsonRpcError::Full {
                    code: -1,
                    message: "Not enough blocks to have a final block".to_string(),
                    data: None,
                });
            }

            let final_block_height = current_height - confirmations;
            match client.get_block_hash(final_block_height) {
                Ok(block_hash) => Ok(block_hash.to_string()),
                Err(e) => Err(JsonRpcError::Full {
                    code: -1,
                    message: e.to_string(),
                    data: None,
                }),
            }
        }
        Err(e) => Err(JsonRpcError::Full {
            code: -1,
            message: e.to_string(),
            data: None,
        }),
    }
}

pub fn make_rpc_server(server_data: Arc<ServerData>) -> RpcServer {
    jsonrpc_v2::Server::new()
        .with_data(Data::new(server_data))
        .with_method("getblockhash", get_block_hash)
        .with_method("getblockcount", get_block_count)
        .with_method("getconfirmedblock", get_confirmed_block)
        .finish()
}
