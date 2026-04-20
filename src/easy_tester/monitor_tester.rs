use std::{
    collections::HashMap,
    fs::File,
    net::TcpListener,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use bitcoin::hashes::sha256;
use log::info;
use tempfile::TempDir;

use crate::{
    easy_tester::{
        error::EasyTesterError,
        model::{
            build_create_subnet_msg, OutputDb, OutputExpectTarget,
            parse_u256_allow_underscores, SetupSpec, ValidatorSpec,
        },
        provider_client::ProviderClient,
        tester::Tester,
    },
    eth_utils::eth_addr_from_x_only_pubkey,
    ipc_lib::{
        IpcCheckpointSubnetMsg, IpcCrossSubnetErcTransfer, IpcErcSupplyAdjustment,
        IpcErcTokenRegistration,
    },
    provider::rpc::{DevMultisignPsbtParams, DevMultisignPsbtResponse},
    SubnetId,
};

// ── constants / defaults ──────────────────────────────────────────────────────

const RPC_USER: &str = "user";
const RPC_PASS: &str = "pass";
const BITCOIND_RPC_PORT: u16 = 18443; // default regtest port
/// Fixed directory where all log files are written (overwritten on each run).
const LOG_DIR: &str = "/tmp/easy_tester";
const WALLET_NAME: &str = "testwallet";
const AUTH_TOKEN: &str = "testtoken";
const BITCOIND_READY_TIMEOUT_SECS: u64 = 30;
const PROVIDER_READY_TIMEOUT_SECS: u64 = 30;
const MONITOR_POLL_INTERVAL: u64 = 1;
const CONFIRM_POLL_TIMEOUT_SECS: u64 = 60;

// ── helper: RAII guard that kills tracked PIDs on drop ───────────────────────
//
// Used during startup to ensure already-spawned processes are cleaned up if a
// later step fails.  Call `disarm()` once startup succeeds to transfer
// ownership to `MonitorTester` (whose own `Drop` handles the normal path).

struct ProcessGuard {
    pids: Vec<u32>,
}

impl ProcessGuard {
    fn new() -> Self {
        Self { pids: Vec::new() }
    }

    fn track(&mut self, pid: u32) {
        self.pids.push(pid);
    }

    /// Consume the guard without killing anything.  Returns the tracked PIDs.
    fn disarm(mut self) -> Vec<u32> {
        std::mem::take(&mut self.pids)
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        for &pid in &self.pids {
            kill_pid(pid);
        }
    }
}

// ── helper: find a free TCP port ──────────────────────────────────────────────

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("OS can bind to port 0")
        .local_addr()
        .expect("bound addr available")
        .port()
}

// ── helper: write a minimal bitcoin.conf ─────────────────────────────────────

fn write_bitcoin_conf(datadir: &std::path::Path) -> Result<(), EasyTesterError> {
    // Same format as the system bitcoin.conf that works with quickstart / demo.ipc.
    // No [regtest] section, no explicit rpcport — uses default 18443 for regtest.
    let content = format!(
        "regtest=1\n\
         server=1\n\
         rpcuser={RPC_USER}\n\
         rpcpassword={RPC_PASS}\n\
         rpcallowip=127.0.0.1\n\
         fallbackfee=0.00003\n\
         paytxfee=0.00003\n\
         listen=1\n\
         txindex=1\n"
    );
    std::fs::write(datadir.join("bitcoin.conf"), content)
        .map_err(|e| EasyTesterError::runtime(format!("failed to write bitcoin.conf: {e}")))?;
    Ok(())
}

// ── helper: run bitcoin-cli command ──────────────────────────────────────────

