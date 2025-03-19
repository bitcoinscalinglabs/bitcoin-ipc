use crate::{
    bitcoin_utils,
    db::{self, Database},
    ipc_lib::{
        IpcCheckpointSubnetMsg, IpcCreateSubnetMsg, IpcFundSubnetMsg, IpcJoinSubnetMsg,
        IpcPrefundSubnetMsg, IpcValidate, SubnetId,
    },
    multisig, NETWORK,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use bitcoin::hashes::Hash;
use bitcoincore_rpc::{Client, RpcApi};
use jsonrpc_v2::{Data, Error as JsonRpcError, ErrorLike, MapRouter, Params};
use log::{error, trace};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use thiserror::Error;

pub type RpcServer = Arc<jsonrpc_v2::Server<MapRouter>>;

#[derive(Error, Debug)]
pub enum RpcError {
    #[error("Unauthorized: Invalid token")]
    Unauthorized,

    #[error("Invalid params: {0}")]
    InvalidParams(String),

    #[error("Database error occurred: {0}")]
    DbError(#[from] db::DbError),

    #[error("Internal server error: {0}")]
    InternalError(String),
}

impl ErrorLike for RpcError {
    fn code(&self) -> i64 {
        match self {
            RpcError::Unauthorized => -32001,
            RpcError::InvalidParams(_) => -32602,
            RpcError::InternalError(_) | RpcError::DbError(_) => -32603,
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

        actix_web::HttpResponse::Ok()
            .content_type("application/json")
            .body(json_rpc_error.to_string())
    }
}

// TODO use generics
#[derive(Clone)]
pub struct ServerData {
    pub db: Arc<db::HeedDb>,
    pub btc_rpc: Arc<Client>,
    pub btc_watchonly_rpc: Arc<Client>,
    pub validator_sk: bitcoin::secp256k1::SecretKey,
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

pub async fn get_confirmed_block_height(data: Data<Arc<ServerData>>) -> Result<u64, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_count() {
        Ok(current_height) => {
            let confirmed_block_height =
                match bitcoin_utils::get_confirmed_from_height(current_height) {
                    Some(height) => height,
                    None => {
                        return Err(JsonRpcError::internal(
                            "Not enough blocks to have a confirmed block",
                        ))
                    }
                };
            Ok(confirmed_block_height)
        }
        Err(e) => Err(JsonRpcError::internal(e)),
    }
}

//
// IPC
//

#[derive(Serialize, Deserialize)]
pub struct CreateSubnetResponse {
    subnet_id: SubnetId,
}

pub async fn create_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcCreateSubnetMsg>,
) -> Result<CreateSubnetResponse, JsonRpcError> {
    if let Err(err) = msg.validate() {
        error!("Invalid create message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let subnet_id = msg
        .submit_to_bitcoin(&data.btc_rpc)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    // Return the response
    Ok(CreateSubnetResponse { subnet_id })
}

#[derive(Serialize, Deserialize)]
pub struct JoinSubnetResponse {
    join_txid: bitcoin::Txid,
}

pub async fn join_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcJoinSubnetMsg>,
) -> Result<JoinSubnetResponse, JsonRpcError> {
    if let Err(err) = msg.validate() {
        error!("Invalid join message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    msg.validate_for_genesis_info(&genesis_info).map_err(|e| {
        error!("Error validating join msg for subnet info: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    // TODO this check should be done in the Db
    let multisig_address = &genesis_info.multisig_address();

    let join_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(JoinSubnetResponse { join_txid })
}

#[derive(Serialize, Deserialize)]
pub struct GetGenesisInfoParams {
    subnet_id: SubnetId,
}

pub async fn get_genesis_info(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<GetGenesisInfoParams>,
) -> Result<db::SubnetGenesisInfo, JsonRpcError> {
    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    Ok(genesis_info)
}

#[derive(Serialize, Deserialize)]
pub struct GetSubnetParams {
    subnet_id: SubnetId,
}

pub async fn get_subnet(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetParams>,
) -> Result<db::SubnetState, JsonRpcError> {
    trace!("getsubnet: {}", params.subnet_id);

    // Check subnet exists
    let subnet = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            error!("Subnet {} not found.", params.subnet_id);
            RpcError::InvalidParams(format!("Subnet {} not found.", params.subnet_id))
        })?;

    Ok(subnet)
}

#[derive(Serialize, Deserialize)]
pub struct PrefundSubnetResponse {
    prefund_txid: bitcoin::Txid,
}

pub async fn prefund_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcPrefundSubnetMsg>,
) -> Result<PrefundSubnetResponse, JsonRpcError> {
    if let Err(err) = msg.validate() {
        error!("Invalid prefund message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    msg.validate_for_genesis_info(&genesis_info).map_err(|e| {
        error!("Error validating prefund msg for subnet info: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    // TODO this check should be done in the Db
    let multisig_address = genesis_info.multisig_address();

    let prefund_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(PrefundSubnetResponse { prefund_txid })
}

#[derive(Serialize, Deserialize)]
pub struct FundSubnetResponse {
    fund_txid: bitcoin::Txid,
}

pub async fn fund_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcFundSubnetMsg>,
) -> Result<FundSubnetResponse, JsonRpcError> {
    if let Err(err) = msg.validate() {
        error!("Invalid prefund message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let subnet_state = data
        .db
        .get_subnet_state(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    let multisig_address = subnet_state.multisig_address();

    println!("subnet multisig = {multisig_address:?}");

    let fund_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(FundSubnetResponse { fund_txid })
}

#[derive(Serialize, Deserialize)]
pub struct GetRootnetMessagesParams {
    subnet_id: SubnetId,
    block_height: u64,
}

pub async fn get_rootnet_messages(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetRootnetMessagesParams>,
) -> Result<Vec<db::RootnetMessage>, JsonRpcError> {
    // Check subnet exists
    data.db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    data.db
        .get_rootnet_msgs_by_height(params.subnet_id, params.block_height)
        .map_err(|e| {
            error!("Error getting rootnet messages from Db: {}", e);
            RpcError::DbError(e).into()
        })
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GenMultisigSpendPsbtParams {
    subnet_id: SubnetId,
    recipient: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    amount: bitcoin::Amount,
}

#[derive(Serialize, Deserialize)]
pub struct GenMultisigSpendPsbtResponse {
    unsigned_psbt: bitcoin::Psbt,
    unsigned_psbt_base64: String,
    unsigned_psbt_hash: bitcoin::hashes::sha256::Hash,
    psbt_inputs_signatures: Vec<bitcoin::secp256k1::schnorr::Signature>,
}

pub async fn gen_multisig_spend_psbt(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GenMultisigSpendPsbtParams>,
) -> Result<GenMultisigSpendPsbtResponse, JsonRpcError> {
    trace!("gen_multisig_spend_psbt: {:?}", params);

    // Check subnet exists
    let subnet = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    let recipient = params.recipient.require_network(NETWORK).map_err(|_| {
        RpcError::InvalidParams(format!("Invalid address network, required {NETWORK}"))
    })?;

    let committee_address = subnet
        .committee
        .multisig_address
        .clone()
        .require_network(NETWORK)
        .expect("Multisig should be valid for saved subnet genesis info");
    let committee_keys = subnet
        .committee
        .validators
        .iter()
        .map(|v| v.pubkey)
        .collect::<Vec<_>>();
    let commitee_threshold = subnet.committee.threshold;

    let unspent = subnet
        .committee
        .get_unspent(&data.btc_watchonly_rpc)
        .map_err(|e| RpcError::InternalError(e.to_string()))?;

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);

    let secp = bitcoin::secp256k1::Secp256k1::new();

    let unsigned_psbt = multisig::construct_spend_psbt(
        &secp,
        &params.subnet_id,
        &committee_keys,
        commitee_threshold,
        &committee_address,
        &unspent,
        &[bitcoin::TxOut {
            value: params.amount,
            script_pubkey: recipient.script_pubkey(),
        }],
        &fee_rate,
    )
    .map_err(|e| {
        error!(
            "Error generating multisig spend psbt for subnet_id={}: {}",
            &params.subnet_id, e
        );
        RpcError::InternalError(e.to_string())
    })?;

    let validator_keypair = data.validator_sk.keypair(&secp);

    let (_, psbt_inputs_signatures) =
        multisig::sign_spend_psbt(&secp, unsigned_psbt.clone(), validator_keypair).map_err(
            |e| {
                error!(
                    "Error signing multisig spend psbt for subnet_id={}: {}",
                    &params.subnet_id, e
                );
                RpcError::InternalError(e.to_string())
            },
        )?;

    let unsigned_psbt_bytes = unsigned_psbt.serialize();
    let unsigned_psbt_hash = bitcoin::hashes::sha256::Hash::hash(&unsigned_psbt_bytes);
    let unsigned_psbt_base64 = BASE64_STANDARD.encode(unsigned_psbt_bytes);

    Ok(GenMultisigSpendPsbtResponse {
        unsigned_psbt_base64,
        unsigned_psbt_hash,
        psbt_inputs_signatures,
        unsigned_psbt,
    })
}

#[derive(Serialize, Deserialize)]
pub struct GenCheckpointPsbtResponse {
    // Checkpoint
    unsigned_psbt: bitcoin::Psbt,
    unsigned_psbt_base64: String,
    unsigned_psbt_hash: bitcoin::hashes::sha256::Hash,
    psbt_inputs_signatures: Vec<bitcoin::secp256k1::schnorr::Signature>,
    // Batch transfer reveal
    batch_transfer_tx_hex: Option<String>,
}

pub async fn gen_checkpoint_psbt(
    data: Data<Arc<ServerData>>,
    Params(mut msg): Params<IpcCheckpointSubnetMsg>,
) -> Result<GenCheckpointPsbtResponse, JsonRpcError> {
    trace!("gen_checkpoint_psbt: {:?}", msg);

    if let Err(err) = msg.validate() {
        error!("Invalid checkpoint message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    if msg.change_address.is_some() {
        return Err(
            RpcError::InvalidParams("Specifying change address not supported".to_string()).into(),
        );
    }

    // Check subnet exists
    let subnet = data
        .db
        .get_subnet_state(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    let secp = bitcoin::secp256k1::Secp256k1::new();

    let (self_pubkey, _) = data.validator_sk.x_only_public_key(&secp);

    // Check if self is a validator in the subnet
    if !subnet.is_validator(&self_pubkey) {
        error!("Configured validator isn't a validator in the specified subnet.");
        return Err(RpcError::InvalidParams(
            "Configured validator isn't a validator in the specified subnet.".to_string(),
        )
        .into());
    }

    // Fill in the subnet addresses, erroring out if any subnet is not found
    msg.update_subnets_for_transfer(&*data.db).map_err(|e| {
        error!("Error updating subnets for transfer: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    let unspent = subnet
        .committee
        .get_unspent(&data.btc_watchonly_rpc)
        .map_err(|e| RpcError::InternalError(e.to_string()))?;

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);

    let unsigned_psbt = msg
        .to_checkpoint_psbt(&subnet.committee, fee_rate, &unspent)
        .map_err(|e| {
            error!(
                "Error generating checkpoint psbt for subnet_id={}: {}",
                &msg.subnet_id, e
            );

            RpcError::InternalError(e.to_string())
        })?;

    let checkpoint_txid = unsigned_psbt.unsigned_tx.compute_txid();

    let batch_transfer_tx = msg
        .make_reveal_batch_transfer_tx(
            checkpoint_txid,
            fee_rate,
            &subnet.committee.address_checked(),
        )
        .map_err(|e| {
            error!(
                "Error generating batch transfer tx for subnet_id={}: {}",
                &msg.subnet_id, e
            );

            RpcError::InternalError(e.to_string())
        })?;

    trace!(
        "checkpoint_txid={} batch_transfer_txid={:?}",
        checkpoint_txid,
        batch_transfer_tx.clone().map(|tx| tx.compute_txid()),
    );

    let batch_transfer_tx_hex =
        batch_transfer_tx.map(|tx| bitcoin::consensus::encode::serialize_hex(&tx));

    let validator_keypair = data.validator_sk.keypair(&secp);

    let (_, psbt_inputs_signatures) =
        multisig::sign_spend_psbt(&secp, unsigned_psbt.clone(), validator_keypair).map_err(
            |e| {
                error!(
                    "Error signing multisig spend psbt for subnet_id={}: {}",
                    &msg.subnet_id, e
                );
                RpcError::InternalError(e.to_string())
            },
        )?;

    let unsigned_psbt_bytes = unsigned_psbt.serialize();
    let unsigned_psbt_hash = bitcoin::hashes::sha256::Hash::hash(&unsigned_psbt_bytes);
    let unsigned_psbt_base64 = BASE64_STANDARD.encode(unsigned_psbt_bytes);

    Ok(GenCheckpointPsbtResponse {
        unsigned_psbt_base64,
        unsigned_psbt_hash,
        psbt_inputs_signatures,
        unsigned_psbt,
        batch_transfer_tx_hex,
    })
}

#[derive(Serialize, Deserialize)]
pub struct DevMultisignPsbtParams {
    unsigned_psbt_base64: String,
    secret_keys: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DevMultisignPsbtResponse {
    // Include signatures for each input, mapped by pubkey
    signatures: Vec<(
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    )>,
}

pub async fn dev_multisign_psbt(
    _data: Data<Arc<ServerData>>,
    Params(params): Params<DevMultisignPsbtParams>,
) -> Result<DevMultisignPsbtResponse, JsonRpcError> {
    trace!(
        "dev_multisign_psbt: unsigned_psbt with {} secret_keys",
        params.secret_keys.len()
    );

    // Decode base64 PSBT
    let psbt_bytes = BASE64_STANDARD
        .decode(params.unsigned_psbt_base64.as_bytes())
        .map_err(|e| {
            error!("Invalid base64 format for PSBT: {}", e);
            RpcError::InvalidParams(format!("Invalid base64 format for PSBT: {}", e))
        })?;

    // Deserialize PSBT
    let unsigned_psbt = bitcoin::psbt::Psbt::deserialize(&psbt_bytes).map_err(|e| {
        error!("Invalid PSBT format: {}", e);
        RpcError::InvalidParams(format!("Invalid PSBT format: {}", e))
    })?;

    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Store signatures keyed by public key
    let mut all_signatures = Vec::new();

    // Convert each hex string to a SecretKey and sign the PSBT
    for hex_key in params.secret_keys {
        // Remove "0x" prefix if present
        let hex_key = hex_key.strip_prefix("0x").unwrap_or(&hex_key);

        // Convert hex string to bytes
        let key_bytes = hex::decode(hex_key).map_err(|e| {
            error!("Invalid hex format for secret key: {}", e);
            RpcError::InvalidParams(format!("Invalid hex format for secret key: {}", e))
        })?;

        // Create secret key from bytes
        let secret_key = bitcoin::secp256k1::SecretKey::from_slice(&key_bytes).map_err(|e| {
            error!("Invalid secret key: {}", e);
            RpcError::InvalidParams(format!("Invalid secret key: {}", e))
        })?;

        // Create keypair
        let keypair = secret_key.keypair(&secp);
        let (xonly_pubkey, _) = keypair.x_only_public_key();

        // Sign and collect signatures
        let (_, signatures) = multisig::sign_spend_psbt(&secp, unsigned_psbt.clone(), keypair)
            .map_err(|e| {
                error!("Error signing psbt with key: {}", e);
                RpcError::InternalError(e.to_string())
            })?;

        all_signatures.push((xonly_pubkey, signatures));
    }

    Ok(DevMultisignPsbtResponse {
        signatures: all_signatures,
    })
}

#[derive(Serialize, Deserialize)]
pub struct FinalizeCheckpointPsbtParams {
    subnet_id: SubnetId,
    unsigned_psbt_base64: String,
    signatures: Vec<(
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    )>,
    batch_transfer_tx_hex: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct FinalizeCheckpointPsbtResponse {
    tx: bitcoin::Transaction,
    txid: String,
    tx_hex: String,
    batch_transfer_txid: Option<String>,
}

pub async fn finalize_checkpoint_psbt(
    data: Data<Arc<ServerData>>,
    Params(params): Params<FinalizeCheckpointPsbtParams>,
) -> Result<FinalizeCheckpointPsbtResponse, JsonRpcError> {
    trace!("finalize_checkpoint_psbt for subnet {}", params.subnet_id);

    // Check subnet exists and get committee info
    let subnet = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    // Get committee info
    let committee_keys: Vec<bitcoin::XOnlyPublicKey> = subnet
        .committee
        .validators
        .iter()
        .map(|v| v.pubkey)
        .collect();
    let committee_threshold = subnet.committee.threshold;

    let batch_transfer_tx: Option<bitcoin::Transaction> = match params.batch_transfer_tx_hex {
        Some(hex) => {
            let tx_bytes = hex::decode(hex).map_err(|e| {
                error!("Invalid hex format for batch transfer tx: {}", e);
                RpcError::InvalidParams(format!("Invalid hex format for batch transfer tx: {}", e))
            })?;

            Some(bitcoin::consensus::deserialize(&tx_bytes).map_err(|e| {
                error!("Invalid transaction format for batch transfer tx: {}", e);
                RpcError::InvalidParams(format!(
                    "Invalid transaction format for batch transfer tx: {}",
                    e
                ))
            })?)
        }
        None => None,
    };

    // Decode base64 PSBT
    let psbt_bytes = BASE64_STANDARD
        .decode(params.unsigned_psbt_base64.as_bytes())
        .map_err(|e| {
            error!("Invalid base64 format for PSBT: {}", e);
            RpcError::InvalidParams(format!("Invalid base64 format for PSBT: {}", e))
        })?;

    // Deserialize PSBT
    let unsigned_psbt = bitcoin::psbt::Psbt::deserialize(&psbt_bytes).map_err(|e| {
        error!("Invalid PSBT format: {}", e);
        RpcError::InvalidParams(format!("Invalid PSBT format: {}", e))
    })?;

    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Organize signatures by signer
    let signature_sets: Vec<&[bitcoin::secp256k1::schnorr::Signature]> = params
        .signatures
        .iter()
        .map(|(_, sigs)| sigs.as_slice())
        .collect();

    // Finalize the PSBT using multisig::finalize_spend_psbt_from_sigs
    let finalized_tx = multisig::finalize_spend_psbt_from_sigs(
        &secp,
        &params.subnet_id,
        &committee_keys,
        committee_threshold,
        &unsigned_psbt,
        &signature_sets,
    )
    .map_err(|e| {
        error!("Error finalizing PSBT: {}", e);
        RpcError::InternalError(e.to_string())
    })?;

    // Convert transaction to hex
    let tx_hex = hex::encode(bitcoin::consensus::serialize(&finalized_tx));

    // Get transaction ID
    let txid = finalized_tx.compute_txid().to_string();
    let batch_transfer_txid = batch_transfer_tx
        .clone()
        .map(|tx| tx.compute_txid().to_string());

    trace!("checkpoint_txid = {}", txid);

    let mut tx_to_submit = vec![finalized_tx.clone()];

    if let Some(batch_transfer_tx) = batch_transfer_tx {
        tx_to_submit.push(batch_transfer_tx);
    }

    // Send the transaction to the Bitcoin network
    bitcoin_utils::submit_to_mempool(&data.btc_rpc, tx_to_submit).map_err(|e| {
        error!("Error sending transaction to Bitcoin network: {}", e);
        RpcError::InternalError(format!(
            "Error sending transaction to Bitcoin network: {}",
            e
        ))
    })?;

    Ok(FinalizeCheckpointPsbtResponse {
        tx: finalized_tx,
        txid,
        tx_hex,
        batch_transfer_txid,
    })
}

pub fn make_rpc_server(server_data: Arc<ServerData>) -> RpcServer {
    jsonrpc_v2::Server::new()
        .with_data(Data::new(server_data))
        // btc info
        .with_method("getblockhash", get_block_hash)
        .with_method("getblockcount", get_block_count)
        .with_method("getconfirmedcount", get_confirmed_block_height)
        // subnet
        .with_method("createsubnet", create_subnet)
        .with_method("joinsubnet", join_subnet)
        .with_method("getsubnet", get_subnet)
        .with_method("getgenesisinfo", get_genesis_info)
        .with_method("prefundsubnet", prefund_subnet)
        .with_method("fundsubnet", fund_subnet)
        // rootnet messages
        .with_method("getrootnetmessages", get_rootnet_messages)
        // multisig
        .with_method("genmultisigspendpsbt", gen_multisig_spend_psbt)
        // checkpoints
        .with_method("gencheckpointpsbt", gen_checkpoint_psbt)
        .with_method("dev_multisignpsbt", dev_multisign_psbt) // dev only
        .with_method("finalizecheckpointpsbt", finalize_checkpoint_psbt)
        .finish()
}
