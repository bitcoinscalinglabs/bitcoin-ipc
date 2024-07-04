use dotenv::dotenv;
use std::env;

pub fn load_env() -> (String, String, String, String) {
    dotenv().ok();
    let rpc_user = env::var("RPC_USER").expect("RPC_USER must be set");
    let rpc_pass = env::var("RPC_PASS").expect("RPC_PASS must be set");
    let rpc_url = env::var("RPC_URL").expect("RPC_URL must be set");
    let wallet_name = env::var("WALLET_NAME").expect("WALLET_NAME must be set");

    (rpc_user, rpc_pass, rpc_url, wallet_name)
}
