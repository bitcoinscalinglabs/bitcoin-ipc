pub mod bitcoin_utils;
pub mod ipc_lib;
pub mod ipc_subnet_state;
pub mod utils;

use bitcoin::Network;

pub const NETWORK: Network = Network::Regtest;
pub const IPC_CREATE_SUBNET_TAG: &str = "IPC:CREATE";
pub const IPC_JOIN_SUBNET_TAG: &str = "IPC:JOIN";
