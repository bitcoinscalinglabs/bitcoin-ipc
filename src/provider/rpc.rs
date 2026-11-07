use crate::{
    bitcoin_utils, collateral,
    db::{self, DatabaseCore, StakingChange},
    eots,
    ipc_lib::{
        self, validate_erc_batch_data, IpcCheckpointSubnetMsg, IpcCreateSubnetMsg,
        IpcFundSubnetMsg, IpcJoinSubnetMsg, IpcJoinUeMsg, IpcKillSubnetMsg, IpcLibError,
        IpcPrefundSubnetMsg, IpcStakeCollateralMsg, IpcUnstakeCollateralMsg, IpcValidate,
        SubnetId,
    },
    multisig, wallet, NETWORK,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use bitcoin::hashes::Hash;
use bitcoincore_rpc::{Client, RpcApi};
use jsonrpc_v2::{Data, Error as JsonRpcError, ErrorLike, MapRouter, Params};
use log::{error, info, trace, warn};
use num_traits::Zero;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use thiserror::Error;

#[cfg(feature = "emission_chain")]
use crate::db::DatabaseRewardExtensions;
#[cfg(feature = "emission_chain")]
use crate::rewards::RewardConfig;

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

    #[error("the monitor has not processed Bitcoin height {requested} yet: last processed {processed}")]
    MonitorBehind { requested: u64, processed: u64 },
}

impl ErrorLike for RpcError {
    fn code(&self) -> i64 {
        match self {
            RpcError::Unauthorized => -32001,
            RpcError::InvalidParams(_) => -32602,
            RpcError::InternalError(_) | RpcError::DbError(_) => -32603,
            RpcError::MonitorBehind { .. } => -32004,
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
    pub validator: Option<(bitcoin::XOnlyPublicKey, bitcoin::secp256k1::SecretKey)>,
    /// The EOTS secret key and the seed of its nonces, from `EOTS_SK_PATH` and `EOTS_SEED_PATH`
    pub eots: Option<(bitcoin::secp256k1::SecretKey, eots::EotsSeed)>,
    /// The EOTS signer with its sign record, for the same key
    pub eots_signer: Option<Arc<std::sync::Mutex<super::eots_signer::EotsSigner>>>,
}

/// The number of heights the EOTS nonce commitment of a `join-ue` covers by default.
pub const DEFAULT_EOTS_NONCE_COUNT: u32 = 100_000;

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
    let last_block_height = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last processed block from Db: {}", e);
        RpcError::DbError(e)
    })?;

    Ok(last_block_height)
}

//
// IPC
//

#[derive(Serialize, Deserialize)]
pub struct CreateSubnetResponse {
    pub subnet_id: SubnetId,
}

