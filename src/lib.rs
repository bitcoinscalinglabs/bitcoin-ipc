pub mod bitcoin_utils;
pub mod ipc_lib;
pub mod provider;

/// Name of the L1 chain
pub const L1_NAME: &str = "BTC";

/// Configures the bitcoin network to use
// TODO make this configurable
pub const NETWORK: bitcoin::Network = bitcoin::Network::Regtest;

/// Number of blocks to wait for before considering a block confirmed
pub const BTC_CONFIRMATIONS: u64 = bitcoin_utils::confirmations(NETWORK);

pub const IPC_TAG_DELIMITER: &str = "#";
pub const IPC_CREATE_SUBNET_TAG: &str = "IPC:CREATE";
pub const IPC_JOIN_SUBNET_TAG: &str = "IPC:JOIN";
pub const IPC_DEPOSIT_TAG: &str = "IPC:DEPOSIT";
pub const IPC_CHECKPOINT_TAG: &str = "IPC:CHECKPOINT";
pub const IPC_TRANSFER_TAG: &str = "IPC:TRANSFER";
pub const IPC_WITHDRAW_TAG: &str = "IPC:WITHDRAW";
pub const IPC_DELETE_SUBNET_TAG: &str = "IPC:DELETE";
