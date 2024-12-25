use crate::{bitcoin_utils::create_multisig_address, utils, NETWORK};
use bitcoin::{Amount, XOnlyPublicKey};
use bitcoincore_rpc::{Client, RpcApi};
use jsonrpc_v2::{Data, Error as JsonRpcError, ErrorLike, MapRouter, Params};
use log::debug;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{str::FromStr, sync::Arc};
use thiserror::Error;

pub type RpcServer = Arc<jsonrpc_v2::Server<MapRouter>>;

#[derive(Error, Debug)]
pub enum RpcError {
    #[error("Unauthorized: Invalid token")]
    Unauthorized,

    #[error("Invalid params: {0}")]
    InvalidParams(String),

    #[error("Internal server error: {0}")]
    InternalError(String),
}

impl ErrorLike for RpcError {
    fn code(&self) -> i64 {
        match self {
            RpcError::Unauthorized => -32001,
            RpcError::InvalidParams(_) => -32602,
            RpcError::InternalError(_) => -32603,
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }
}

impl actix_web::error::ResponseError for RpcError {
    fn error_response(&self) -> actix_web::HttpResponse {
        let json_rpc_error = json!({
            "jsonrpc": "2.0",
            "error": {
                "code": &self.code(),
                "message": &self.message()
            },
            "id": null
        });
        actix_web::HttpResponse::Unauthorized()
            .content_type("application/json")
            .body(json_rpc_error.to_string())
    }
}

#[derive(Clone)]
pub struct ServerData {
    pub btc_rpc: Arc<Client>,
    pub config: utils::Config,
}

//
// Bitcoin RPC
//

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
        Err(e) => Err(JsonRpcError::internal(e)),
    }
}

pub async fn get_block_count(data: Data<Arc<ServerData>>) -> Result<u64, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_count() {
        Ok(block_count) => Ok(block_count),
        Err(e) => Err(RpcError::InternalError(e.to_string()).into()),
    }
}

pub async fn get_confirmed_block(data: Data<Arc<ServerData>>) -> Result<String, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    let confirmations = data.config.ipc_finalization_parameter;

    match client.get_block_count() {
        Ok(current_height) => {
            if current_height < confirmations {
                return Err(JsonRpcError::internal(
                    "Not enough blocks to have a final block",
                ));
            }

            let final_block_height = current_height - confirmations;
            match client.get_block_hash(final_block_height) {
                Ok(block_hash) => Ok(block_hash.to_string()),
                Err(e) => Err(JsonRpcError::internal(e)),
            }
        }
        Err(e) => Err(JsonRpcError::internal(e)),
    }
}

//
// IPC
//

#[derive(Serialize, Deserialize)]
pub struct CreateSubnetParams {
    /// The minimum number of collateral required for validators in Satoshis
    min_validator_stake: u64,
    /// Minimum number of validators required to bootstrap the subnet
    min_validators: u64,
    /// The bottom up checkpoint period in number of blocks
    bottomup_check_period: u64,
    /// The max number of active validators in subnet
    active_validators_limit: u16,
    /// Minimum fee for cross-net messages in subnet (in Satoshis)
    min_cross_msg_fee: Amount,
    /// The addresses of whitelisted validators
    whitelist: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CreateSubnetResponse {
    subnet_id: String,
}

pub async fn create_subnet(
    data: Data<Arc<ServerData>>,
    Params(params): Params<CreateSubnetParams>,
) -> Result<CreateSubnetResponse, JsonRpcError> {
    if params.min_validators == 0 {
        return Err(RpcError::InvalidParams(
            "The minimum number of validators must be greater than 0".to_string(),
        )
        .into());
    }

    if params.whitelist.len() < params.min_validators as usize {
        return Err(RpcError::InvalidParams(
            "Number of whitelisted validators is less than the minimum required validators"
                .to_string(),
        )
        .into());
    }

    // TODO check the maximum size of the multisig signatures
    let required_sigs: i64 = params.min_validators.try_into().map_err(|_| {
        RpcError::InvalidParams(format!(
            "The minimum number of validators must not be greater than {}",
            i64::MAX
        ))
    })?;

    // Parse the min_validator_stake as Amount
    let min_validator_stake = Amount::from_sat(params.min_validator_stake);

    // Parse the whitelist addresses as XOnlyPublicKey
    let public_keys: Result<Vec<XOnlyPublicKey>, _> = params
        .whitelist
        .iter()
        .map(|addr| {
            XOnlyPublicKey::from_str(addr)
                .map_err(|_e| RpcError::InvalidParams(format!("Public key {} is invalid", &addr)))
        })
        .collect();

    // TODO handle errors
    let public_keys = public_keys?;

    // Create a multisig address from the public keys
    let multisig_address = create_multisig_address(&public_keys, required_sigs, NETWORK);

    debug!("multisig_address: {}", multisig_address);

    let mut params_map = std::collections::HashMap::new();
    params_map.insert(
        "min_validator_stake",
        min_validator_stake.to_sat().to_string(),
    );
    params_map.insert("min_validators", params.min_validators.to_string());
    params_map.insert(
        "bottomup_check_period",
        params.bottomup_check_period.to_string(),
    );
    params_map.insert(
        "active_validators_limit",
        params.active_validators_limit.to_string(),
    );
    params_map.insert(
        "min_cross_msg_fee",
        params.min_cross_msg_fee.to_sat().to_string(),
    );
    params_map.insert("whitelist", params.whitelist.join(","));

    // Create the subnet data string
    let mut subnet_data = String::new();
    subnet_data.push_str(crate::IPC_CREATE_SUBNET_TAG);

    for (key, value) in &params_map {
        subnet_data.push_str(&format!("{}{}={}", crate::DELIMITER, key, value));
    }

    debug!("subnet_data: {}", subnet_data);

    // Create and submit the create child transaction
    let (commit_tx, _) = crate::ipc_lib::create_and_submit_create_child_tx(
        &data.btc_rpc,
        &multisig_address,
        &subnet_data,
    )
    .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    // Compute the transaction ID
    let commit_tx_id: bitcoin::Txid = commit_tx.compute_txid();

    // Generate the subnet ID
    let subnet_id = format!("{}/{}", crate::L1_NAME, commit_tx_id);

    debug!("subnet_id: {}", subnet_id);

    // Return the response
    Ok(CreateSubnetResponse { subnet_id })
}

pub fn make_rpc_server(server_data: Arc<ServerData>) -> RpcServer {
    jsonrpc_v2::Server::new()
        .with_data(Data::new(server_data))
        .with_method("getblockhash", get_block_hash)
        .with_method("getblockcount", get_block_count)
        .with_method("getconfirmedblock", get_confirmed_block)
        .with_method("createsubnet", create_subnet)
        .finish()
}