pub async fn create_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcCreateSubnetMsg>,
) -> Result<CreateSubnetResponse, JsonRpcError> {
    info!("createsubnet: {:?}", msg);

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

/// Request struct for the join_subnet RPC endpoint.
/// Separate from `IpcJoinSubnetMsg` because collateral is provided in the JSON request
/// but skipped in postcard serialization (it's derived from the transaction output).
#[derive(Debug, Serialize, Deserialize)]
pub struct JoinSubnetRequest {
    pub subnet_id: SubnetId,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub collateral: bitcoin::Amount,
    pub ip: std::net::SocketAddr,
    pub backup_address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
    pub pubkey: bitcoin::XOnlyPublicKey,
    /// With it, a `join-ue`: the collateral seats the validator for `epochs` epochs
    #[serde(default)]
    pub epochs: Option<u32>,
    /// The first subnet height the EOTS nonce commitment of a `join-ue` covers (default 1)
    #[serde(default)]
    pub eots_nonce_start: Option<u64>,
    /// The number of heights it covers (default `DEFAULT_EOTS_NONCE_COUNT`)
    #[serde(default)]
    pub eots_nonce_count: Option<u32>,
}

impl From<JoinSubnetRequest> for IpcJoinSubnetMsg {
    fn from(req: JoinSubnetRequest) -> Self {
        Self {
            subnet_id: req.subnet_id,
            collateral: req.collateral,
            ip: req.ip,
            backup_address: req.backup_address,
            pubkey: req.pubkey,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct JoinSubnetResponse {
    join_txid: bitcoin::Txid,
}

pub async fn join_subnet(
    data: Data<Arc<ServerData>>,
    Params(req): Params<JoinSubnetRequest>,
) -> Result<JoinSubnetResponse, JsonRpcError> {
    let epochs = req.epochs;
    let nonce_range = (
        req.eots_nonce_start.unwrap_or(1),
        req.eots_nonce_count.unwrap_or(DEFAULT_EOTS_NONCE_COUNT),
    );
    let msg: IpcJoinSubnetMsg = req.into();
    info!("joinsubnet: {:?} epochs={:?}", msg, epochs);

    // The activation switch is checked before any validation
    let tip = match epochs {
        Some(_) => {
            let tip = data
                .btc_rpc
                .get_block_count()
                .map_err(|e| RpcError::InternalError(e.to_string()))?;
            check_ue_active(collateral::ue_activation_height(), tip).map_err(|m| {
                error!("joinsubnet (join-ue) rejected: {m}");
                JsonRpcError::from(RpcError::InvalidParams(m))
            })?;
            Some(tip)
        }
        None => None,
    };

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

    let subnet_state = data.db.get_subnet_state(msg.subnet_id).map_err(|e| {
        error!("Error getting subnet info from Db: {}", e);
        RpcError::DbError(e)
    })?;

    msg.validate_for_subnet(&genesis_info, &subnet_state)
        .map_err(|e| {
            error!("Error validating join msg for subnet info: {}", e);
            RpcError::InvalidParams(e.to_string())
        })?;

    if let (Some(epochs), Some(tip)) = (epochs, tip) {
        let join_txid = join_subnet_ue(&data, &genesis_info, msg, epochs, tip, nonce_range)?;
        return Ok(JoinSubnetResponse { join_txid });
    }

    // TODO this check should be done in the Db
    let multisig_address = &genesis_info.multisig_address();

    let join_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(JoinSubnetResponse { join_txid })
}

/// The join-ue path of `joinsubnet`: the join signed by this provider's validator key, with
/// the timeout `T` that seats the validator for `epochs` epochs, the EOTS nonce commitment of
/// this provider's EOTS key for `nonce_range = (start, count)`, and the collateral naming the
/// current configuration. The caller has checked the activation switch at `tip`.
fn join_subnet_ue(
    data: &ServerData,
    genesis_info: &db::SubnetGenesisInfo,
    join: IpcJoinSubnetMsg,
    epochs: u32,
    tip: u64,
    nonce_range: (u64, u32),
) -> Result<bitcoin::Txid, JsonRpcError> {
    let invalid = |m: String| -> JsonRpcError {
        error!("joinsubnet (join-ue) rejected: {m}");
        RpcError::InvalidParams(m).into()
    };
    let Some((validator_xpk, validator_sk)) = data.validator else {
        return Err(RpcError::InternalError("No validator keypair configured.".to_string()).into());
    };
    let Some((eots_sk, eots_seed)) = data.eots else {
        return Err(RpcError::InternalError("No EOTS key configured (EOTS_SK_PATH).".to_string()).into());
    };
    if join.pubkey != validator_xpk {
        return Err(invalid(format!(
            "join-ue signs with this provider's validator key {validator_xpk}, not {}",
            join.pubkey
        )));
    }
    let params = &genesis_info.create_subnet_msg;
    if epochs < params.min_timeout || epochs > params.max_timeout {
        return Err(invalid(format!(
            "epochs must be between min_timeout {} and max_timeout {}, got {epochs}",
            params.min_timeout, params.max_timeout
        )));
    }

    collateral::refuse_earlier_join(&*data.db, join.subnet_id, &join.pubkey, None)
        .map_err(|e| invalid(e.to_string()))?;
    let context =
        collateral::join_context(&*data.db, join.subnet_id).map_err(|e| invalid(e.to_string()))?;

    // `T = H_0 + (e + epochs + 3) K`, with the latest final height in place of `H_0` before
    // activation; the monitor's rule 2.6 is checked at the next block before submission
    let origin = match context.h_0 {
        Some(h_0) => h_0,
        None => data
            .db
            .get_last_processed_block()
            .map_err(|e| RpcError::DbError(e))?,
    };
    let timeout =
        u32::try_from(params.cutoff_height(origin, context.epoch + u64::from(epochs) + 3))
            .ok()
            .filter(|t| *t < collateral::LOCKTIME_THRESHOLD)
            .ok_or_else(|| invalid("the timeout is not a block height".to_string()))?;
    context.check_timeout(timeout, tip + 1).map_err(invalid)?;

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (start, count) = nonce_range;
    if start < 1 || count < 1 {
        return Err(invalid(format!(
            "the EOTS nonce range needs start >= 1 and count >= 1, got start {start} count {count}"
        )));
    }
    let (commitment, _) = eots::commit_nonces(&eots_seed, &join.subnet_id, start, count);
    let msg = IpcJoinUeMsg::new_signed(&secp, join, timeout, &validator_sk, &eots_sk, commitment);
    msg.validate().map_err(|e| invalid(e.to_string()))?;
    collateral::refuse_earlier_eots_key(
        &*data.db,
        msg.join.subnet_id,
        &msg.join.pubkey,
        &msg.pk_eots,
        None,
    )
    .map_err(|e| invalid(e.to_string()))?;

    info!(
        "joinsubnet (join-ue): timeout {timeout} for {epochs} epochs from epoch {} (origin {origin}, tip {tip}), EOTS key {} nonces {start}..{}",
        context.epoch,
        msg.pk_eots,
        start + u64::from(count)
    );
    msg.submit_to_bitcoin(&data.btc_rpc, &context)
        .map_err(|e| JsonRpcError::internal(e.to_string()))
}

/// The activation switch: `join-ue` and `unstakeue` are refused while `UE_ACTIVATION_HEIGHT`
/// is unset or above the tip.
fn check_ue_active(activation: Option<u64>, tip: u64) -> Result<(), String> {
    match activation {
        Some(activation) if activation <= tip => Ok(()),
        Some(activation) => Err(format!(
            "unilateral exit is active from block {activation}, the tip is {tip}"
        )),
        None => Err(format!(
            "unilateral exit is off: {} is not set",
            collateral::UE_ACTIVATION_HEIGHT_VAR
        )),
    }
}

/// How a checkpoint is built from the monitor's state: who signs its fund inputs, which
/// unspent outputs they are, where the change goes and whether it rotates.
struct CheckpointPlan {
    signer: ipc_lib::CheckpointSigner,
    change_address: bitcoin::Address,
    unspent: Vec<bitcoincore_rpc::json::ListUnspentResultEntry>,
    rotation: bool,
    /// The change goes to another multisig: the checkpoint spends every fund UTXO
    sweep: bool,
}

impl CheckpointPlan {
    /// The plan from the monitor's epoch `e`, without the unspent outputs: the first checkpoint
    /// after the cutoff `H_{e-1}` is the rotation checkpoint of `e`, which pays `C_e`; any other
    /// checkpoint pays the signing configuration. The change number it carries plays no part.
    fn of_state(
        genesis_info: &db::SubnetGenesisInfo,
        subnet: &db::SubnetState,
    ) -> Result<Self, JsonRpcError> {
        let signer = signing_configuration(genesis_info, subnet)?;
        let rotation = subnet.last_rotation_epoch < subnet.epoch;
        let change_address = if rotation {
            subnet.latest_committee().address_checked()
        } else {
            signer.address.clone()
        };
        let sweep = change_address != signer.address;
        Ok(Self {
            signer,
            change_address,
            unspent: Vec::new(),
            rotation,
            sweep,
        })
    }
}

/// The configuration that signs a checkpoint's fund inputs: `C_wlist`, which holds the funds
/// until the rotation checkpoint of epoch 1 moves them to `C_1`, then the current one.
fn signing_configuration(
    genesis_info: &db::SubnetGenesisInfo,
    subnet: &db::SubnetState,
) -> Result<ipc_lib::CheckpointSigner, JsonRpcError> {
    if subnet.last_rotation_epoch == 0 {
        ipc_lib::CheckpointSigner::of_configuration(
            &subnet.id,
            &genesis_info.whitelist_configuration(),
        )
        .map_err(|e| RpcError::InternalError(e.to_string()).into())
    } else {
        Ok(ipc_lib::CheckpointSigner::of_committee(&subnet.committee))
    }
}

/// The configured validator must sign the checkpoint: a member of the signing configuration.
fn check_checkpoint_signer(
    genesis_info: &db::SubnetGenesisInfo,
    subnet: &db::SubnetState,
    validator: &bitcoin::XOnlyPublicKey,
) -> Result<(), JsonRpcError> {
    let signs = signing_configuration(genesis_info, subnet)?
        .keys
        .iter()
        .any(|(key, _)| key == validator);
    if !signs {
        error!("Configured validator isn't a validator in the specified subnet.");
        return Err(RpcError::InvalidParams(
            "Configured validator isn't a validator in the specified subnet.".to_string(),
        )
        .into());
    }
    Ok(())
}

/// Plans the checkpoint from the monitor's state, sets its rotation flag and takes the unspent
/// outputs of the signing configuration.
fn plan_checkpoint(
    data: &ServerData,
    genesis_info: &db::SubnetGenesisInfo,
    subnet: &db::SubnetState,
    msg: &mut IpcCheckpointSubnetMsg,
) -> Result<CheckpointPlan, JsonRpcError> {
    let mut plan = CheckpointPlan::of_state(genesis_info, subnet)?;
    msg.is_rotation_checkpoint = plan.rotation;
    plan.unspent = wallet::get_unspent_for_address(&data.btc_watchonly_rpc, &plan.signer.address)
        .map_err(|e| JsonRpcError::from(RpcError::InternalError(e.to_string())))?;
    Ok(plan)
}

/// The members of `committee` that joined with `join-ue`: the rotation and kill checkpoints
/// pay them nothing.
fn ue_members(
    db: &db::HeedDb,
    subnet_id: SubnetId,
    committee: &db::SubnetCommittee,
) -> Result<std::collections::HashSet<bitcoin::XOnlyPublicKey>, RpcError> {
    let mut members = std::collections::HashSet::new();
    for v in &committee.validators {
        if collateral::is_ue_validator(db, subnet_id, v.join_txid, &v.pubkey)? {
            members.insert(v.pubkey);
        }
    }
    Ok(members)
}

/// The kill checkpoint's payouts: the collateral of every member but the UE ones.
fn kill_unstakes(
    committee: &db::SubnetCommittee,
    ue_members: &std::collections::HashSet<bitcoin::XOnlyPublicKey>,
) -> Vec<ipc_lib::IpcUnstake> {
    committee
        .validators
        .iter()
        .filter(|v| !ue_members.contains(&v.pubkey))
        .map(|v| ipc_lib::IpcUnstake {
            amount: v.collateral,
            address: v.backup_address.clone(),
            pubkey: v.pubkey,
        })
        .collect()
}

/// The unstake outputs of a rotation: one per Withdraw stake change of a member of
/// `committee`, none for a UE member.
fn rotation_unstakes(
    stake_changes: &[db::StakeChangeRequest],
    committee: &db::SubnetCommittee,
    ue_members: &std::collections::HashSet<bitcoin::XOnlyPublicKey>,
) -> Vec<ipc_lib::IpcUnstake> {
    stake_changes
        .iter()
        .filter_map(|sc| {
            let StakingChange::Withdraw { amount } = sc.change else {
                return None;
            };
            let Some(v) = committee
                .validators
                .iter()
                .find(|v| v.pubkey == sc.validator_xpk)
            else {
                error!(
                    "Validator {} not found in committee for a stake change, skipping",
                    sc.validator_xpk
                );
                return None;
            };
            if ue_members.contains(&v.pubkey) {
                info!(
                    "Validator {} is a UE validator, no unstake output for its stake change",
                    sc.validator_xpk
                );
                return None;
            }
            Some(ipc_lib::IpcUnstake {
                amount,
                address: v.backup_address.clone(),
                pubkey: sc.validator_xpk,
            })
        })
        .collect()
}

/// The error of `stakecollateral` and `unstakecollateral`, which are refused for a UE validator.
fn refuse_ue_member(
    db: &db::HeedDb,
    subnet_id: SubnetId,
    committee: &db::SubnetCommittee,
    pubkey: &bitcoin::XOnlyPublicKey,
) -> Result<(), RpcError> {
    if collateral::is_ue_member(db, subnet_id, committee, pubkey)? {
        let m = format!(
            "Validator {pubkey} joined subnet {subnet_id} with join-ue; its collateral is \
             not in the multisig and is returned by unstakeue after it matures."
        );
        error!("{m}");
        return Err(RpcError::InvalidParams(m));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct GetGenesisInfoParams {
    pub subnet_id: SubnetId,
}

pub async fn get_genesis_info(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetGenesisInfoParams>,
) -> Result<db::SubnetGenesisInfo, JsonRpcError> {
    info!("getgenesisinfo: {}", params.subnet_id);

    let genesis_info = data
        .db
        .get_subnet_genesis_info(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    Ok(genesis_info)
}

#[derive(Serialize, Deserialize)]
pub struct GetSubnetParams {
    pub subnet_id: SubnetId,
}

pub async fn get_subnet(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetParams>,
) -> Result<db::SubnetState, JsonRpcError> {
    info!("getsubnet: {}", params.subnet_id);

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
pub struct GetSubnetBalanceResponse {
    /// Address of the subnet's current committee multisig. Useful for callers
    /// that want to top up (refund) the multisig.
    pub multisig_address: String,
    pub balance_sats: u64,
    pub utxo_count: usize,
}

/// Returns the on-chain balance of the subnet — i.e. the sats held by the
/// current committee multisig, summed across all unspent outputs the watch-only
/// wallet tracks for it. The monitor imports the multisig of each configuration
/// into the watch-only wallet when it records it, so no setup needed.
pub async fn get_subnet_balance(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetParams>,
) -> Result<GetSubnetBalanceResponse, JsonRpcError> {
    info!("getsubnetbalance: {}", params.subnet_id);

    let subnet_state = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet state from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            error!("Subnet {} not found.", params.subnet_id);
            RpcError::InvalidParams(format!("Subnet {} not found.", params.subnet_id))
        })?;

    let multisig_address = subnet_state.multisig_address();

    let unspent = wallet::get_unspent_for_address(&data.btc_watchonly_rpc, &multisig_address)
        .map_err(|e| {
            error!("Error listing unspent for multisig {}: {}", multisig_address, e);
            RpcError::InternalError(e.to_string())
        })?;

    let balance_sats: u64 = unspent.iter().map(|u| u.amount.to_sat()).sum();

    Ok(GetSubnetBalanceResponse {
        multisig_address: multisig_address.to_string(),
        balance_sats,
        utxo_count: unspent.len(),
    })
}

#[derive(Serialize, Deserialize)]
pub struct PrefundSubnetResponse {
    prefund_txid: bitcoin::Txid,
}

pub async fn prefund_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcPrefundSubnetMsg>,
) -> Result<PrefundSubnetResponse, JsonRpcError> {
    info!("prefundsubnet: {}", msg.subnet_id);

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
    info!("fundsubnet: {}", msg.subnet_id);

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

    // The current configuration: `C_wlist` in epoch 0, from a cutoff on the next configuration
    let multisig_address = if subnet_state.is_activated() {
        subnet_state.latest_committee().address_checked()
    } else {
        data.db
            .get_subnet_genesis_info(msg.subnet_id)
            .map_err(RpcError::DbError)?
            .ok_or(RpcError::InvalidParams(format!(
                "Subnet {} not found.",
                msg.subnet_id
            )))?
            .multisig_address()
    };

    let fund_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(FundSubnetResponse { fund_txid })
}

#[derive(Serialize, Deserialize)]
pub struct GetRootnetMessagesParams {
    pub subnet_id: SubnetId,
    pub block_height: u64,
}

pub async fn get_rootnet_messages(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetRootnetMessagesParams>,
) -> Result<Vec<db::RootnetMessage>, JsonRpcError> {
    info!(
        "getrootnetmessages: {} at {}",
        params.subnet_id, params.block_height
    );

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

#[derive(Serialize, Deserialize)]
pub struct GetTokenBalanceParams {
    pub home_subnet_id: SubnetId,
    pub home_token_address: alloy_primitives::Address,
    pub subnet_id: SubnetId,
}

#[derive(Serialize, Deserialize)]
pub struct GetTokenBalanceResponse {
    /// Balance as a decimal string (U256 may exceed u64).
    pub balance: String,
}

pub async fn get_token_balance(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetTokenBalanceParams>,
) -> Result<GetTokenBalanceResponse, JsonRpcError> {
    info!(
        "gettokenbalance: home_subnet={} token={} subnet={}",
        params.home_subnet_id, params.home_token_address, params.subnet_id
    );

    let balance = data
        .db
        .get_token_balance(
            params.home_subnet_id,
            params.home_token_address,
            params.subnet_id,
        )
        .map_err(|e| {
            error!("Error getting token balance from Db: {}", e);
            RpcError::DbError(e)
        })?;

    Ok(GetTokenBalanceResponse {
        balance: balance.to_string(),
    })
}

#[derive(Serialize, Deserialize)]
pub struct GetSubnetCheckpointParams {
    subnet_id: SubnetId,
    number: Option<u64>,
    /// With it, the latest checkpoint in a Bitcoin block at or below this height
    #[serde(default)]
    max_block_height: Option<u64>,
}

pub async fn get_subnet_checkpoint(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetCheckpointParams>,
) -> Result<Option<db::SubnetCheckpoint>, JsonRpcError> {
    info!(
        "getsubnetcheckpoint: {} number={:?} max_block_height={:?}",
        params.subnet_id, params.number, params.max_block_height
    );
    Ok(latest_checkpoint_at(
        &data.db,
        params.subnet_id,
        params.number,
        params.max_block_height,
    )?)
}

/// The checkpoint `number` of the subnet, the last one without it; with `max_block_height`,
/// the latest checkpoint in a Bitcoin block at or below that height. The answer for a
/// `max_block_height` depends on that height alone: a height the monitor has not processed
/// yet is refused with `RpcError::MonitorBehind`, never answered with an older checkpoint.
pub fn latest_checkpoint_at(
    db: &db::HeedDb,
    subnet_id: SubnetId,
    number: Option<u64>,
    max_block_height: Option<u64>,
) -> Result<Option<db::SubnetCheckpoint>, RpcError> {
    if let Some(max_block_height) = max_block_height {
        let processed = db.get_last_processed_block().map_err(|e| {
            error!("Error getting last processed block from Db: {}", e);
            RpcError::DbError(e)
        })?;
        if processed < max_block_height {
            warn!(
                "getsubnetcheckpoint: {} asks for Bitcoin height {max_block_height}, monitor at {processed}",
                subnet_id
            );
            return Err(RpcError::MonitorBehind {
                requested: max_block_height,
                processed,
            });
        }
    }

    // Check if subnet exists
    let subnet = db
        .get_subnet_state(subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            error!("Subnet {} not found.", subnet_id);
            RpcError::InvalidParams(format!("Subnet {} not found.", subnet_id))
        })?;

    let last_checkpoint_number = match subnet.last_checkpoint_number {
        Some(number) => number,
        None => {
            return Ok(None);
        }
    };

    // Default to the last checkpoint
    let number = number.unwrap_or(last_checkpoint_number);
    let get = |number: u64| {
        db.get_checkpoint(subnet_id, number).map_err(|e| {
            error!("Error getting checkpoint from Db: {}", e);
            RpcError::DbError(e)
        })
    };
    let Some(max_block_height) = max_block_height else {
        return get(number);
    };
    // Checkpoint numbers follow Bitcoin heights: walk back to the first one in range
    let mut number = number;
    loop {
        match get(number)? {
            Some(checkpoint) if checkpoint.block_height <= max_block_height => {
                return Ok(Some(checkpoint))
            }
            _ if number == 0 => return Ok(None),
            _ => number -= 1,
        }
    }
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
    info!("gen_multisig_spend_psbt: {:?}", params);

    let (_, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

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
    let committee_keys = subnet.committee.validator_weighted_keys();
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
        false,
        &[bitcoin::TxOut {
            value: params.amount,
            script_pubkey: recipient.script_pubkey(),
        }],
        &fee_rate,
        0,
    )
    .map_err(|e| {
        error!(
            "Error generating multisig spend psbt for subnet_id={}: {}",
            &params.subnet_id, e
        );
        RpcError::InternalError(e.to_string())
    })?;

    let validator_keypair = validator_sk.keypair(&secp);

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
    info!("gen_checkpoint_psbt: {:?}", msg);

    let (validator_xonly_pubkey, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    if let Err(err) = msg.validate() {
        error!("Invalid checkpoint message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    if msg.change_address.is_some() {
        return Err(
            RpcError::InvalidParams("Specifying change address not supported".to_string()).into(),
        );
    }
    // The subnet posts every pre-signature; the batch transfer carries the complete entries
    counted_slash_entries(&mut msg);
    msg.slash_signatures = carried_slash_entries(&*data.db, msg.subnet_id, &msg.slash_signatures);
    fit_slash_entries(&mut msg).map_err(|e| RpcError::InternalError(e.to_string()))?;

    if !msg.unstakes.len().is_zero() {
        return Err(
            RpcError::InvalidParams("Specifying unstakes not supported".to_string()).into(),
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

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(RpcError::DbError)?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;
    check_checkpoint_signer(&genesis_info, &subnet, &validator_xonly_pubkey)?;

    if subnet.is_killed() {
        error!(
            "Subnet {} is killed, cannot generate checkpoint.",
            msg.subnet_id
        );
        return Err(RpcError::InvalidParams(format!(
            "Subnet {} is killed, cannot generate checkpoint.",
            msg.subnet_id
        ))
        .into());
    }

    let should_kill_subnet = subnet.should_kill();

    // Fill in the subnet addresses, erroring out if any subnet is not found
    msg.update_subnets_for_transfer(&*data.db).map_err(|e| {
        error!("Error updating subnets for transfer: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);

    let plan = plan_checkpoint(&data, &genesis_info, &subnet, &mut msg)?;
    let unspent = &plan.unspent;
    let mut exhaust_unspent = plan.sweep;

    let ue_members = ue_members(&data.db, msg.subnet_id, &subnet.committee)?;

    if should_kill_subnet {
        // Mark the checkpoint as a kill checkpoint
        msg.is_kill_checkpoint = true;
        exhaust_unspent = true;

        // Calculation of total unspent and stake amounts, alongside with transfers and withdrawals to process

        let total_unspent_amount = unspent.iter().map(|u| u.amount).sum::<bitcoin::Amount>();
        let total_stake_amount = subnet
            .committee
            .validators
            .iter()
            .filter(|v| !ue_members.contains(&v.pubkey))
            .map(|v| v.collateral)
            .sum::<bitcoin::Amount>();

        let total_transfer_amount_to_process = msg
            .transfers
            .iter()
            .map(|t| t.amount)
            .sum::<bitcoin::Amount>();

        let total_withdrawal_amount_to_process = msg
            .withdrawals
            .iter()
            .map(|w| w.amount)
            .sum::<bitcoin::Amount>();

        let total_extra_amount = total_unspent_amount
            - total_stake_amount
            - total_withdrawal_amount_to_process
            - total_transfer_amount_to_process;

        info!(
            "Killing subnet {}, total unspent amount: {}, total stake amount: {}, extra value after withdrawals and transfers: {}",
            msg.subnet_id, total_unspent_amount, total_stake_amount, total_extra_amount
        );

        // Process all validators in the committee for unstaking
        msg.unstakes = kill_unstakes(&subnet.committee, &ue_members);

        info!(
            "Killing subnet {}, processing {} unstakes.",
            msg.subnet_id,
            msg.unstakes.len()
        );
    } else if plan.rotation && subnet.is_activated() {
        // The rotation checkpoint pays the multisig members that leave
        let unconfirmed_stake_changes = data
            .db
            .get_unconfirmed_stake_changes(msg.subnet_id)
            .map_err(|e| {
                error!("Error getting unconfirmed stake changes from Db: {}", e);
                RpcError::DbError(e)
            })?;
        msg.unstakes =
            rotation_unstakes(&unconfirmed_stake_changes, &subnet.committee, &ue_members);

        info!(
            "Rotation checkpoint of epoch {} to multisig address {}",
            subnet.epoch, plan.change_address
        );
        info!("Processing {} unstakes.", msg.unstakes.len());
    }

    validate_erc_batch_data(
        msg.subnet_id,
        &msg.token_registrations,
        &msg.token_supply_adjustments,
        &msg.token_transfers,
        &*data.db,
    )
    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;

    let unsigned_psbt = msg
        .build_checkpoint_psbt(
            &plan.signer,
            &plan.change_address,
            exhaust_unspent,
            fee_rate,
            unspent,
        )
        .map_err(|e| {
            error!(
                "Error generating checkpoint psbt for subnet_id={}: {}",
                &msg.subnet_id, e
            );

            RpcError::InternalError(e.to_string())
        })?;

    let checkpoint_txid = unsigned_psbt.unsigned_tx.compute_txid();

    let batch_transfer_tx = msg
        .make_reveal_batch_transfer_tx(checkpoint_txid, fee_rate, &plan.signer.address)
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

    let validator_keypair = validator_sk.keypair(&secp);

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

/// Identical to `gen_checkpoint_psbt` but skips the token-transfer balance
/// check.  Only available with the `dev` feature.  Used by the monitor tester
/// to inject malicious checkpoints that exceed subnet balances so we can verify
/// the monitor enforces the firewall on its side.
#[cfg(feature = "dev")]
pub async fn dev_gen_checkpoint_psbt(
    data: Data<Arc<ServerData>>,
    Params(mut msg): Params<IpcCheckpointSubnetMsg>,
) -> Result<GenCheckpointPsbtResponse, JsonRpcError> {
    info!("dev_gen_checkpoint_psbt (no balance check): {:?}", msg);

    let (validator_xonly_pubkey, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    if let Err(err) = msg.validate() {
        error!("Invalid checkpoint message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    if msg.change_address.is_some() {
        return Err(
            RpcError::InvalidParams("Specifying change address not supported".to_string()).into(),
        );
    }

    if !msg.unstakes.len().is_zero() {
        return Err(
            RpcError::InvalidParams("Specifying unstakes not supported".to_string()).into(),
        );
    }

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

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(RpcError::DbError)?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;
    check_checkpoint_signer(&genesis_info, &subnet, &validator_xonly_pubkey)?;

    if subnet.is_killed() {
        error!(
            "Subnet {} is killed, cannot generate checkpoint.",
            msg.subnet_id
        );
        return Err(RpcError::InvalidParams(format!(
            "Subnet {} is killed, cannot generate checkpoint.",
            msg.subnet_id
        ))
        .into());
    }

    msg.update_subnets_for_transfer(&*data.db).map_err(|e| {
        error!("Error updating subnets for transfer: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);

    let plan = plan_checkpoint(&data, &genesis_info, &subnet, &mut msg)?;

    // balance check intentionally omitted

    let unsigned_psbt = msg
        .build_checkpoint_psbt(
            &plan.signer,
            &plan.change_address,
            plan.sweep,
            fee_rate,
            &plan.unspent,
        )
        .map_err(|e| {
            error!(
                "Error generating checkpoint psbt for subnet_id={}: {}",
                &msg.subnet_id, e
            );
            RpcError::InternalError(e.to_string())
        })?;

    let checkpoint_txid = unsigned_psbt.unsigned_tx.compute_txid();

    let batch_transfer_tx = msg
        .make_reveal_batch_transfer_tx(checkpoint_txid, fee_rate, &plan.signer.address)
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

    let validator_keypair = validator_sk.keypair(&secp);

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
    pub unsigned_psbt_base64: String,
    pub secret_keys: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DevMultisignPsbtResponse {
    // Include signatures for each input, mapped by pubkey
    pub signatures: Vec<(
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    )>,
}

#[cfg(feature = "dev")]
pub async fn dev_multisign_psbt(
    _data: Data<Arc<ServerData>>,
    Params(params): Params<DevMultisignPsbtParams>,
) -> Result<DevMultisignPsbtResponse, JsonRpcError> {
    info!(
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
    info!("finalize_checkpoint_psbt for subnet {}", params.subnet_id);

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

    let genesis_info = data
        .db
        .get_subnet_genesis_info(params.subnet_id)
        .map_err(RpcError::DbError)?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;
    let signer = signing_configuration(&genesis_info, &subnet)?;
    let (committee_keys, committee_threshold) = (signer.keys, signer.threshold);

    let batch_transfer_tx: Option<bitcoin::Transaction> = match params.batch_transfer_tx_hex {
        Some(hex) => {
            if hex.is_empty() {
                None
            } else {
                let tx_bytes = hex::decode(hex).map_err(|e| {
                    error!("Invalid hex format for batch transfer tx: {}", e);
                    RpcError::InvalidParams(format!(
                        "Invalid hex format for batch transfer tx: {}",
                        e
                    ))
                })?;

                Some(bitcoin::consensus::deserialize(&tx_bytes).map_err(|e| {
                    error!("Invalid transaction format for batch transfer tx: {}", e);
                    RpcError::InvalidParams(format!(
                        "Invalid transaction format for batch transfer tx: {}",
                        e
                    ))
                })?)
            }
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

    // Create a map of provided signatures indexed by public key
    let signatures_map: std::collections::HashMap<
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    > = params.signatures.into_iter().collect();

    // Prepare signature sets in the same order as committee keys
    let mut signature_sets: Vec<&[bitcoin::secp256k1::schnorr::Signature]> =
        Vec::with_capacity(committee_keys.len());

    // Check if any unrecognized public keys were provided
    for pubkey in signatures_map.keys() {
        if !committee_keys
            .iter()
            .any(|(committee_key, _)| committee_key == pubkey)
        {
            error!("Unrecognized public key in signatures: {}", pubkey);
            return Err(RpcError::InvalidParams(format!(
                "Unrecognized public key in signatures: {}",
                pubkey
            ))
            .into());
        }
    }

    // For each committee key, get the corresponding signatures or use empty set
    for (pubkey, _) in &committee_keys {
        let sigs = match signatures_map.get(pubkey) {
            Some(sigs) => sigs.as_slice(),
            None => &[],
        };
        signature_sets.push(sigs);
    }

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
    if let Some(ref batch_transfer_txid) = batch_transfer_txid {
        trace!("batch_transfer_txid = {}", *batch_transfer_txid);
    }

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

// Stake collateral

#[derive(Serialize, Deserialize)]
pub struct StakeCollateralResponse {
    txid: bitcoin::Txid,
}

pub async fn stake_collateral(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcStakeCollateralMsg>,
) -> Result<StakeCollateralResponse, JsonRpcError> {
    info!(
        "stakecollateral: {} {} {}",
        msg.subnet_id, msg.pubkey, msg.amount
    );

    if let Err(err) = msg.validate() {
        error!("Invalid stake collateral message={msg:?}: {err}");
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

    refuse_ue_member(
        &data.db,
        msg.subnet_id,
        subnet_state.latest_committee(),
        &msg.pubkey,
    )?;
    msg.validate_for_subnet(&subnet_state).map_err(|e| {
        error!(
            "Error validating stake collateral msg for subnet info: {}",
            e
        );
        RpcError::InvalidParams(e.to_string())
    })?;

    let multisig_address = subnet_state.multisig_address();

    let txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(StakeCollateralResponse { txid })
}

// Unstake collateral

#[derive(Serialize, Deserialize)]
pub struct UnstakeCollateralResponse {
    txid: bitcoin::Txid,
}

pub async fn unstake_collateral(
    data: Data<Arc<ServerData>>,
    Params(mut msg): Params<IpcUnstakeCollateralMsg>,
) -> Result<UnstakeCollateralResponse, JsonRpcError> {
    info!("unstakecollateral: {} {}", msg.subnet_id, msg.amount);

    let (validator_xpk, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    msg.pubkey = Some(validator_xpk);

    if let Err(err) = msg.validate() {
        error!("Invalid unstake collateral message={msg:?}: {err}");
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

    refuse_ue_member(
        &data.db,
        msg.subnet_id,
        subnet_state.latest_committee(),
        &validator_xpk,
    )?;
    msg.validate_for_subnet(&genesis_info, &subnet_state)
        .map_err(|e| {
            error!(
                "Error validating unstake collateral msg for subnet info: {}",
                e
            );
            RpcError::InvalidParams(e.to_string())
        })?;

    let multisig_address = subnet_state.multisig_address();

    let txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address, validator_sk)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(UnstakeCollateralResponse { txid })
}

// Unstake UE collateral

#[derive(Serialize, Deserialize)]
pub struct UnstakeUeParams {
    pub subnet_id: SubnetId,
}

#[derive(Serialize, Deserialize)]
pub struct UnstakeUeResponse {
    txid: bitcoin::Txid,
    /// The collateral spent, before the fee
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    amount: bitcoin::Amount,
    /// The address paid
    address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
}

/// Spends this validator's matured join-ue collaterals in `subnet_id` to its backup address
/// in one transaction: those paying the backup address of the latest matured record; a
/// further call spends the rest, if any.
pub async fn unstake_ue(
    data: Data<Arc<ServerData>>,
    Params(params): Params<UnstakeUeParams>,
) -> Result<UnstakeUeResponse, JsonRpcError> {
    info!("unstakeue: {}", params.subnet_id);

    let Some((validator_xpk, validator_sk)) = data.validator else {
        error!("No validator keypair configured.");
        return Err(RpcError::InternalError("No validator keypair configured.".to_string()).into());
    };
    let invalid = |m: String| -> JsonRpcError {
        error!("unstakeue rejected: {m}");
        RpcError::InvalidParams(m).into()
    };
    let internal = |e: &dyn std::fmt::Display| -> JsonRpcError {
        error!("unstakeue failed: {e}");
        RpcError::InternalError(e.to_string()).into()
    };

    let records = data
        .db
        .get_ue_collaterals(params.subnet_id)
        .map_err(RpcError::DbError)?
        .into_iter()
        .filter(|r| r.pubkey == validator_xpk)
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Err(invalid(format!(
            "Validator {validator_xpk} has no join-ue collateral in subnet {}",
            params.subnet_id
        )));
    }

    let tip = data.btc_rpc.get_block_count().map_err(|e| internal(&e))?;
    check_ue_active(collateral::ue_activation_height(), tip).map_err(invalid)?;
    // The matured records paying the backup address of the latest matured one
    let Some(latest) = records
        .iter()
        .filter(|r| u64::from(r.timeout) <= tip)
        .max_by_key(|r| r.timeout)
        .cloned()
    else {
        let earliest = records.iter().map(|r| r.timeout).min().expect("non-empty");
        return Err(invalid(format!(
            "No matured join-ue collateral: the earliest matures at block {earliest}, the tip is {tip}"
        )));
    };
    let matured = records
        .iter()
        .filter(|r| u64::from(r.timeout) <= tip && r.backup_address == latest.backup_address)
        .collect::<Vec<_>>();

    let to = latest
        .backup_address
        .clone()
        .require_network(NETWORK)
        .map_err(|e| invalid(format!("backup address: {e}")))?;
    // Bitcoin Core relays transactions up to 400,000 weight units; each exit input weighs about
    // 305, so this many fit with room to spare and the rest are spent by the next call
    const MAX_EXIT_INPUTS: usize = 1000;
    if matured.len() > MAX_EXIT_INPUTS {
        info!(
            "unstakeue: {} matured outputs, spending {MAX_EXIT_INPUTS} now and the rest on the next call",
            matured.len()
        );
    }
    let inputs = matured
        .iter()
        .take(MAX_EXIT_INPUTS)
        .map(|r| {
            Ok(collateral::UeExitInput {
                outpoint: r.outpoint,
                value: r.value,
                script: collateral::collateral_script_of(&*data.db, r)?,
            })
        })
        .collect::<Result<Vec<_>, IpcLibError>>()
        .map_err(|e| internal(&e))?;
    let amount = inputs.iter().map(|i| i.value).sum::<bitcoin::Amount>();
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let keypair = validator_sk.keypair(&secp);
    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_rpc, None, None);
    let tx = collateral::build_ue_exit_tx(&secp, &inputs, &keypair, &to, fee_rate)
        .map_err(|e| internal(&e))?;
    let txid = data
        .btc_rpc
        .send_raw_transaction(&tx)
        .map_err(|e| internal(&e))?;
    info!(
        "unstakeue: spent {} collateral outputs ({amount}) of validator {validator_xpk} in subnet {} to {to}, txid={txid}",
        inputs.len(),
        params.subnet_id
    );

    Ok(UnstakeUeResponse {
        txid,
        amount,
        address: latest.backup_address,
    })
}

// Stake changes

#[derive(Serialize, Deserialize)]
pub struct GetStakeChangesParams {
    pub subnet_id: SubnetId,
    pub block_height: u64,
}

pub async fn get_stake_changes(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetStakeChangesParams>,
) -> Result<Vec<db::StakeChangeRequest>, JsonRpcError> {
    info!(
        "getstakechanges: {} at {}",
        params.subnet_id, params.block_height
    );

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
        .get_stake_changes_by_height(params.subnet_id, params.block_height)
        .map_err(|e| {
            error!("Error getting stake changes from Db: {}", e);
            RpcError::DbError(e).into()
        })
}

// Kill subnet

#[derive(Serialize, Deserialize)]
pub struct KillSubnetResponse {
    txid: bitcoin::Txid,
}

pub async fn kill_subnet(
    data: Data<Arc<ServerData>>,
    Params(mut msg): Params<IpcKillSubnetMsg>,
) -> Result<KillSubnetResponse, JsonRpcError> {
    info!("killsubnet: {}", msg.subnet_id);

    let (validator_xpk, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    msg.pubkey = Some(validator_xpk);

    if let Err(err) = msg.validate() {
        error!("Invalid kill subnet message={msg:?}: {err}");
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

    msg.validate_for_subnet(&genesis_info, &subnet_state)
        .map_err(|e| {
            error!("Error validating kill subnet msg for subnet info: {}", e);
            RpcError::InvalidParams(e.to_string())
        })?;

    let current_block_height = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last block height from Db: {}", e);
        RpcError::DbError(e)
    })?;

    let current_valid_requests = data
        .db
        .get_valid_kill_requests(msg.subnet_id, current_block_height)
        .map_err(|e| {
            error!("Error getting valid kill requests from Db: {}", e);
            RpcError::DbError(e)
        })?;

    // Check if the validator has already submitted a kill request
    if current_valid_requests
        .iter()
        .any(|kr| kr.validator_xpk == validator_xpk)
    {
        return Err(RpcError::InvalidParams(format!(
            "Validator {} has already submitted a kill request for subnet {}",
            validator_xpk, msg.subnet_id
        ))
        .into());
    }

    let multisig_address = subnet_state.multisig_address();

    let txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address, validator_sk)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(KillSubnetResponse { txid })
}

// Stake changes

#[derive(Serialize, Deserialize)]
pub struct GetKillRequestsParams {
    pub subnet_id: SubnetId,
}

pub async fn get_kill_requests(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetKillRequestsParams>,
) -> Result<Vec<db::KillRequest>, JsonRpcError> {
    info!("getkillrequests: {}", params.subnet_id);

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

    let current_block_height = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last block height from Db: {}", e);
        RpcError::DbError(e)
    })?;

    let valid_kill_requests = data
        .db
        .get_valid_kill_requests(params.subnet_id, current_block_height)
        .map_err(|e| {
            error!("Error getting valid kill requests from Db: {}", e);
            RpcError::DbError(e)
        })?;

    Ok(valid_kill_requests)
}

#[derive(Serialize, Deserialize)]
#[cfg(feature = "dev")]
pub struct DevKillSubnetParams {
    subnet_id: SubnetId,
    secret_keys: Vec<String>,
}

#[cfg(feature = "dev")]
pub async fn dev_kill_subnet(
    data: Data<Arc<ServerData>>,
    Params(params): Params<DevKillSubnetParams>,
) -> Result<Vec<KillSubnetResponse>, JsonRpcError> {
    use std::str::FromStr;

    info!(
        "dev_killsubnet: {} with {} secret keys",
        params.subnet_id,
        params.secret_keys.len()
    );

    let genesis_info = data
        .db
        .get_subnet_genesis_info(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    let subnet_state = data
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

    let current_block_height = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last block height from Db: {}", e);
        RpcError::DbError(e)
    })?;

    let valid_kill_requests = data
        .db
        .get_valid_kill_requests(params.subnet_id, current_block_height)
        .map_err(|e| {
            error!("Error getting valid kill requests from Db: {}", e);
            RpcError::DbError(e)
        })?;

    let multisig_address = subnet_state.multisig_address();
    let mut responses = Vec::new();

    for secret_key_str in params.secret_keys {
        // Parse the secret key
        let secret_key = bitcoin::secp256k1::SecretKey::from_str(&secret_key_str).map_err(|e| {
            error!("Invalid secret key: {}", e);
            RpcError::InvalidParams(format!("Invalid secret key: {}", e))
        })?;

        // Get the public key from the secret key
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (validator_xpk, _) = secret_key.x_only_public_key(&secp);

        if valid_kill_requests
            .iter()
            .any(|kr| kr.validator_xpk == validator_xpk)
        {
            continue; // Skip if this validator has already submitted a kill request
        }

        // Create the kill subnet message
        let msg = IpcKillSubnetMsg {
            subnet_id: params.subnet_id,
            pubkey: Some(validator_xpk),
        };

        // Validate the message
        if let Err(err) = msg.validate() {
            error!("Invalid kill subnet message={msg:?}: {err}");
            return Err(RpcError::InvalidParams(err.to_string()).into());
        }

        // Validate for the specific subnet
        if let Err(err) = msg.validate_for_subnet(&genesis_info, &subnet_state) {
            error!("Error validating kill subnet msg for subnet info: {}", err);
            return Err(RpcError::InvalidParams(err.to_string()).into());
        }

        // Submit to Bitcoin
        let txid = msg
            .submit_to_bitcoin(&data.btc_rpc, &multisig_address, secret_key)
            .map_err(|e| {
                error!("Error submitting kill subnet transaction: {}", e);
                JsonRpcError::internal(e.to_string())
            })?;

        info!("Kill subnet transaction submitted with txid: {}", txid);
        responses.push(KillSubnetResponse { txid });
    }

    Ok(responses)
}

#[cfg(feature = "emission_chain")]
pub async fn get_rewarded_collaterals(
    data: Data<Arc<ServerData>>,
    Params(params): Params<crate::rewards::GetRewardedCollateralsParams>,
) -> Result<crate::rewards::GetRewardedCollateralsResponse, JsonRpcError> {
    let reward_config = RewardConfig::new_from_env()
        .unwrap_or_else(|e| panic!("Failed to load reward config: {}", e));

    let (start_height, end_height) =
        reward_config
            .snapshot_boundaries(params.snapshot)
            .map_err(|e| {
                RpcError::InternalError(format!(
                "Error getting boundaries for snapshot {}. Is the server configured correctly?: {}",
                params.snapshot,
                e.to_string()
            ))
            })?;

    let last_processed_block = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last processed block from Db: {}", e);
        RpcError::DbError(e)
    })?;

    info!(
        "get_validator_rewards snapshot={} start_height={} end_height={}. Last proccessed Bitcoin block={}",
        params.snapshot, start_height, end_height, last_processed_block
    );

    let result = data
        .db
        .get_snapshot_result(params.snapshot)
        .map_err(|e| {
            error!("Error getting reward result from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            info!("No cached rewards found for snapshot {}. Last processed block: {last_processed_block}", params.snapshot);
            RpcError::InvalidParams(format!(
                "No cached rewards found for snapshot {}",
                params.snapshot
            ))
        })?;

    Ok(crate::rewards::GetRewardedCollateralsResponse {
        collaterals: result
            .rewards_list
            .into_iter()
            .map(|(pk, amt)| (crate::eth_utils::eth_addr_from_x_only_pubkey(pk), amt))
            .collect(),
        total_rewarded_collateral: result.total_rewarded_collateral,
    })
}

//
// EOTS and slashing
//

#[derive(Serialize, Deserialize, Debug)]
pub struct SubnetParams {
    pub subnet_id: SubnetId,
}

/// A validator's EOTS key
#[derive(Serialize, Deserialize, Debug)]
pub struct EotsKeyEntry {
    pub pubkey: bitcoin::XOnlyPublicKey,
    pub pk_eots: bitcoin::XOnlyPublicKey,
}

/// `geteotskeys`: the EOTS key of every validator that joined the subnet with a collateral
/// output.
pub async fn get_eots_keys(
    data: Data<Arc<ServerData>>,
    Params(params): Params<SubnetParams>,
) -> Result<Vec<EotsKeyEntry>, JsonRpcError> {
    let mut keys = data
        .db
        .get_ue_collaterals(params.subnet_id)
        .map_err(RpcError::DbError)?
        .into_iter()
        .map(|r| EotsKeyEntry {
            pubkey: r.pubkey,
            pk_eots: r.pk_eots,
        })
        .collect::<Vec<_>>();
    keys.sort_by_key(|k| k.pubkey);
    keys.dedup_by_key(|k| k.pubkey);
    Ok(keys)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GetEotsCommitmentsParams {
    pub subnet_id: SubnetId,
    pub pk_eots: bitcoin::XOnlyPublicKey,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EotsCommitmentEntry {
    pub start: u64,
    pub count: u32,
    pub root: String,
    /// The Bitcoin block that holds the transaction that carried it
    pub block_height: u64,
}

/// `geteotscommitments`: every EOTS nonce commitment the monitor recorded for an EOTS key.
pub async fn get_eots_commitments(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetEotsCommitmentsParams>,
) -> Result<Vec<EotsCommitmentEntry>, JsonRpcError> {
    Ok(data
        .db
        .get_eots_commitments(params.subnet_id, &params.pk_eots)
        .map_err(RpcError::DbError)?
        .into_iter()
        .map(|r| EotsCommitmentEntry {
            start: r.commitment.start,
            count: r.commitment.count,
            root: hex::encode(r.commitment.root),
            block_height: r.block_height,
        })
        .collect())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SignEotsVoteParams {
    pub subnet_id: SubnetId,
    pub height: u64,
    /// The subnet block hash, hex
    pub block_hash: String,
}

/// An EOTS vote with its proof, hex-encoded
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EotsVoteJson {
    pub pk_eots: String,
    pub height: u64,
    pub block_hash: String,
    pub signature: String,
    pub start: u64,
    pub proof: Vec<String>,
}

impl From<&eots::EotsVote> for EotsVoteJson {
    fn from(vote: &eots::EotsVote) -> Self {
        Self {
            pk_eots: hex::encode(vote.pk_eots),
            height: vote.height,
            block_hash: hex::encode(vote.block_hash),
            signature: hex::encode(eots::signature_to_bytes(&vote.signature)),
            start: vote.start,
            proof: vote.proof.iter().map(hex::encode).collect(),
        }
    }
}

fn hex32(what: &str, value: &str) -> Result<[u8; 32], JsonRpcError> {
    hex::decode(value)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| RpcError::InvalidParams(format!("{what} must be 32 bytes in hex")).into())
}

/// Refuses a new EOTS vote at a height the latest checkpoint has made final already, and
/// every vote after the kill checkpoint.
fn refuse_checkpointed_height(
    latest: Option<&db::SubnetCheckpoint>,
    height: u64,
) -> Result<(), String> {
    match latest {
        Some(checkpoint) if checkpoint.is_kill_checkpoint => Err(format!(
            "the kill checkpoint at subnet height {} ends finality",
            checkpoint.checkpoint_height
        )),
        Some(checkpoint) if height <= checkpoint.checkpoint_height => Err(format!(
            "height {height} is at or below the latest checkpointed height {}",
            checkpoint.checkpoint_height
        )),
        _ => Ok(()),
    }
}

/// The latest checkpoint the monitor recorded for a subnet, `None` before the first one.
fn latest_checkpoint<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
) -> Result<Option<db::SubnetCheckpoint>, db::DbError> {
    match db
        .get_subnet_state(subnet_id)?
        .and_then(|s| s.last_checkpoint_number)
    {
        Some(number) => db.get_checkpoint(subnet_id, number),
        None => Ok(None),
    }
}

/// `signeotsvote`: this validator's EOTS vote for a subnet block, signed with the nonce it
/// committed for that height, or the vote it already signed there. A second block at a signed
/// height is refused, and so is a new vote at or below the latest checkpointed height or
/// after the kill checkpoint.
pub async fn sign_eots_vote(
    data: Data<Arc<ServerData>>,
    Params(params): Params<SignEotsVoteParams>,
) -> Result<EotsVoteJson, JsonRpcError> {
    let Some(signer) = &data.eots_signer else {
        return Err(RpcError::InternalError("No EOTS key configured (EOTS_SK_PATH).".to_string()).into());
    };
    let block_hash = hex32("block_hash", &params.block_hash)?;
    let mut signer = signer.lock().expect("EOTS signer lock");
    let already_signed = signer
        .signed(params.subnet_id, params.height)
        .map_err(|e| RpcError::InternalError(e.to_string()))?;
    if already_signed.is_none() {
        let latest = latest_checkpoint(&*data.db, params.subnet_id).map_err(RpcError::DbError)?;
        refuse_checkpointed_height(latest.as_ref(), params.height).map_err(|e| {
            error!("signeotsvote refused at height {}: {e}", params.height);
            JsonRpcError::from(RpcError::InvalidParams(e))
        })?;
    }
    let commitments = data
        .db
        .get_eots_commitments(params.subnet_id, &signer.pk_eots())
        .map_err(RpcError::DbError)?
        .into_iter()
        .map(|r| r.commitment)
        .collect::<Vec<_>>();
    let vote = signer
        .sign(params.subnet_id, &commitments, params.height, &block_hash)
        .map_err(|e| {
            error!("signeotsvote refused at height {}: {e}", params.height);
            JsonRpcError::from(RpcError::InvalidParams(e.to_string()))
        })?;
    Ok(EotsVoteJson::from(&vote))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SignSlashParams {
    pub subnet_id: SubnetId,
    pub txid: bitcoin::Txid,
    pub vout: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SignSlashResponse {
    /// This validator's index in the configuration the output names
    pub index: u16,
    /// The pre-signature, hex
    pub signature: String,
}

/// `signslash`: this validator's pre-signature of the slash transaction of a collateral
/// output, as a member of the configuration the output's script names.
pub async fn sign_slash(
    data: Data<Arc<ServerData>>,
    Params(params): Params<SignSlashParams>,
) -> Result<SignSlashResponse, JsonRpcError> {
    let Some((_, validator_sk)) = data.validator else {
        return Err(RpcError::InternalError("No validator keypair configured.".to_string()).into());
    };
    let outpoint = bitcoin::OutPoint::new(params.txid, params.vout);
    let record = data
        .db
        .get_ue_collaterals(params.subnet_id)
        .map_err(RpcError::DbError)?
        .into_iter()
        .find(|r| r.outpoint == outpoint)
        .ok_or_else(|| {
            RpcError::InvalidParams(format!(
                "{outpoint} is not a collateral of subnet {}",
                params.subnet_id
            ))
        })?;
    let script = collateral::collateral_script_of(&*data.db, &record)
        .map_err(|e| RpcError::InternalError(e.to_string()))?;
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (index, signature) = crate::slash::sign_slash(
        &secp,
        &script,
        outpoint,
        record.value,
        &validator_sk.keypair(&secp),
    )
    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;
    Ok(SignSlashResponse {
        index,
        signature: hex::encode(eots::signature_to_bytes(&signature)),
    })
}

/// A collateral output that is not slashable yet, with the configuration its script names.
#[derive(Serialize, Deserialize, Debug)]
pub struct UnslashableOutput {
    pub txid: bitcoin::Txid,
    pub vout: u32,
    pub pubkey: bitcoin::XOnlyPublicKey,
    /// The join (reveal) transaction of the collateral
    pub join_txid: bitcoin::Txid,
    /// The Bitcoin block of the join
    pub block_height: u64,
    pub committee_number: u64,
    pub threshold: multisig::Power,
    /// The keys of the named configuration in ascending order, with their powers
    pub keys: Vec<(bitcoin::XOnlyPublicKey, multisig::Power)>,
    /// Whether this provider's validator key is one of them
    pub signer: bool,
}

/// `getunslashableoutputs`: the unspent collateral outputs of a subnet that the monitor has
/// not recorded as slashable, oldest first.
pub async fn get_unslashable_outputs(
    data: Data<Arc<ServerData>>,
    Params(params): Params<SubnetParams>,
) -> Result<Vec<UnslashableOutput>, JsonRpcError> {
    let validator = data.validator.map(|(k, _)| k);
    let mut outputs = Vec::new();
    let mut records = data
        .db
        .get_ue_collaterals(params.subnet_id)
        .map_err(RpcError::DbError)?;
    records.sort_by_key(|r| (r.timeout, r.outpoint));
    for record in records {
        if record.spent
            || data
                .db
                .get_slash_presigs(params.subnet_id, &record.outpoint)
                .map_err(RpcError::DbError)?
                .is_some()
        {
            continue;
        }
        let block_height =
            collateral::join_block_height(&*data.db, &record).map_err(RpcError::DbError)?;
        let configuration = collateral::configuration_by_number(
            &*data.db,
            params.subnet_id,
            record.committee_number,
        )
        .map_err(|e| RpcError::InternalError(e.to_string()))?
        .ok_or_else(|| {
            RpcError::InternalError(format!("No committee {}", record.committee_number))
        })?;
        let keys = configuration.sorted_keys();
        outputs.push(UnslashableOutput {
            txid: record.outpoint.txid,
            vout: record.outpoint.vout,
            pubkey: record.pubkey,
            join_txid: record.join_txid,
            block_height,
            committee_number: record.committee_number,
            threshold: configuration.threshold,
            signer: validator.is_some_and(|v| keys.iter().any(|(k, _)| *k == v)),
            keys,
        });
    }
    outputs.sort_by_key(|o| (o.block_height, o.txid, o.vout));
    Ok(outputs)
}

/// The slash pre-signature entries a checkpoint carries, from the ones posted on the subnet:
/// invalid signatures are dropped, and an entry is kept only when the valid ones reach the
/// threshold of the named configuration and the output is not slashable yet. Oldest output
/// first; the batch transfer data orders them by outpoint.
pub(crate) fn carried_slash_entries<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    posted: &[crate::slash::SlashSignatureEntry],
) -> Vec<crate::slash::SlashSignatureEntry> {
    let mut entries = Vec::new();
    for entry in posted {
        let checked = match ipc_lib::check_slash_entry(db, subnet_id, entry) {
            Ok(checked) => checked,
            Err(e) => {
                warn!("Slash pre-signatures of {} left out: {e}", entry.outpoint());
                continue;
            }
        };
        if !checked.invalid.is_empty() {
            warn!(
                "Slash pre-signatures of {} with invalid signatures {:?} left out",
                entry.outpoint(),
                checked.invalid
            );
        }
        if checked.slashable || !checked.reaches_threshold() {
            continue;
        }
        let join_height = match collateral::join_block_height(db, &checked.record) {
            Ok(height) => height,
            Err(e) => {
                warn!("Slash pre-signatures of {} left out: {e}", entry.outpoint());
                continue;
            }
        };
        entries.push((
            join_height,
            crate::slash::SlashSignatureEntry::new(entry.outpoint(), checked.valid),
        ));
    }
    entries.sort_by_key(|(height, e)| (*height, e.txid, e.vout));
    entries.into_iter().map(|(_, e)| e).collect()
}

/// Keeps the first `slash_pre_signature_count` entries of the checkpoint's slash pre-signature
/// list, those the subnet held at the end of the checkpoint height, so that every validator
/// builds the same reveal; all of them when the count is absent.
fn counted_slash_entries(msg: &mut IpcCheckpointSubnetMsg) {
    if let Some(count) = msg.slash_pre_signature_count {
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        msg.slash_signatures.truncate(count);
    }
}

/// Keeps the longest prefix of the checkpoint's slash pre-signature entries whose batch
/// transfer reveal is a standard transaction. The weight grows with the prefix, so a binary
/// search finds it.
fn fit_slash_entries(msg: &mut IpcCheckpointSubnetMsg) -> Result<(), IpcLibError> {
    let max = bitcoin::Weight::from_wu(u64::from(bitcoin::policy::MAX_STANDARD_TX_WEIGHT));
    let entries = std::mem::take(&mut msg.slash_signatures);
    let (mut fits, mut high) = (0, entries.len());
    while fits < high {
        let mid = (fits + high).div_ceil(2);
        msg.slash_signatures = entries[..mid].to_vec();
        if msg.batch_transfer_reveal_weight()? <= max {
            fits = mid;
        } else {
            high = mid - 1;
        }
    }
    msg.slash_signatures = entries[..fits].to_vec();
    Ok(())
}

pub fn make_rpc_server(server_data: Arc<ServerData>) -> RpcServer {
    let server = jsonrpc_v2::Server::new()
        .with_data(Data::new(server_data))
        // btc info
        .with_method("getblockhash", get_block_hash)
        .with_method("getblockcount", get_block_count)
        .with_method("getconfirmedcount", get_confirmed_block_height)
        // subnet
        .with_method("createsubnet", create_subnet)
        .with_method("joinsubnet", join_subnet)
        .with_method("getsubnet", get_subnet)
        .with_method("getsubnetbalance", get_subnet_balance)
        .with_method("getgenesisinfo", get_genesis_info)
        .with_method("prefundsubnet", prefund_subnet)
        .with_method("fundsubnet", fund_subnet)
        .with_method("killsubnet", kill_subnet)
        // rootnet messages
        .with_method("getrootnetmessages", get_rootnet_messages)
        // token balance
        .with_method("gettokenbalance", get_token_balance)
        // multisig
        .with_method("genmultisigspendpsbt", gen_multisig_spend_psbt)
        // checkpoints
        .with_method("gencheckpointpsbt", gen_checkpoint_psbt)
        .with_method("finalizecheckpointpsbt", finalize_checkpoint_psbt)
        .with_method("getsubnetcheckpoint", get_subnet_checkpoint)
        // stake changes
        .with_method("stakecollateral", stake_collateral)
        .with_method("unstakecollateral", unstake_collateral)
        .with_method("unstakeue", unstake_ue)
        .with_method("geteotskeys", get_eots_keys)
        .with_method("geteotscommitments", get_eots_commitments)
        .with_method("signeotsvote", sign_eots_vote)
        .with_method("signslash", sign_slash)
        .with_method("getunslashableoutputs", get_unslashable_outputs)
        .with_method("getstakechanges", get_stake_changes)
        // kill requests
        .with_method("getkillrequests", get_kill_requests);

    #[cfg(feature = "emission_chain")]
    let server = server.with_method("getrewardedcollaterals", get_rewarded_collaterals);

    #[cfg(feature = "dev")]
    // dev methods
    let server = server
        .with_method("dev_multisignpsbt", dev_multisign_psbt)
        .with_method("dev_killsubnet", dev_kill_subnet)
        .with_method("dev_gencheckpointpsbt", dev_gen_checkpoint_psbt);

    server.finish()
}

#[cfg(test)]
mod ue_tests {
    use super::*;

    #[test]
    fn latest_checkpoint_at_a_bitcoin_height_waits_for_the_monitor() {
        use crate::test_utils::{create_rand_txid, create_test_db, generate_subnet};
        let db = create_test_db();
        let mut subnet = generate_subnet(3);
        subnet.last_checkpoint_number = Some(1);
        let subnet_id = subnet.id;
        let checkpoint = |number: u64, block_height: u64| db::SubnetCheckpoint {
            checkpoint_number: number,
            checkpoint_hash: bitcoin::hashes::sha256::Hash::all_zeros(),
            checkpoint_height: 10 * (number + 1),
            block_height,
            txid: create_rand_txid(),
            batch_transfer_txid: None,
            batch_transfer_block_height: None,
            signed_committee_number: 0,
            next_committee_number: 0,
            next_configuration_number: 0,
            is_kill_checkpoint: false,
            epoch: 1,
            is_rotation_checkpoint: false,
        };
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_state(&mut wtxn, subnet_id, &subnet).unwrap();
        db.save_checkpoint(&mut wtxn, subnet_id, &checkpoint(0, 100), 0)
            .unwrap();
        db.save_checkpoint(&mut wtxn, subnet_id, &checkpoint(1, 120), 1)
            .unwrap();
        wtxn.commit().unwrap();
        db.set_last_processed_block(125).unwrap();

        let at = |height: Option<u64>| latest_checkpoint_at(&db, subnet_id, None, height);
        // Processed heights answer by the height alone
        assert_eq!(at(Some(110)).unwrap().unwrap().checkpoint_number, 0);
        assert_eq!(at(Some(120)).unwrap().unwrap().checkpoint_number, 1);
        assert_eq!(at(Some(125)).unwrap().unwrap().checkpoint_number, 1);
        assert!(at(Some(99)).unwrap().is_none());
        assert_eq!(at(None).unwrap().unwrap().checkpoint_number, 1);
        // A height the monitor has not processed is refused, not answered with checkpoint 1
        assert!(matches!(
            at(Some(126)),
            Err(RpcError::MonitorBehind {
                requested: 126,
                processed: 125
            })
        ));
        // Once the monitor is there, the same question has an answer
        db.set_last_processed_block(126).unwrap();
        assert_eq!(at(Some(126)).unwrap().unwrap().checkpoint_number, 1);
        // Without a Bitcoin height the monitor's lag does not matter
        db.set_last_processed_block(0).unwrap();
        assert_eq!(at(None).unwrap().unwrap().checkpoint_number, 1);
        assert_eq!(
            latest_checkpoint_at(&db, subnet_id, Some(0), None)
                .unwrap()
                .unwrap()
                .checkpoint_number,
            0
        );
    }

    #[test]
    fn carried_entries_keep_complete_valid_ones_only() {
        use crate::collateral::{self, CollateralScript};
        use crate::slash::{sign_slash, SlashSignatureEntry};
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let db = create_test_db();
        let subnet_id = generate_subnet_id();
        let keys = (1u8..9)
            .map(|seed| bitcoin::secp256k1::SecretKey::from_slice(&[seed; 32]).unwrap())
            .collect::<Vec<_>>();
        let pks = keys
            .iter()
            .map(|k| k.x_only_public_key(&secp).0)
            .collect::<Vec<_>>();
        // A subnet created with a whitelist of three keys, min 2: committee 0 has threshold 2
        let genesis = db::SubnetGenesisInfo {
            subnet_id,
            create_subnet_msg: IpcCreateSubnetMsg {
                min_validator_stake: bitcoin::Amount::from_sat(10_000),
                power_unit_exponent: 4,
                min_validators: 2,
                bottomup_check_period: 10,
                active_validators_limit: 10,
                min_cross_msg_fee: bitcoin::Amount::from_sat(1),
                whitelist: vec![pks[0], pks[1], pks[2]],
                epoch_length: 10,
                min_timeout: 1,
                max_timeout: 1000,
                min_deposit: bitcoin::Amount::from_sat(330),
                kill_vote_period: 36,
                kill_delay_period: 5,
            },
            create_msg_block_height: 1,
            bootstrapped: false,
            genesis_block_height: None,
            genesis_validators: vec![],
            genesis_balance_entries: vec![],
        };
        let record = UeCollateral {
            subnet_id,
            pubkey: pks[3],
            outpoint: bitcoin::OutPoint::new(create_rand_txid(), 1),
            value: bitcoin::Amount::from_sat(1_000_000),
            timeout: 1_000,
            pk_eots: pks[0],
            committee_number: 0,
            ip: "127.0.0.1:8080".parse().unwrap(),
            backup_address: crate::test_utils::create_rand_addr().into_unchecked(),
            join_txid: create_rand_txid(),
            spent: false,
        };
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_genesis_info(&mut wtxn, subnet_id, &genesis).unwrap();
        db.add_ue_collateral(&mut wtxn, &record).unwrap();
        wtxn.commit().unwrap();
        let script: CollateralScript = collateral::collateral_script_of(&db, &record).unwrap();
        let sign = |k: &bitcoin::secp256k1::SecretKey| {
            sign_slash(&secp, &script, record.outpoint, record.value, &k.keypair(&secp)).unwrap()
        };
        let (s0, s1) = (sign(&keys[0]), sign(&keys[1]));
        let mut bad = sign(&keys[2]);
        bad.1[1][0] ^= 1;
        // Two valid of three: carried with the valid ones only; one valid: left out
        let posted = vec![SlashSignatureEntry::new(record.outpoint, vec![s0, s1, bad])];
        let carried = carried_slash_entries(&db, subnet_id, &posted);
        assert_eq!(carried.len(), 1);
        assert_eq!(carried[0].signatures, vec![s0, s1]);
        let posted = vec![SlashSignatureEntry::new(record.outpoint, vec![s0, bad])];
        assert!(carried_slash_entries(&db, subnet_id, &posted).is_empty());
        // An unknown output is left out; a slashable one too
        let mut unknown = SlashSignatureEntry::new(record.outpoint, vec![s0, s1]);
        unknown.vout = 3;
        assert!(carried_slash_entries(&db, subnet_id, &[unknown]).is_empty());
        let mut wtxn = db.write_txn().unwrap();
        db.add_slash_presigs(
            &mut wtxn,
            &db::SlashPresigRecord {
                subnet_id,
                outpoint: record.outpoint,
                signatures: vec![s0, s1],
                block_height: 5,
                batch_transfer_txid: create_rand_txid(),
            },
        )
        .unwrap();
        wtxn.commit().unwrap();
        let posted = vec![SlashSignatureEntry::new(record.outpoint, vec![s0, s1])];
        assert!(carried_slash_entries(&db, subnet_id, &posted).is_empty());

        // A second collateral whose join is in an earlier block comes first, whatever its outpoint
        let older = UeCollateral {
            pubkey: pks[4],
            outpoint: bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([0xff; 32]), 1),
            pk_eots: pks[5],
            join_txid: create_rand_txid(),
            ..record.clone()
        };
        let newer = UeCollateral {
            pubkey: pks[6],
            outpoint: bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([0x00; 32]), 1),
            pk_eots: pks[7],
            join_txid: create_rand_txid(),
            ..record.clone()
        };
        let mut wtxn = db.write_txn().unwrap();
        for (collateral, block_height) in [(&older, 10), (&newer, 20)] {
            db.add_ue_collateral(&mut wtxn, collateral).unwrap();
            db.add_eots_commitment(
                &mut wtxn,
                &db::EotsCommitmentRecord {
                    subnet_id,
                    pk_eots: collateral.pk_eots,
                    commitment: crate::eots::EotsNonceCommitment {
                        start: 1,
                        count: 1,
                        root: [0; 32],
                    },
                    block_height,
                    txid: collateral.join_txid,
                },
            )
            .unwrap();
        }
        wtxn.commit().unwrap();
        let entry_of = |collateral: &UeCollateral| {
            let script = collateral::collateral_script_of(&db, collateral).unwrap();
            let signatures = keys[..2]
                .iter()
                .map(|k| {
                    sign_slash(
                        &secp,
                        &script,
                        collateral.outpoint,
                        collateral.value,
                        &k.keypair(&secp),
                    )
                    .unwrap()
                })
                .collect();
            SlashSignatureEntry::new(collateral.outpoint, signatures)
        };
        let posted = vec![entry_of(&newer), entry_of(&older)];
        let carried = carried_slash_entries(&db, subnet_id, &posted);
        assert_eq!(
            carried.iter().map(|e| e.outpoint()).collect::<Vec<_>>(),
            vec![older.outpoint, newer.outpoint]
        );
    }

    /// Synthetic entries of 350 signatures each, about 23 kB: 17 fit a standard reveal
    #[test]
    fn checkpoint_carries_the_longest_prefix_of_entries_that_fits() {
        let entry = |i: u8| crate::slash::SlashSignatureEntry {
            txid: [i; 32],
            vout: 1,
            signatures: (0..350u16)
                .map(|index| (index, [[index as u8; 32]; 2]))
                .collect(),
        };
        let mut msg = IpcCheckpointSubnetMsg {
            subnet_id: generate_subnet_id(),
            checkpoint_hash: bitcoin::hashes::sha256::Hash::from_byte_array([0; 32]),
            checkpoint_height: 1,
            next_committee_configuration_number: 0,
            withdrawals: vec![],
            transfers: vec![],
            token_registrations: vec![],
            token_supply_adjustments: vec![],
            token_transfers: vec![],
            slash_signatures: (0..20).map(entry).collect(),
            unstakes: vec![],
            change_address: None,
            is_kill_checkpoint: false,
            slash_pre_signature_count: None,
            is_rotation_checkpoint: false,
        };
        let max = bitcoin::Weight::from_wu(u64::from(bitcoin::policy::MAX_STANDARD_TX_WEIGHT));
        assert!(msg.batch_transfer_reveal_weight().unwrap() > max);
        fit_slash_entries(&mut msg).unwrap();
        assert_eq!(msg.slash_signatures.len(), 17);
        assert_eq!(msg.slash_signatures, (0..17).map(entry).collect::<Vec<_>>());
        assert!(msg.batch_transfer_reveal_weight().unwrap() <= max);
        msg.slash_signatures.push(entry(17));
        assert!(msg.batch_transfer_reveal_weight().unwrap() > max);
        // Entries that fit are all kept
        msg.slash_signatures = (0..5).map(entry).collect();
        fit_slash_entries(&mut msg).unwrap();
        assert_eq!(msg.slash_signatures.len(), 5);
    }

    #[test]
    fn eots_votes_are_refused_at_checkpointed_heights_and_after_the_kill_checkpoint() {
        let checkpoint = |height: u64, kill: bool| db::SubnetCheckpoint {
            checkpoint_number: 3,
            checkpoint_hash: bitcoin::hashes::sha256::Hash::from_byte_array([0; 32]),
            checkpoint_height: height,
            block_height: 100,
            txid: create_rand_txid(),
            batch_transfer_txid: None,
            batch_transfer_block_height: None,
            signed_committee_number: 1,
            next_committee_number: 1,
            next_configuration_number: 0,
            is_kill_checkpoint: kill,
            epoch: 1,
            is_rotation_checkpoint: false,
        };
        assert!(refuse_checkpointed_height(None, 1).is_ok());
        let latest = checkpoint(50, false);
        assert!(refuse_checkpointed_height(Some(&latest), 50).is_err());
        assert!(refuse_checkpointed_height(Some(&latest), 10).is_err());
        assert!(refuse_checkpointed_height(Some(&latest), 51).is_ok());
        let kill = checkpoint(50, true);
        assert!(refuse_checkpointed_height(Some(&kill), 51).is_err());
    }
    use crate::db::{SubnetValidator, SubnetValidators, UeCollateral};
    use crate::test_utils::{
        create_rand_eth_addr, create_rand_txid, create_test_db, generate_subnet_id,
        generate_xonly_pubkeys,
    };
    use std::str::FromStr;

    fn validator(pubkey: bitcoin::XOnlyPublicKey) -> SubnetValidator {
        SubnetValidator {
            pubkey,
            subnet_address: create_rand_eth_addr(),
            power: 10,
            collateral: bitcoin::Amount::from_sat(1_000_000),
            backup_address: bitcoin::Address::from_str(
                "bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd",
            )
            .unwrap(),
            ip: "127.0.0.1:8000".parse().unwrap(),
            join_txid: create_rand_txid(),
        }
    }

    /// `stakecollateral` and `unstakecollateral` are refused for a UE member, and only for it.
    #[test]
    fn refuses_ue_members_only() {
        let db = create_test_db();
        let subnet_id = generate_subnet_id();
        let keys = generate_xonly_pubkeys(3);
        let (ue, ordinary, outsider) = (keys[0], keys[1], keys[2]);
        let members = vec![validator(ue), validator(ordinary)];
        // A reveal carrying both joins: the ordinary member's join_txid has the UE record too
        let shared_txid = members[0].join_txid;
        let mut members = members;
        members[1].join_txid = shared_txid;
        let committee = members.to_committee(&subnet_id, 0);

        let record = UeCollateral {
            subnet_id,
            pubkey: ue,
            outpoint: bitcoin::OutPoint::new(create_rand_txid(), 1),
            value: bitcoin::Amount::from_sat(1_000_000),
            timeout: 1_000,
            pk_eots: generate_xonly_pubkeys(1)[0],
            committee_number: 0,
            ip: "127.0.0.1:8080".parse().unwrap(),
            backup_address: members[0].backup_address.clone(),
            join_txid: shared_txid,
            spent: false,
        };
        let mut txn = db.write_txn().unwrap();
        db.add_ue_collateral(&mut txn, &record).unwrap();
        txn.commit().unwrap();

        assert!(collateral::is_ue_member(&db, subnet_id, &committee, &ue).unwrap());
        assert!(!collateral::is_ue_member(&db, subnet_id, &committee, &ordinary).unwrap());
        assert!(!collateral::is_ue_member(&db, subnet_id, &committee, &outsider).unwrap());

        let err = refuse_ue_member(&db, subnet_id, &committee, &ue).unwrap_err();
        assert!(matches!(err, RpcError::InvalidParams(m) if m.contains("unstakeue")));
        refuse_ue_member(&db, subnet_id, &committee, &ordinary).unwrap();
        refuse_ue_member(&db, subnet_id, &committee, &outsider).unwrap();
    }

    /// A rotation pays only ordinary members' withdrawals; a kill pays only ordinary members.
    #[test]
    fn checkpoint_outputs_skip_ue_members() {
        use crate::db::{StakeChangeRequest, StakingChange};
        use crate::test_utils::{create_rand_blockhash, generate_subnet_id};
        let subnet_id = generate_subnet_id();
        let keys = generate_xonly_pubkeys(3);
        let (ue, ordinary, outsider) = (keys[0], keys[1], keys[2]);
        let committee = vec![validator(ue), validator(ordinary)].to_committee(&subnet_id, 0);
        let ue_members = std::collections::HashSet::from([ue]);
        let withdraw = |pubkey: bitcoin::XOnlyPublicKey, sats: u64| StakeChangeRequest {
            change: StakingChange::Withdraw {
                amount: bitcoin::Amount::from_sat(sats),
            },
            validator_xpk: pubkey,
            validator_subnet_address: create_rand_eth_addr(),
            configuration_number: 1,
            committee_after_change: committee.clone(),
            block_height: 100,
            block_hash: create_rand_blockhash(),
            checkpoint_block_height: None,
            checkpoint_block_hash: None,
            txid: create_rand_txid(),
        };
        let changes = vec![
            withdraw(ue, 1_000_000),
            withdraw(ordinary, 300_000),
            withdraw(outsider, 5),
            StakeChangeRequest {
                change: StakingChange::Deposit {
                    amount: bitcoin::Amount::from_sat(7),
                },
                ..withdraw(ordinary, 0)
            },
        ];

        let unstakes = rotation_unstakes(&changes, &committee, &ue_members);
        assert_eq!(unstakes.len(), 1);
        assert_eq!(unstakes[0].pubkey, ordinary);
        assert_eq!(unstakes[0].amount.to_sat(), 300_000);
        assert_eq!(unstakes[0].address, committee.validators[1].backup_address);

        let unstakes = kill_unstakes(&committee, &ue_members);
        assert_eq!(unstakes.len(), 1);
        assert_eq!(unstakes[0].pubkey, ordinary);
        assert_eq!(unstakes[0].amount.to_sat(), 1_000_000);

        // Without UE members both paths pay everyone, as before
        let none = std::collections::HashSet::new();
        assert_eq!(rotation_unstakes(&changes, &committee, &none).len(), 2);
        assert_eq!(kill_unstakes(&committee, &none).len(), 2);
    }

    /// The switch refuses while unset or above the tip.
    #[test]
    fn switch_gates_join_ue_and_unstakeue() {
        assert!(check_ue_active(None, 1_000)
            .unwrap_err()
            .contains("not set"));
        assert!(check_ue_active(Some(500), 499)
            .unwrap_err()
            .contains("active from block 500"));
        check_ue_active(Some(500), 500).unwrap();
        check_ue_active(Some(500), 501).unwrap();
    }

    /// `epochs` is optional in the request and absent in the join message.
    #[test]
    fn join_request_epochs_is_optional() {
        let json = serde_json::json!({
            "subnet_id": generate_subnet_id(),
            "collateral": 100_000,
            "ip": "127.0.0.1:8000",
            "backup_address": "bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd",
            "pubkey": generate_xonly_pubkeys(1)[0],
        });
        let req: JoinSubnetRequest = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(req.epochs, None);
        let mut with_epochs = json;
        with_epochs["epochs"] = serde_json::json!(20);
        let req: JoinSubnetRequest = serde_json::from_value(with_epochs).unwrap();
        assert_eq!(req.epochs, Some(20));
        let msg: IpcJoinSubnetMsg = req.into();
        assert_eq!(msg.collateral.to_sat(), 100_000);
    }
}

#[cfg(test)]
mod checkpoint_plan_tests {
    use super::*;
    use crate::test_utils::{generate_subnet, generate_xonly_pubkeys};

    /// The genesis information of `subnet_id`, created with `whitelist`, min 2
    fn genesis(
        subnet_id: SubnetId,
        whitelist: Vec<bitcoin::XOnlyPublicKey>,
    ) -> db::SubnetGenesisInfo {
        db::SubnetGenesisInfo {
            subnet_id,
            create_subnet_msg: IpcCreateSubnetMsg {
                min_validator_stake: bitcoin::Amount::from_sat(10_000),
                power_unit_exponent: 4,
                min_validators: 2,
                bottomup_check_period: 10,
                active_validators_limit: 10,
                min_cross_msg_fee: bitcoin::Amount::from_sat(1),
                whitelist,
                epoch_length: 10,
                min_timeout: 1,
                max_timeout: 1000,
                min_deposit: bitcoin::Amount::from_sat(330),
                kill_vote_period: 36,
                kill_delay_period: 5,
            },
            create_msg_block_height: 1,
            bootstrapped: true,
            genesis_block_height: Some(1),
            genesis_validators: vec![],
            genesis_balance_entries: vec![],
        }
    }

    fn plan(genesis: &db::SubnetGenesisInfo, subnet: &db::SubnetState) -> CheckpointPlan {
        CheckpointPlan::of_state(genesis, subnet).unwrap_or_else(|_| panic!("no checkpoint plan"))
    }

    #[test]
    fn an_unchanged_epoch_gets_a_rotation_checkpoint_that_sweeps_nothing() {
        let mut subnet = generate_subnet(3);
        let genesis = genesis(subnet.id, generate_xonly_pubkeys(3));
        // Epoch 2 with `C_2` = `C_1`, after the rotation checkpoint of epoch 1
        subnet.epoch = 2;
        subnet.last_rotation_epoch = 1;
        subnet.waiting_committee = None;
        let rotation = plan(&genesis, &subnet);
        assert!(rotation.rotation);
        assert!(!rotation.sweep);
        assert_eq!(rotation.signer.address, subnet.committee.address_checked());
        assert_eq!(rotation.change_address, rotation.signer.address);

        // After it, the checkpoints of the epoch are regular
        subnet.last_rotation_epoch = 2;
        let regular = plan(&genesis, &subnet);
        assert!(!regular.rotation);
        assert!(!regular.sweep);
    }

    #[test]
    fn epoch_1_with_c_1_equal_to_c_gen_sweeps_the_whitelist_multisig() {
        let mut subnet = generate_subnet(2);
        let mut whitelist: Vec<_> = subnet
            .committee
            .validators
            .iter()
            .map(|v| v.pubkey)
            .collect();
        whitelist.extend(generate_xonly_pubkeys(1));
        let genesis = genesis(subnet.id, whitelist);
        // Epoch 1 with `C_1` = `C_gen`, before its rotation checkpoint: `C_wlist` holds the funds
        subnet.epoch = 1;
        subnet.last_rotation_epoch = 0;
        subnet.waiting_committee = None;
        let wlist = ipc_lib::CheckpointSigner::of_configuration(
            &subnet.id,
            &genesis.whitelist_configuration(),
        )
        .unwrap();
        let rotation = plan(&genesis, &subnet);
        assert!(rotation.rotation);
        assert!(rotation.sweep);
        assert_eq!(rotation.signer.address, wlist.address);
        assert_eq!(rotation.change_address, subnet.committee.address_checked());
    }
}
