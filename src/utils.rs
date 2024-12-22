use dotenv::dotenv;
use serde::Deserialize;
use std::{env, fs::File, io::Read};
use thiserror::Error;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub listener_interval: u64,
    pub ipc_finalization_parameter: u64,
    pub checkpoint_interval: u64,
    pub postbox_interval: u64,
}

/// Load environment variables from a .env file
///
/// # Returns
///
/// A tuple containing the RPC user, RPC password, RPC URL, and wallet name
pub fn load_env() -> Result<(String, String, String, String), env::VarError> {
    dotenv().ok();
    let rpc_user = env::var("RPC_USER")?;
    let rpc_pass = env::var("RPC_PASS")?;
    let rpc_url = env::var("RPC_URL")?;
    let wallet_name = env::var("WALLET_NAME")?;

    Ok((rpc_user, rpc_pass, rpc_url, wallet_name))
}

/// Load the configuration from a JSON file
///
/// # Returns
///
pub fn load_config() -> Result<Config, LoadConfigError> {
    let mut file = File::open("config.json")?;
    let mut json = String::new();
    file.read_to_string(&mut json)?;

    let config: Config = serde_json::from_str(&json)?;

    Ok(config)
}

#[derive(Error, Debug)]
pub enum LoadConfigError {
    #[error("cannot open or read file")]
    IoError(#[from] std::io::Error),

    #[error("cannot deserialize file")]
    JsonError(#[from] serde_json::Error),
}