fn bitcoin_cli(datadir: &std::path::Path, args: &[&str]) -> Result<String, EasyTesterError> {
    let out = Command::new("bitcoin-cli")
        .arg(format!("-datadir={}", datadir.display()))
        .args(args)
        .output()
        .map_err(|e| EasyTesterError::runtime(format!("bitcoin-cli exec failed: {e}")))?;
    if !out.status.success() {
        return Err(EasyTesterError::runtime(format!(
            "bitcoin-cli {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ── helper: write a validator secret-key file ────────────────────────────────

fn write_validator_sk(
    dir: &std::path::Path,
    name: &str,
    sk: &bitcoin::secp256k1::SecretKey,
) -> Result<PathBuf, EasyTesterError> {
    let path = dir.join(format!("{name}.sk"));
    let hex = hex::encode(sk.secret_bytes());
    std::fs::write(&path, hex)
        .map_err(|e| EasyTesterError::runtime(format!("failed to write {name}.sk: {e}")))?;
    Ok(path)
}

// ── helper: write .env file ───────────────────────────────────────────────────

fn write_env(
    path: &std::path::Path,
    provider_port: u16,
    db_url: &str,
    sk_path: &str,
    activation_height: Option<u64>,
    snapshot_length: Option<u64>,
    log_level: Option<&str>,
) -> Result<(), EasyTesterError> {
    let rust_log = log_level.unwrap_or("info");
    let mut content = format!(
        "RPC_USER={RPC_USER}\n\
         RPC_PASS={RPC_PASS}\n\
         RPC_URL=http://127.0.0.1:{BITCOIND_RPC_PORT}\n\
         WALLET_NAME={WALLET_NAME}\n\
         VALIDATOR_SK_PATH={sk_path}\n\
         DATABASE_URL={db_url}\n\
         PROVIDER_PORT={provider_port}\n\
         PROVIDER_AUTH_TOKEN={AUTH_TOKEN}\n\
         MONITOR_POLL_INTERVAL={MONITOR_POLL_INTERVAL}\n\
         RUST_LOG={rust_log}\n"
    );
    if let Some(ah) = activation_height {
        content.push_str(&format!("ACTIVATION_HEIGHT={ah}\n"));
    }
    if let Some(sl) = snapshot_length {
        content.push_str(&format!("SNAPSHOT_LENGTH={sl}\n"));
    }
    std::fs::write(path, content)
        .map_err(|e| EasyTesterError::runtime(format!("failed to write .env: {e}")))?;
    Ok(())
}

// ── helper: compile monitor + provider ───────────────────────────────────────

fn compile_binaries(workspace_root: &std::path::Path) -> Result<(), EasyTesterError> {
    info!("Compiling monitor + provider (--release --features emission_chain,dev) ...");
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--features",
            "emission_chain,dev",
            "--bins",
        ])
        .current_dir(workspace_root)
        .status()
        .map_err(|e| EasyTesterError::runtime(format!("cargo build exec failed: {e}")))?;
    if !status.success() {
        return Err(EasyTesterError::runtime(
            "cargo build --release --features emission_chain,dev failed".to_string(),
        ));
    }
    Ok(())
}

// ── Local wire-format types ───────────────────────────────────────────────────
// The tester is an external client of the provider JSON-RPC API.  It depends
// only on field names and JSON types — not on the provider's internal Rust
// structs.  Serde ignores unknown fields by default, so slim response structs
// safely capture only the fields we actually use.
//
// Exceptions: DevMultisignPsbtParams / DevMultisignPsbtResponse are imported
// from provider::rpc because their `signatures` type must match the
// Finalize*Params structs exactly for serde roundtrip.

// ---- params (Serialize) ----

/// Generic single-subnet-id param: covers genbootstraphandover, getgenesisinfo,
/// getkillrequests, getsubnet (all expect `{ "subnet_id": "..." }`).
#[derive(serde::Serialize)]
struct SubnetIdParam {
    subnet_id: SubnetId,
}

/// Generic subnet + block_height param: covers getrootnetmessages, getstakechanges.
#[derive(serde::Serialize)]
struct SubnetBlockParam {
    subnet_id: SubnetId,
    block_height: u64,
}

#[derive(serde::Serialize)]
struct GetTokenBalanceLocalParams {
    home_subnet_id: SubnetId,
    home_token_address: alloy_primitives::Address,
    subnet_id: SubnetId,
}

#[derive(serde::Serialize)]
struct JoinSubnetLocalRequest {
    subnet_id: SubnetId,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    collateral: bitcoin::Amount,
    ip: std::net::SocketAddr,
    backup_address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
    pubkey: bitcoin::XOnlyPublicKey,
}

#[derive(serde::Serialize)]
struct FinalizeBootstrapHandoverLocalParams {
    subnet_id: SubnetId,
    unsigned_psbt_base64: String,
    signatures: Vec<(bitcoin::XOnlyPublicKey, Vec<bitcoin::secp256k1::schnorr::Signature>)>,
}

#[derive(serde::Serialize)]
struct FinalizeCheckpointLocalParams {
    subnet_id: SubnetId,
    unsigned_psbt_base64: String,
    signatures: Vec<(bitcoin::XOnlyPublicKey, Vec<bitcoin::secp256k1::schnorr::Signature>)>,
    batch_transfer_tx_hex: Option<String>,
}

#[derive(serde::Serialize)]
struct GetRewardedCollateralsLocalParams {
    snapshot: u64,
}

#[derive(serde::Serialize)]
struct StakeCollateralLocalParams {
    subnet_id: SubnetId,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    amount: bitcoin::Amount,
    pubkey: bitcoin::XOnlyPublicKey,
}

#[derive(serde::Serialize)]
struct UnstakeCollateralLocalParams {
    subnet_id: SubnetId,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    amount: bitcoin::Amount,
    pubkey: Option<bitcoin::XOnlyPublicKey>,
}

// ---- responses (Deserialize) ----

/// genbootstraphandover / dev_gencheckpointpsbt both include a full
/// `bitcoin::Psbt` object that does not roundtrip through JSON (Parity
/// serde mismatch).  We only need the base64 string, so we capture just that.
#[derive(serde::Deserialize)]
struct GenBootstrapHandoverResult {
    unsigned_psbt_base64: String,
}

#[derive(serde::Deserialize)]
struct GenCheckpointPsbtResult {
    unsigned_psbt_base64: String,
    batch_transfer_tx_hex: Option<String>,
}

#[derive(serde::Deserialize)]
struct CreateSubnetLocalResponse {
    subnet_id: SubnetId,
}

/// Only `bootstrapped` is needed to decide whether to run the handover.
#[derive(serde::Deserialize)]
struct GenesisInfoBootstrapped {
    bootstrapped: bool,
}

#[derive(serde::Deserialize)]
struct GetTokenBalanceLocalResponse {
    balance: String,
}

#[derive(serde::Deserialize)]
struct RewardedCollateralsLocalResult {
    collaterals: Vec<(alloy_primitives::Address, bitcoin::Amount)>,
    total_rewarded_collateral: bitcoin::Amount,
}

/// One entry from `getrootnetmessages`.  The `registration` and `msg` fields
/// are kept as raw JSON so we don't depend on internal nested types.
#[derive(serde::Deserialize)]
struct RootnetMsgValue {
    kind: String,
    nonce: u64,
    #[serde(default)]
    registration: Option<serde_json::Value>,
    #[serde(default)]
    msg: Option<serde_json::Value>,
}

// ── MonitorTester ─────────────────────────────────────────────────────────────

struct LastRootnetMsgs {
    msgs: Vec<RootnetMsgValue>,
}

struct LastRewardResults {
    snapshot: u64,
    rewards_by_validator: HashMap<String, u64>,
    total_sats: u64,
}

/// The MonitorTester spawns real bitcoind, monitor, and provider processes
/// and exercises the full integration stack.
///
/// One provider is started per validator declared in setup (up to 7).
/// Validator-specific operations (join, stake, unstake, fund) are routed
/// to the provider that holds that validator's key.  Read-only and dev
/// operations go to the first provider (validator1).
pub struct MonitorTester {
    setup: SetupSpec,
    /// Default provider URL (validator1's) for non-validator-specific calls.
    default_provider_url: String,
    /// Validator name → provider URL.
    validator_provider_urls: HashMap<String, String>,
    bitcoind_pid: u32,
    monitor_pid: u32,
    /// PIDs of all provider processes (one per validator).
    provider_pids: Vec<u32>,
    tmpdir: TempDir,
    current_block: u64,
    /// Subnet name → SubnetId
    created_subnets: HashMap<String, SubnetId>,
    /// Token name → (home_subnet_name, home_token_address)
    registered_tokens: HashMap<String, (String, alloy_primitives::Address)>,
    pending_registrations: HashMap<String, Vec<IpcErcTokenRegistration>>,
    pending_supply_adjustments: HashMap<String, Vec<IpcErcSupplyAdjustment>>,
    pending_erc_transfers: HashMap<String, Vec<IpcCrossSubnetErcTransfer>>,
    last_rootnet_msgs: Option<LastRootnetMsgs>,
    last_token_balance: Option<alloy_primitives::U256>,
    last_reward_results: Option<LastRewardResults>,
    checkpoint_heights: HashMap<String, u64>,
    all_sk_hex: HashMap<String, String>,
    /// Subnet names for which the bootstrap handover has already been broadcast.
    done_handovers: std::collections::HashSet<String>,
    /// Directory where log files are written.
    log_dir: PathBuf,
}

impl MonitorTester {
    pub async fn new(
        setup: SetupSpec,
        activation_height: Option<u64>,
        snapshot_length: Option<u64>,
        monitor_log_level: Option<String>,
        provider_log_level: Option<String>,
    ) -> Result<Self, EasyTesterError> {
        // All blocking setup runs in block_in_place so the tokio executor stays free.
        struct Started {
            tmpdir: TempDir,
            bitcoind_pid: u32,
            monitor_pid: u32,
            /// One (validator_name, pid, port) per validator.
            providers: Vec<(String, u32, u16)>,
            all_sk_hex: HashMap<String, String>,
        }

        // Set FVM network to match NETWORK (Regtest → Testnet prefix for addresses).
        crate::eth_utils::set_fvm_network();

        // Prepare log directory and files (each run overwrites the previous ones).
        let log_dir = PathBuf::from(LOG_DIR);
        std::fs::create_dir_all(&log_dir)
            .map_err(|e| EasyTesterError::runtime(format!("failed to create log dir: {e}")))?;

        let open_log = |name: &str| -> Result<File, EasyTesterError> {
            File::create(log_dir.join(name))
                .map_err(|e| EasyTesterError::runtime(format!("failed to create {name}: {e}")))
        };

        // Truncate rpc_client.log so each run starts fresh (it is written in append mode per-call).
        open_log("rpc_client.log")?;

        let started = tokio::task::block_in_place(|| -> Result<Started, EasyTesterError> {
            // Guard kills all tracked child processes if we return early with an error.
            let mut guard = ProcessGuard::new();

            // 1. Verify required binaries
            which("bitcoind")?;
            which("bitcoin-cli")?;

            // 2. Compile monitor + provider
            let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            compile_binaries(&workspace_root)?;
            let release_dir = workspace_root.join("target").join("release");
            let monitor_bin = release_dir.join("monitor");
            let provider_bin = release_dir.join("provider");

            // 3. Temp dir
            let tmpdir = tempfile::tempdir()
                .map_err(|e| EasyTesterError::runtime(format!("tempdir failed: {e}")))?;
            let datadir = tmpdir.path().to_path_buf();

            // 4. Write bitcoin.conf
            write_bitcoin_conf(&datadir)?;

            // 4b. Stop any running bitcoind before starting ours
            stop_running_bitcoind();

            // 5. Start bitcoind (raise FD soft limit; bitcoind v28+ needs ≥ ~8 k FDs)
            let bitcoind_log = open_log("bitcoind.log")?;
            let bitcoind_cmd = format!(
                "ulimit -n 65536 2>/dev/null; exec bitcoind -datadir={} -daemon=0",
                datadir.display()
            );
            let bitcoind_log_stderr = bitcoind_log.try_clone()
                .map_err(|e| EasyTesterError::runtime(format!("failed to clone bitcoind log fd: {e}")))?;
            let bitcoind = Command::new("sh")
                .args(["-c", &bitcoind_cmd])
                .stdout(Stdio::from(bitcoind_log))
                .stderr(Stdio::from(bitcoind_log_stderr))
                .spawn()
                .map_err(|e| EasyTesterError::runtime(format!("failed to spawn bitcoind: {e}")))?;
            let bitcoind_pid = bitcoind.id();
            std::mem::forget(bitcoind);
            guard.track(bitcoind_pid);

            // 6. Wait for bitcoind, then mine 101 blocks
            wait_for_bitcoind(&datadir, BITCOIND_READY_TIMEOUT_SECS)?;
            bitcoin_cli(&datadir, &["createwallet", WALLET_NAME])?;
            let mine_address = bitcoin_cli(
                &datadir,
                &[&format!("-rpcwallet={WALLET_NAME}"), "getnewaddress"],
            )?;
            bitcoin_cli(
                &datadir,
                &[
                    &format!("-rpcwallet={WALLET_NAME}"),
                    "generatetoaddress",
                    "101",
                    &mine_address,
                ],
            )?;

            // 7. Write validator SK files
            let mut all_sk_hex = HashMap::new();
            for (name, v) in &setup.validators {
                write_validator_sk(&datadir, name, &v.secret_key)?;
                all_sk_hex.insert(name.clone(), hex::encode(v.secret_key.secret_bytes()));
            }
            let primary_validator = setup
                .validators
                .keys()
                .min()
                .cloned()
                .unwrap_or_else(|| "validator1".to_string());
            let primary_sk_path = datadir.join(format!("{primary_validator}.sk"));

            // 8. Create shared DB dir + monitor .env
            let db_url = datadir.join("db");
            std::fs::create_dir_all(&db_url)
                .map_err(|e| EasyTesterError::runtime(format!("failed to create db dir: {e}")))?;
            let db_url_str = db_url.to_str().unwrap_or("/tmp/db");

            // Monitor uses the primary validator SK (only needed for key-based
            // identity, not for signing transactions).
            let monitor_port = free_port(); // not actually contacted, but .env requires it
            let monitor_env_path = datadir.join("monitor.env");
            write_env(
                &monitor_env_path,
                monitor_port,
                db_url_str,
                primary_sk_path.to_str().unwrap_or("/tmp/validator1.sk"),
                activation_height,
                snapshot_length,
                monitor_log_level.as_deref(),
            )?;

            // 9. Start monitor
            let monitor_log = open_log("monitor.log")?;
            let monitor = Command::new(&monitor_bin)
                .args(["--env", monitor_env_path.to_str().unwrap()])
                .stdout(Stdio::null())
                .stderr(Stdio::from(monitor_log))
                .spawn()
                .map_err(|e| EasyTesterError::runtime(format!("failed to spawn monitor: {e}")))?;
            let monitor_pid = monitor.id();
            std::mem::forget(monitor);
            guard.track(monitor_pid);

            // 9b. Wait for the monitor to create the LMDB database file before
            // starting providers (they open the DB in read-only mode).
            let mdb_file = db_url.join("data.mdb");
            let db_deadline = Instant::now() + Duration::from_secs(30);
            while !mdb_file.exists() {
                if Instant::now() > db_deadline {
                    return Err(EasyTesterError::runtime(
                        "timed out waiting for monitor to initialize DB".to_string(),
                    ));
                }
                thread::sleep(Duration::from_millis(200));
            }

            // 10. Start one provider per validator.
            // Sorted by name so validator1 is always first (= default provider).
            let mut sorted_validators: Vec<&String> = setup.validators.keys().collect();
            sorted_validators.sort();

            let mut providers: Vec<(String, u32, u16)> = Vec::new();
            for name in &sorted_validators {
                let port = free_port();
                let sk_path = datadir.join(format!("{name}.sk"));
                let env_path = datadir.join(format!("provider-{name}.env"));
                write_env(
                    &env_path,
                    port,
                    db_url_str,
                    sk_path.to_str().unwrap_or("/tmp/validator.sk"),
                    activation_height,
                    snapshot_length,
                    provider_log_level.as_deref(),
                )?;
                let prov_log = open_log(&format!("provider-{name}.log"))?;
                let child = Command::new(&provider_bin)
                    .args(["--env", env_path.to_str().unwrap()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::from(prov_log))
                    .spawn()
                    .map_err(|e| {
                        EasyTesterError::runtime(format!(
                            "failed to spawn provider for {name}: {e}"
                        ))
                    })?;
                let pid = child.id();
                std::mem::forget(child);
                guard.track(pid);
                providers.push(((*name).clone(), pid, port));
            }

            // 11. Poll until every provider responds to HTTP.
            for (name, _pid, port) in &providers {
                wait_for_provider_http(*port, AUTH_TOKEN, PROVIDER_READY_TIMEOUT_SECS)
                    .map_err(|e| {
                        EasyTesterError::runtime(format!(
                            "provider for {name} (port {port}) failed to start: {e}"
                        ))
                    })?;
            }

            // Success — disarm the guard so processes survive past this scope.
            guard.disarm();

            Ok(Started {
                tmpdir,
                bitcoind_pid,
                monitor_pid,
                providers,
                all_sk_hex,
            })
        })?;

        let mut validator_provider_urls = HashMap::new();
        let mut provider_pids = Vec::new();
        for (name, pid, port) in &started.providers {
            validator_provider_urls
                .insert(name.clone(), format!("http://127.0.0.1:{port}/api"));
            provider_pids.push(*pid);
        }
        let default_provider_url = validator_provider_urls
            .get(&started.providers[0].0)
            .cloned()
            .expect("at least one validator/provider must exist");

        let pids_str: Vec<String> = started
            .providers
            .iter()
            .map(|(n, pid, _)| format!("{n}={pid}"))
            .collect();
        eprintln!(
            "MonitorTester ready (bitcoind={} monitor={} providers=[{}]). Logs: {LOG_DIR}/",
            started.bitcoind_pid,
            started.monitor_pid,
            pids_str.join(", "),
        );

        Ok(Self {
            setup,
            default_provider_url,
            validator_provider_urls,
            bitcoind_pid: started.bitcoind_pid,
            monitor_pid: started.monitor_pid,
            provider_pids,
            tmpdir: started.tmpdir,
            current_block: 101,
            created_subnets: HashMap::new(),
            registered_tokens: HashMap::new(),
            pending_registrations: HashMap::new(),
            pending_supply_adjustments: HashMap::new(),
            pending_erc_transfers: HashMap::new(),
            last_rootnet_msgs: None,
            last_token_balance: None,
            last_reward_results: None,
            checkpoint_heights: HashMap::new(),
            all_sk_hex: started.all_sk_hex,
            done_handovers: std::collections::HashSet::new(),
            log_dir,
        })
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Returns the secret keys (hex) for validators in a subnet's whitelist.
    fn sk_hex_for_subnet(&self, subnet_name: &str) -> Vec<String> {
        self.setup
            .subnets
            .get(subnet_name)
            .map(|subnet| {
                subnet
                    .whitelist_names
                    .iter()
                    .filter_map(|name| self.all_sk_hex.get(name).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns the secret keys (hex) for validators in the current committee,
    /// ordered to match the committee's validator list.
    fn sk_hex_for_committee(&self, committee: &crate::db::SubnetCommittee) -> Vec<String> {
        committee
            .validators
            .iter()
            .filter_map(|cv| {
                self.setup
                    .validators
                    .values()
                    .find(|v| v.pubkey == cv.pubkey)
                    .and_then(|v| self.all_sk_hex.get(&v.name).cloned())
            })
            .collect()
    }

    /// Find the name of a setup validator that is in the given committee and
    /// has a running provider.  Used to route checkpoint RPCs to a provider
    /// whose configured validator is still a committee member.
    fn find_committee_validator_name(
        &self,
        committee: &crate::db::SubnetCommittee,
    ) -> Result<String, EasyTesterError> {
        for cv in &committee.validators {
            if let Some(v) = self.setup.validators.values().find(|v| v.pubkey == cv.pubkey) {
                if self.validator_provider_urls.contains_key(&v.name) {
                    return Ok(v.name.clone());
                }
            }
        }
        Err(EasyTesterError::runtime(
            "no setup validator with a provider found in the current committee".to_string(),
        ))
    }

    fn resolve_subnet_id(&self, subnet_name: &str) -> Result<SubnetId, EasyTesterError> {
        self.created_subnets
            .get(subnet_name)
            .copied()
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("subnet '{subnet_name}' not created yet"))
            })
    }

    fn resolve_validator(&self, validator_name: &str) -> Result<&ValidatorSpec, EasyTesterError> {
        self.setup.validators.get(validator_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("validator '{validator_name}' not in setup"))
        })
    }

    /// Synchronous JSON-RPC call to the default provider (validator1).
    fn rpc_call<Req, Resp>(&self, method: &str, params: Req) -> Result<Resp, EasyTesterError>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        self.rpc_call_to(&self.default_provider_url, method, params)
    }

    /// Synchronous JSON-RPC call routed to a specific validator's provider.
    fn rpc_call_as<Req, Resp>(
        &self,
        validator_name: &str,
        method: &str,
        params: Req,
    ) -> Result<Resp, EasyTesterError>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        let url = self.validator_provider_urls.get(validator_name).ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "no provider for validator '{validator_name}'"
            ))
        })?;
        self.rpc_call_to(url, method, params)
    }

    /// Low-level JSON-RPC call to a given provider URL.
    fn rpc_call_to<Req, Resp>(
        &self,
        url: &str,
        method: &str,
        params: Req,
    ) -> Result<Resp, EasyTesterError>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        // Check that at least one provider is still alive.
        let any_alive = self.provider_pids.iter().any(|pid| {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        });
        if !any_alive {
            return Err(EasyTesterError::runtime(
                "no provider processes are running".to_string(),
            ));
        }
        // Suppress logging for high-frequency polling calls to keep rpc_client.log readable.
        let silent = matches!(method, "getconfirmedcount");
        let log_file = if silent { None } else { Some(self.log_dir.join("rpc_client.log")) };
        ProviderClient::new(url.to_string(), AUTH_TOKEN.to_string(), log_file)
            .call(method, params)
    }

    /// Poll provider's `getconfirmedcount` until it reaches `target_height`.
    fn wait_for_confirmation(&self, target_height: u64) -> Result<(), EasyTesterError> {
        let deadline = Instant::now() + Duration::from_secs(CONFIRM_POLL_TIMEOUT_SECS);
        loop {
            let count: u64 = self
                .rpc_call("getconfirmedcount", serde_json::Value::Null)
                .unwrap_or(0);
            if count >= target_height {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(EasyTesterError::runtime(format!(
                    "timed out waiting for confirmed block height {target_height} (got {count})"
                )));
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    /// Mine Bitcoin blocks to reach `target_height`
    fn mine_to_height(&mut self, target_height: u64) -> Result<(), EasyTesterError> {
        if target_height <= self.current_block {
            return Ok(());
        }
        let blocks_to_mine = target_height - self.current_block;
        let datadir = self.tmpdir.path().to_path_buf();
        let address = bitcoin_cli(
            &datadir,
            &[&format!("-rpcwallet={WALLET_NAME}"), "getnewaddress"],
        )?;
        // give time to bitcoind to proccess transactions in the mempool
        thread::sleep(Duration::from_millis(100));
        bitcoin_cli(
            &datadir,
            &[
                &format!("-rpcwallet={WALLET_NAME}"),
                "generatetoaddress",
                &blocks_to_mine.to_string(),
                &address,
            ],
        )?;
        self.current_block = target_height;
        Ok(())
    }

    /// Build a checkpoint PSBT via `dev_gencheckpointpsbt` (no balance check),
    /// sign it with all validator keys, and finalize it.  This simulates a
    /// malicious or out-of-order checkpoint that bypasses the provider's
    /// balance firewall so we can verify the monitor handles it correctly.
    /// Performs the whitelist→committee multisig handover for a subnet that just bootstrapped.
    fn do_bootstrap_handover(
        &mut self,
        subnet_name: &str,
        subnet_id: SubnetId,
    ) -> Result<(), EasyTesterError> {
        eprintln!("[easy_tester] bootstrap handover for subnet '{subnet_name}'");

        let gen_resp: GenBootstrapHandoverResult =
            self.rpc_call("genbootstraphandover", SubnetIdParam { subnet_id })?;

        let sign_params = DevMultisignPsbtParams {
            unsigned_psbt_base64: gen_resp.unsigned_psbt_base64.clone(),
            secret_keys: self.sk_hex_for_subnet(subnet_name),
        };
        let sign_resp: DevMultisignPsbtResponse =
            self.rpc_call("dev_multisignpsbt", sign_params)?;

        let finalize_params = FinalizeBootstrapHandoverLocalParams {
            subnet_id,
            unsigned_psbt_base64: gen_resp.unsigned_psbt_base64,
            signatures: sign_resp.signatures,
        };
        let _: serde_json::Value = self.rpc_call("finalizebootstraphandover", finalize_params)?;

        info!("Bootstrap handover broadcast for subnet '{}'", subnet_name);
        Ok(())
    }

    /// After each block confirmation, check all known subnets for bootstrap and
    /// run the whitelist→committee handover if not yet done.
    fn do_handovers_if_needed(&mut self) -> Result<(), EasyTesterError> {
        let subnets: Vec<(String, SubnetId)> = self
            .created_subnets
            .iter()
            .map(|(name, id)| (name.clone(), *id))
            .collect();

        for (subnet_name, subnet_id) in subnets {
            if self.done_handovers.contains(&subnet_name) {
                continue;
            }
            let genesis_info: GenesisInfoBootstrapped =
                match self.rpc_call("getgenesisinfo", SubnetIdParam { subnet_id }) {
                    Ok(info) => info,
                    Err(_) => continue,
                };
            if !genesis_info.bootstrapped {
                continue;
            }
            self.do_bootstrap_handover(&subnet_name, subnet_id)?;
            self.done_handovers.insert(subnet_name);
        }
        Ok(())
    }

    fn do_checkpoint_malicious(
        &mut self,
        subnet_name: &str,
        subnet_id: SubnetId,
        token_registrations: Vec<IpcErcTokenRegistration>,
        supply_adjustments: Vec<IpcErcSupplyAdjustment>,
        erc_transfers: Vec<IpcCrossSubnetErcTransfer>,
    ) -> Result<(), EasyTesterError> {
        let checkpoint_height = self
            .checkpoint_heights
            .entry(subnet_name.to_string())
            .and_modify(|h| *h += 1)
            .or_insert(1);
        let checkpoint_height = *checkpoint_height;

        // Determine if there is a pending committee rotation.
        // If a waiting_committee exists, its configuration_number is the next one.
        let subnet_state: crate::db::SubnetState =
            self.rpc_call("getsubnet", SubnetIdParam { subnet_id })?;
        let current_cfg = subnet_state.committee.configuration_number;
        let next_cfg = subnet_state
            .waiting_committee
            .as_ref()
            .map(|wc| wc.configuration_number)
            .filter(|&wc_cfg| wc_cfg > current_cfg)
            .unwrap_or(0);

        // Route checkpoint RPCs to a provider whose validator is still in the
        // committee (the default provider's validator may have left).
        let committee_validator_name = self.find_committee_validator_name(&subnet_state.committee)?;

        let msg = IpcCheckpointSubnetMsg {
            subnet_id,
            checkpoint_hash: random_sha256(),
            checkpoint_height,
            next_committee_configuration_number: next_cfg,
            withdrawals: vec![],
            transfers: vec![],
            token_registrations,
            token_supply_adjustments: supply_adjustments,
            token_transfers: erc_transfers,
            unstakes: vec![],
            change_address: None,
            is_kill_checkpoint: false,
        };

        let gen_resp: GenCheckpointPsbtResult =
            self.rpc_call_as(&committee_validator_name, "dev_gencheckpointpsbt", msg)?;

        let sign_params = DevMultisignPsbtParams {
            unsigned_psbt_base64: gen_resp.unsigned_psbt_base64.clone(),
            secret_keys: self.sk_hex_for_committee(&subnet_state.committee),
        };
        let sign_resp: DevMultisignPsbtResponse =
            self.rpc_call_as(&committee_validator_name, "dev_multisignpsbt", sign_params)?;

        let finalize_params = FinalizeCheckpointLocalParams {
            subnet_id,
            unsigned_psbt_base64: gen_resp.unsigned_psbt_base64,
            signatures: sign_resp.signatures,
            batch_transfer_tx_hex: gen_resp.batch_transfer_tx_hex,
        };
        let _: serde_json::Value =
            self.rpc_call_as(&committee_validator_name, "finalizecheckpointpsbt", finalize_params)?;

        info!(
            "Checkpoint #{} for subnet '{}' finalized",
            checkpoint_height, subnet_name
        );
        Ok(())
    }
}

// ── wait helpers ──────────────────────────────────────────────────────────────

fn wait_for_bitcoind(datadir: &std::path::Path, timeout_secs: u64) -> Result<(), EasyTesterError> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let ok = Command::new("bitcoin-cli")
            .arg(format!("-datadir={}", datadir.display()))
            .arg("getblockchaininfo")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(EasyTesterError::runtime(
                "timed out waiting for bitcoind to start".to_string(),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Blocking check: poll with real HTTP until provider responds, or timeout.
fn wait_for_provider_http(
    port: u16,
    auth_token: &str,
    timeout_secs: u64,
) -> Result<(), EasyTesterError> {
    let url = format!("http://127.0.0.1:{port}/api");
    let body = r#"{"jsonrpc":"2.0","method":"getblockcount","params":[],"id":1}"#;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let result = Command::new("curl")
            .args([
                "-s",
                "-m",
                "2",
                "-X",
                "POST",
                "-H",
                &format!("Authorization: Bearer {auth_token}"),
                "-H",
                "Content-Type: application/json",
                "-d",
                body,
                &url,
            ])
            .output();
        if let Ok(out) = result {
            if out.status.success() && !out.stdout.is_empty() {
                eprintln!("[easy_tester] provider ready at {url}");
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(EasyTesterError::runtime(format!(
                "timed out waiting for provider at {url}"
            )));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Stop any currently running bitcoind gracefully, then force-kill if still alive.
fn stop_running_bitcoind() {
    let running = Command::new("pgrep")
        .args(["-x", "bitcoind"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !running {
        return;
    }

    eprintln!("[easy_tester] stopping existing bitcoind");
    // Graceful stop via bitcoin-cli (uses default system conf / --regtest)
    let _ = Command::new("bitcoin-cli")
        .args(["--regtest", "stop"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    // Wait up to 5 s for bitcoind to exit
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let still_running = Command::new("pgrep")
            .args(["-x", "bitcoind"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !still_running || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(300));
    }

    // Force-kill any remaining instance
    let _ = Command::new("pkill")
        .args(["-9", "-x", "bitcoind"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    thread::sleep(Duration::from_millis(500));
}

fn which(name: &str) -> Result<(), EasyTesterError> {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| EasyTesterError::runtime(format!("failed to run 'which {name}': {e}")))
        .and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err(EasyTesterError::runtime(format!(
                    "'{name}' not found in PATH — please install it"
                )))
            }
        })
}

fn random_sha256() -> sha256::Hash {
    use bitcoin::hashes::Hash;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    sha256::Hash::from_slice(&bytes).expect("random bytes make a sha256")
}

// ── Drop: clean up child processes ───────────────────────────────────────────

impl Drop for MonitorTester {
    fn drop(&mut self) {
        for &pid in &self.provider_pids {
            kill_pid(pid);
        }
        kill_pid(self.monitor_pid);
        // Ask bitcoind to stop gracefully, then force-kill
        let datadir = self.tmpdir.path().to_path_buf();
        let _ = Command::new("bitcoin-cli")
            .arg(format!("-datadir={}", datadir.display()))
            .args(["stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        thread::sleep(Duration::from_millis(500));
        kill_pid(self.bitcoind_pid);
        eprintln!("Logs written to: {LOG_DIR}");
    }
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// ── Tester impl ───────────────────────────────────────────────────────────────

impl Tester for MonitorTester {
    fn exec_mine_block(&mut self, height: u64) -> Result<(), EasyTesterError> {
        self.mine_to_height(height)?;
        self.wait_for_confirmation(height)?;
        self.do_handovers_if_needed()
    }

    fn exec_create_subnet(
        &mut self,
        _height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        let spec = self
            .setup
            .subnets
            .get(subnet_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("subnet '{subnet_name}' missing from setup"))
            })?
            .clone();
        let msg = build_create_subnet_msg(&spec);

        let resp: CreateSubnetLocalResponse = self.rpc_call("createsubnet", msg)?;

        self.created_subnets
            .insert(subnet_name.to_string(), resp.subnet_id);
        info!("Created subnet '{}' => {}", subnet_name, resp.subnet_id);
        Ok(())
    }

    fn exec_join_subnet(
        &mut self,
        _height: u64,
        subnet_name: &str,
        validator_name: &str,
        collateral_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let v = self.resolve_validator(validator_name)?.clone();

        let req = JoinSubnetLocalRequest {
            subnet_id,
            collateral: bitcoin::Amount::from_sat(collateral_sats),
            ip: v.default_ip,
            backup_address: v.default_backup_address.clone(),
            pubkey: v.pubkey,
        };

        let _: serde_json::Value = self.rpc_call_as(validator_name, "joinsubnet", req)?;

        info!(
            "Joined validator '{}' to subnet '{}'",
            validator_name, subnet_name
        );
        Ok(())
    }

    fn exec_deposit(
        &mut self,
        _height: u64,
        subnet_name: &str,
        address_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let v = self.resolve_validator(address_name)?.clone();
        let eth_address = eth_addr_from_x_only_pubkey(v.pubkey);

        let msg = crate::ipc_lib::IpcFundSubnetMsg {
            subnet_id,
            amount: bitcoin::Amount::from_sat(amount_sats),
            address: eth_address,
        };
        let _: serde_json::Value = self.rpc_call_as(address_name, "fundsubnet", msg)?;

        info!(
            "Deposited {} sats to '{}' on subnet '{}'",
            amount_sats, address_name, subnet_name
        );
        Ok(())
    }

    fn exec_stake_subnet(
        &mut self,
        _height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let v = self.resolve_validator(validator_name)?.clone();

        let params = StakeCollateralLocalParams {
            subnet_id,
            amount: bitcoin::Amount::from_sat(amount_sats),
            pubkey: v.pubkey,
        };

        let _: serde_json::Value = self.rpc_call_as(validator_name, "stakecollateral", params)?;

        info!(
            "Staked {} sats for '{}' on subnet '{}'",
            amount_sats, validator_name, subnet_name
        );
        Ok(())
    }

    fn exec_unstake_subnet(
        &mut self,
        _height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let v = self.resolve_validator(validator_name)?.clone();

        let params = UnstakeCollateralLocalParams {
            subnet_id,
            amount: bitcoin::Amount::from_sat(amount_sats),
            pubkey: Some(v.pubkey),
        };

        let _: serde_json::Value = self.rpc_call_as(validator_name, "unstakecollateral", params)?;

        info!(
            "Unstaked {} sats for '{}' from subnet '{}'",
            amount_sats, validator_name, subnet_name
        );
        Ok(())
    }

    fn exec_checkpoint_subnet(
        &mut self,
        _height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let registrations = self
            .pending_registrations
            .remove(subnet_name)
            .unwrap_or_default();
        let adjustments = self
            .pending_supply_adjustments
            .remove(subnet_name)
            .unwrap_or_default();
        let transfers = self
            .pending_erc_transfers
            .remove(subnet_name)
            .unwrap_or_default();

        self.do_checkpoint_malicious(
            subnet_name,
            subnet_id,
            registrations,
            adjustments,
            transfers,
        )
    }

    fn exec_register_token(
        &mut self,
        _height: u64,
        subnet_name: &str,
        name: &str,
        symbol: &str,
        initial_supply: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.resolve_subnet_id(subnet_name)?;

        let home_token_address = if let Some((prev_subnet, prev_addr)) =
            self.registered_tokens.get(name)
        {
            if prev_subnet != subnet_name {
                return Err(EasyTesterError::runtime(format!(
                        "token '{}' already registered on subnet '{}', this register does not allow reusing token names",
                        name, prev_subnet
                    )));
            }
            *prev_addr
        } else {
            alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>())
        };

        let etr = IpcErcTokenRegistration {
            home_token_address,
            name: name.to_string(),
            symbol: symbol.to_string(),
            decimals: 18,
            initial_supply,
        };

        self.registered_tokens.insert(
            name.to_string(),
            (subnet_name.to_string(), home_token_address),
        );
        self.pending_registrations
            .entry(subnet_name.to_string())
            .or_default()
            .push(etr);

        info!(
            "Queued token registration '{}' on subnet '{}'",
            name, subnet_name
        );
        Ok(())
    }

    fn exec_mint_token(
        &mut self,
        _height: u64,
        subnet_name: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.resolve_subnet_id(subnet_name)?;
        let (_, addr) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("token '{}' not registered", token_name))
        })?;
        let addr = *addr;

        let delta = alloy_primitives::I256::try_from(amount)
            .map_err(|e| EasyTesterError::runtime(format!("mint amount too large for I256: {e}")))?;

        self.pending_supply_adjustments
            .entry(subnet_name.to_string())
            .or_default()
            .push(IpcErcSupplyAdjustment {
                home_token_address: addr,
                delta,
            });
        Ok(())
    }

    fn exec_burn_token(
        &mut self,
        _height: u64,
        subnet_name: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.resolve_subnet_id(subnet_name)?;
        let (_, addr) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("token '{}' not registered", token_name))
        })?;
        let addr = *addr;

        let pos = alloy_primitives::I256::try_from(amount)
            .map_err(|e| EasyTesterError::runtime(format!("burn amount too large for I256: {e}")))?;
        let delta = pos.checked_neg()
            .ok_or_else(|| EasyTesterError::runtime("burn amount overflow (I256::MIN)".to_string()))?;

        self.pending_supply_adjustments
            .entry(subnet_name.to_string())
            .or_default()
            .push(IpcErcSupplyAdjustment {
                home_token_address: addr,
                delta,
            });
        Ok(())
    }

    fn exec_erc_transfer(
        &mut self,
        _height: u64,
        src_subnet: &str,
        dst_subnet: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        let dst_subnet_id = self.resolve_subnet_id(dst_subnet)?;
        let (home_subnet_name, home_token_address) =
            self.registered_tokens.get(token_name).ok_or_else(|| {
                EasyTesterError::runtime(format!("token '{}' not registered", token_name))
            })?;
        let home_subnet_name = home_subnet_name.clone();
        let home_token_address = *home_token_address;

        let home_subnet_id = self.resolve_subnet_id(&home_subnet_name)?;

        self.pending_erc_transfers
            .entry(src_subnet.to_string())
            .or_default()
            .push(IpcCrossSubnetErcTransfer {
                home_subnet_id,
                home_token_address,
                amount,
                destination_subnet_id: dst_subnet_id,
                recipient: alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>()),
            });
        Ok(())
    }

    fn exec_output_read(
        &mut self,
        _height: u64,
        db: OutputDb,
        args: &[String],
    ) -> Result<(), EasyTesterError> {
        self.last_rootnet_msgs = None;
        self.last_token_balance = None;
        self.last_reward_results = None;

        match db {
            OutputDb::RootnetMsgs => {
                let subnet_name = &args[0];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;

                // getrootnetmessages filters by exact block height, so query
                // every height from 1 to current_block to accumulate all messages.
                let mut msgs: Vec<RootnetMsgValue> = Vec::new();
                for h in 1..=self.current_block {
                    let batch: Vec<RootnetMsgValue> = self.rpc_call(
                        "getrootnetmessages",
                        SubnetBlockParam { subnet_id, block_height: h },
                    )?;
                    msgs.extend(batch);
                }

                println!(
                    "OUTPUT read rootnet_msgs subnet='{}': {} messages",
                    subnet_name,
                    msgs.len()
                );
                for (i, msg) in msgs.iter().enumerate() {
                    println!("  [{}] kind={}, nonce={}", i, msg.kind, msg.nonce);
                }

                self.last_rootnet_msgs = Some(LastRootnetMsgs { msgs });
            }

            OutputDb::TokenBalance => {
                let subnet_name = &args[0];
                let token_name = &args[1];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;

                let (home_subnet_name, home_token_address) = self
                    .registered_tokens
                    .get(token_name.as_str())
                    .ok_or_else(|| {
                        EasyTesterError::runtime(format!("token '{}' not registered", token_name))
                    })?;
                let home_subnet_name = home_subnet_name.clone();
                let home_token_address = *home_token_address;
                let home_subnet_id = self.resolve_subnet_id(&home_subnet_name)?;

                let params = GetTokenBalanceLocalParams {
                    home_subnet_id,
                    home_token_address,
                    subnet_id,
                };
                let resp: GetTokenBalanceLocalResponse =
                    self.rpc_call("gettokenbalance", params)?;

                let val = parse_u256_allow_underscores(&resp.balance).map_err(|e| {
                    EasyTesterError::runtime(format!("token balance parse error: {e}"))
                })?;
                println!(
                    "OUTPUT read token_balance subnet='{}' token='{}': {}",
                    subnet_name, token_name, val
                );
                self.last_token_balance = Some(val);
            }

            OutputDb::Subnet => {
                let subnet_name = &args[0];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                let resp: serde_json::Value =
                    self.rpc_call("getsubnet", SubnetIdParam { subnet_id })?;
                println!("OUTPUT read Subnet '{}' => {:#}", subnet_name, resp);
            }

            OutputDb::SubnetGenesis => {
                let subnet_name = &args[0];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                let resp: serde_json::Value =
                    self.rpc_call("getgenesisinfo", SubnetIdParam { subnet_id })?;
                println!("OUTPUT read SubnetGenesis '{}' => {:#}", subnet_name, resp);
            }

            OutputDb::StakeChanges => {
                let subnet_name = &args[0];
                let block_height = args[1]
                    .parse::<u64>()
                    .map_err(|e| EasyTesterError::runtime(format!("invalid block_height: {e}")))?;
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                let resp: serde_json::Value = self.rpc_call(
                    "getstakechanges",
                    SubnetBlockParam {
                        subnet_id,
                        block_height,
                    },
                )?;
                println!(
                    "OUTPUT read StakeChanges '{}' {} => {:#}",
                    subnet_name, block_height, resp
                );
            }

            OutputDb::KillRequests => {
                let subnet_name = &args[0];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                let resp: serde_json::Value =
                    self.rpc_call("getkillrequests", SubnetIdParam { subnet_id })?;
                println!("OUTPUT read KillRequests '{}' => {:#}", subnet_name, resp);
            }

            OutputDb::RewardResults => {
                let snapshot = args[0]
                    .parse::<u64>()
                    .map_err(|e| EasyTesterError::runtime(format!("invalid snapshot: {e}")))?;

                let resp: RewardedCollateralsLocalResult = self.rpc_call(
                    "getrewardedcollaterals",
                    GetRewardedCollateralsLocalParams { snapshot },
                )?;

                // Map ETH addresses back to validator names.
                let mut addr_to_name: HashMap<alloy_primitives::Address, String> = HashMap::new();
                for (name, v) in &self.setup.validators {
                    addr_to_name.insert(eth_addr_from_x_only_pubkey(v.pubkey), name.clone());
                }

                let total_sats = resp.total_rewarded_collateral.to_sat();
                let mut rewards_by_validator: HashMap<String, u64> = HashMap::new();

                println!("OUTPUT read RewardResults snapshot={}", snapshot);
                println!("rewards_list:");
                for (addr, amt) in &resp.collaterals {
                    let sats = amt.to_sat();
                    let label = addr_to_name
                        .get(addr)
                        .cloned()
                        .unwrap_or_else(|| format!("{addr}"));
                    *rewards_by_validator.entry(label.clone()).or_insert(0) += sats;
                    println!("  {} -> {} SAT", label, fmt_sats_with_underscores(sats));
                }
                println!(
                    "total_rewarded_collateral -> {} SAT",
                    fmt_sats_with_underscores(total_sats)
                );

                self.last_reward_results = Some(LastRewardResults {
                    snapshot,
                    rewards_by_validator,
                    total_sats,
                });
            }

            // No provider endpoints for these — direct DB access only.
            OutputDb::Committee | OutputDb::RewardCandidates => {
                return Err(EasyTesterError::runtime(format!(
                    "read {:?} has no provider RPC endpoint — use DbTester for this read",
                    db
                )));
            }
            OutputDb::TokenMetadata => {
                return Err(EasyTesterError::runtime(
                    "read token_metadata is not supported by MonitorTester",
                ));
            }
        }

        Ok(())
    }

    fn exec_output_expect(
        &mut self,
        _height: u64,
        target: OutputExpectTarget,
        expected_value: &str,
    ) -> Result<String, EasyTesterError> {
        // token_balance
        if let Some(balance) = self.last_token_balance {
            match target.path.as_str() {
                "balance" => {
                    let expected = parse_u256_allow_underscores(expected_value).map_err(|e| {
                        EasyTesterError::runtime(format!("balance must be numeric: {e}"))
                    })?;
                    if balance != expected {
                        return Err(EasyTesterError::runtime(format!(
                            "EXPECT failed (line {}): result.balance expected {}, got {}",
                            target.line_no, expected, balance
                        )));
                    }
                    return Ok(format!("result.balance == {}", expected));
                }
                other => {
                    return Err(EasyTesterError::runtime(format!(
                        "after 'read token_balance', only 'result.balance' is supported, got 'result.{}'",
                        other
                    )));
                }
            }
        }

        // reward_results
        if let Some(last) = self.last_reward_results.as_ref() {
            let expected_sats: u64 = expected_value.parse::<u64>().map_err(|e| {
                EasyTesterError::runtime(format!(
                    "expect rhs must be numeric for reward_results: {e}"
                ))
            })?;
            let parts: Vec<&str> = target.path.split('.').collect();
            match parts.as_slice() {
                ["rewards_list", key] | ["reward_list", key] => {
                    let got = last.rewards_by_validator.get(*key).copied().unwrap_or(0);
                    if got != expected_sats {
                        return Err(EasyTesterError::runtime(format!(
                            "EXPECT failed (line {}, snapshot {}): result.rewards_list.{} expected {} sats, got {} sats",
                            target.line_no, last.snapshot, key, expected_sats, got
                        )));
                    }
                    return Ok(format!(
                        "result.rewards_list.{} == {} SAT",
                        key,
                        fmt_sats_with_underscores(expected_sats)
                    ));
                }
                ["total_rewarded_collateral"] => {
                    let got = last.total_sats;
                    if got != expected_sats {
                        return Err(EasyTesterError::runtime(format!(
                            "EXPECT failed (line {}, snapshot {}): result.total_rewarded_collateral expected {} sats, got {} sats",
                            target.line_no, last.snapshot, expected_sats, got
                        )));
                    }
                    return Ok(format!(
                        "result.total_rewarded_collateral == {} SAT",
                        fmt_sats_with_underscores(expected_sats)
                    ));
                }
                _ => {
                    return Err(EasyTesterError::runtime(format!(
                        "unsupported expect path 'result.{}' after 'read reward_results'",
                        target.path
                    )));
                }
            }
        }

        // rootnet_msgs
        let last = self.last_rootnet_msgs.as_ref().ok_or_else(|| {
            EasyTesterError::runtime("expect used but no previous 'read' command")
        })?;

        let parts: Vec<&str> = target.path.split('.').collect();
        Ok(match parts.as_slice() {
            ["count"] => {
                let expected: u64 = expected_value
                    .parse::<u64>()
                    .map_err(|e| EasyTesterError::runtime(format!("count must be numeric: {e}")))?;
                let got = last.msgs.len() as u64;
                if got != expected {
                    return Err(EasyTesterError::runtime(format!(
                        "EXPECT failed (line {}): result.count expected {}, got {}",
                        target.line_no, expected, got
                    )));
                }
                format!("result.count == {}", expected)
            }
            [index_str, field] => {
                let index: usize = index_str.parse().map_err(|e| {
                    EasyTesterError::runtime(format!("invalid index '{}': {}", index_str, e))
                })?;
                let msg = last.msgs.get(index).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "result[{}] out of range (have {} messages)",
                        index,
                        last.msgs.len()
                    ))
                })?;
                let got = rootnet_msg_field(msg, field)?;
                let values_match = match (got.parse::<u64>(), expected_value.parse::<u64>()) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => got == expected_value,
                };
                if !values_match {
                    return Err(EasyTesterError::runtime(format!(
                        "EXPECT failed (line {}): result.{}.{} expected '{}', got '{}'",
                        target.line_no, index, field, expected_value, got
                    )));
                }
                format!("result.{}.{} == {}", index, field, got)
            }
            _ => {
                return Err(EasyTesterError::runtime(format!(
                    "unsupported expect path 'result.{}'",
                    target.path
                )));
            }
        })
    }
}

