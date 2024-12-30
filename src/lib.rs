pub mod bitcoin_utils;
pub mod db;
pub mod ipc_lib;
pub mod provider;

// Temporary re-exports
pub use ipc_lib::prelude::*;

/// Name of the L1 chain
pub const L1_NAME: &str = "BTC";

/// Configures the bitcoin network to use
// TODO make this configurable
pub const NETWORK: bitcoin::Network = bitcoin::Network::Regtest;

/// Number of blocks to wait for before considering a block confirmed
pub const BTC_CONFIRMATIONS: u64 = bitcoin_utils::confirmations(NETWORK);
