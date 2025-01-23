use bitcoin::{FeeRate, Network};

pub mod bitcoin_utils;
pub mod db;
pub mod eth_utils;
pub mod ipc_lib;
pub mod multisig;
pub mod provider;
pub mod wallet;

// Temporary re-exports
pub use ipc_lib::prelude::*;

/// Configures the bitcoin network to use
// TODO make this configurable
pub const NETWORK: Network = Network::Regtest;
/// Name of the L1 chain for each network
/// See https://github.com/bitcoin/bips/blob/master/bip-0122.mediawiki
pub const L1_NAME: &str = match NETWORK {
    Network::Bitcoin => "/bip122:000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    Network::Testnet => "/bip122:000000000933ea01ad0ee984209779baaec3ced90fa3f408719526f8d77f4943",
    Network::Testnet4 => "/bip122:00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043",
    Network::Signet => "/bip122:00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6",
    Network::Regtest => "/bip122:0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206",
    _ => panic!("Unsupported network"),
};

/// Number of blocks to wait for before considering a block confirmed
pub const BTC_CONFIRMATIONS: u64 = bitcoin_utils::confirmations(NETWORK);

pub const DEFAULT_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(10);
pub const MINIMUM_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(1);
pub const MAXIMUM_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(100);