fn fmt_sats_with_underscores(sats: u64) -> String {
    let s = sats.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i != 0 && i % 3 == 0 {
            out.push('_');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn rootnet_msg_field(msg: &RootnetMsgValue, field: &str) -> Result<String, EasyTesterError> {
    match field {
        "kind" => Ok(msg.kind.clone()),
        "nonce" => Ok(msg.nonce.to_string()),
        "tokenName" => json_nested_field(msg.registration.as_ref(), "name", "tokenName"),
        "tokenSymbol" => json_nested_field(msg.registration.as_ref(), "symbol", "tokenSymbol"),
        "tokenDecimals" => json_nested_field(msg.registration.as_ref(), "decimals", "tokenDecimals"),
        "token" => json_nested_field(msg.registration.as_ref(), "home_token_address", "token"),
        "amount" => json_nested_field(msg.msg.as_ref(), "amount", "amount"),
        other => Err(EasyTesterError::runtime(format!("unknown rootnet_msg field '{other}'"))),
    }
}

/// Extract a named field from an optional nested JSON object and return it as a String.
fn json_nested_field(
    obj: Option<&serde_json::Value>,
    key: &str,
    display_field: &str,
) -> Result<String, EasyTesterError> {
    let val = obj
        .and_then(|v| v.get(key))
        .ok_or_else(|| {
            EasyTesterError::runtime(format!("field '{display_field}' not present in this message"))
        })?;
    Ok(match val {
        serde_json::Value::String(s) => {
            // U256 and similar types serialize as "0x..." hex strings; convert to decimal.
            if s.starts_with("0x") || s.starts_with("0X") {
                if let Ok(n) = s.parse::<alloy_primitives::U256>() {
                    return Ok(n.to_string());
                }
            }
            s.clone()
        }
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    })
}
